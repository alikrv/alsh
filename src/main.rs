// alsh-rs - Custom shell with job control
mod builtin;
mod completion;
mod control_flow;
mod executor;
mod jobs;
mod parser;
mod signals;

use builtin::BuiltinRegistry;
use completion::ShellCompleter;
use control_flow::{ControlFlowParser, Statement, Value};
use executor::Executor;
use jobs::{JobManager, JobState};
use nix::sys::wait::waitpid;
use nix::unistd::{execvp, fork, pipe, ForkResult, geteuid};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{Config, Editor};
use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::ffi::CString;
use std::fs::{read_to_string, OpenOptions};
use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;
use std::rc::Rc;

fn setup_signals() {
    signals::install_signal_handlers();
}

fn reset_signals_for_child() {
    signals::reset_signals_for_child();
}

fn execute_pipeline(pipeline: &parser::Pipeline, job_manager: &Rc<RefCell<JobManager>>) {
    let num_commands = pipeline.commands.len();

    if num_commands == 0 {
        return;
    }

    let mut pipes: Vec<(RawFd, RawFd)> = Vec::new();

    // Create pipes for pipeline
    for _ in 0..num_commands.saturating_sub(1) {
        if let Ok((read_fd, write_fd)) = pipe() {
            pipes.push((read_fd, write_fd));
        } else {
            eprintln!("Failed to create pipe");
            return;
        }
    }

    let mut pids = Vec::new();

    for (i, cmd) in pipeline.commands.iter().enumerate() {
        match unsafe { fork() } {
            Ok(ForkResult::Child) => {
                reset_signals_for_child();

                // Set up stdout redirection if requested by the command
                if let Some((ref path, append)) = cmd.stdout_redirect {
                    let mut options = OpenOptions::new();
                    options.write(true).create(true);
                    if append {
                        options.append(true);
                    } else {
                        options.truncate(true);
                    }

                    if let Ok(file) = options.open(path) {
                        let fd = file.as_raw_fd();
                        unsafe {
                            libc::dup2(fd, 1);
                        }
                    }
                } else if pipeline.background {
                    if let Ok(devnull) = OpenOptions::new().write(true).open("/dev/null") {
                        let fd = devnull.as_raw_fd();
                        if i == num_commands - 1 {
                            // Last command in pipeline
                            unsafe {
                                libc::dup2(fd, 1);
                                libc::dup2(fd, 2);
                            }
                        }
                    }
                }

                // Set up pipes
                if i > 0 {
                    // Not first command: read from previous pipe
                    unsafe {
                        libc::dup2(pipes[i - 1].0, 0);
                    }
                }

                if i < num_commands - 1 {
                    // Not last command: write to next pipe
                    unsafe {
                        libc::dup2(pipes[i].1, 1);
                    }
                }

                // Close all pipe fds
                for (read_fd, write_fd) in &pipes {
                    unsafe {
                        libc::close(*read_fd);
                        libc::close(*write_fd);
                    }
                }

                // Execute command
                let command = CString::new(cmd.argv[0].as_str()).unwrap();
                let args: Vec<CString> = cmd
                    .argv
                    .iter()
                    .map(|s| CString::new(s.as_str()).unwrap())
                    .collect();

                // execvp replaces the process - if it returns, there was an error
                let _ = execvp(&command, &args);
                eprintln!("{}: command not found", cmd.argv[0]);
                std::process::exit(127);
            }
            Ok(ForkResult::Parent { child }) => {
                pids.push(child);
            }
            Err(e) => {
                eprintln!("fork failed: {}", e);
                return;
            }
        }
    }

    // Close all pipes in parent
    for (read_fd, write_fd) in pipes {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    // Handle foreground or background
    if pipeline.background {
        // Add last process to job list
        if let Some(&last_pid) = pids.last() {
            let command_line = pipeline
                .commands
                .iter()
                .map(|c| c.argv.join(" "))
                .collect::<Vec<_>>()
                .join(" | ");

            let job_id =
                job_manager
                    .borrow_mut()
                    .add_job(last_pid, command_line, JobState::Running);
            println!("[{}] {}", job_id, last_pid);
        }
    } else {
        // Wait for all processes in foreground
        for pid in pids {
            waitpid(pid, None).ok();
        }
    }
}

fn run_shell() -> rustyline::Result<()> {
    setup_signals();

    let config = Config::builder().build();
    let helper = ShellCompleter::new();
    let mut rl: Editor<ShellCompleter, DefaultHistory> = Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    let job_manager = Rc::new(RefCell::new(JobManager::new()));
    let registry = Rc::new(BuiltinRegistry::new()); // run_shell
    let mut executor = Executor::new(job_manager.clone(), registry.clone());
    set_universal_shell_env(&mut executor);
    let mut aliases: HashMap<String, String> = HashMap::new();
    source_startup_file(&mut executor, &mut aliases);

    let mut multiline_buffer = String::new();
    let mut in_control_flow = false;
    let mut last_command: Option<String> = None;

    loop {
        // Update job states before prompt
        job_manager.borrow_mut().update_job_states();

        let prompt = if in_control_flow {
            "... ".to_string()
        } else {
            get_prompt_string()
        };
        let readline = rl.readline(&prompt);

        match readline {
            Ok(line) => {
                if line.trim().is_empty() && !in_control_flow {
                    continue;
                }

                let upper = line.trim().to_uppercase();

                // Check if starting control flow
                if !in_control_flow
                    && (upper.starts_with("IF ")
                        || upper.starts_with("WHILE ")
                        || upper.starts_with("FOR ")
                        || upper.starts_with("LOOP"))
                {
                    in_control_flow = true;
                    multiline_buffer.push_str(&line);
                    multiline_buffer.push('\n');
                    continue;
                }

                // Check if ending control flow
                if in_control_flow {
                    multiline_buffer.push_str(&line);
                    multiline_buffer.push('\n');

                    if upper.starts_with("ENDIF")
                        || upper.starts_with("ENDWHILE")
                        || upper.starts_with("ENDFOR")
                        || upper.starts_with("ENDLOOP")
                        || upper.starts_with("DONE")
                    {
                        // Check if we have balanced blocks
                        let if_count = multiline_buffer.lines().filter(|line| line.trim().to_uppercase().starts_with("IF ")).count();
                        let endif_count = multiline_buffer.lines().filter(|line| line.trim().to_uppercase().starts_with("ENDIF")).count();
                        let while_count = multiline_buffer.lines().filter(|line| line.trim().to_uppercase().starts_with("WHILE ")).count();
                        let endwhile_count = multiline_buffer.lines().filter(|line| line.trim().to_uppercase().starts_with("ENDWHILE")).count();
                        let for_count = multiline_buffer.lines().filter(|line| line.trim().to_uppercase().starts_with("FOR ")).count();
                        let endfor_count = multiline_buffer.lines().filter(|line| line.trim().to_uppercase().starts_with("ENDFOR")).count();
                        let loop_count = multiline_buffer.lines().filter(|line| line.trim().to_uppercase().starts_with("LOOP")).count();
                        let endloop_count = multiline_buffer.lines().filter(|line| {
                            let upper = line.trim().to_uppercase();
                            upper.starts_with("ENDLOOP") || upper.starts_with("DONE")
                        }).count();

                        if if_count <= endif_count && while_count <= endwhile_count && for_count <= endfor_count && loop_count <= endloop_count {
                            in_control_flow = false;

                            // Parse and execute control flow
                            let mut parser = ControlFlowParser::new(&multiline_buffer);
                            match parser.parse() {
                                Ok(statements) => {
                                    if let Err(e) = executor.execute_statements(&statements) {
                                        eprintln!("Execution error: {}", e);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Parse error: {}", e);
                                }
                            }

                            multiline_buffer.clear();
                            continue;
                        }
                    }
                    continue;
                }

                let mut input_line = line.clone();
                match expand_history(&input_line, last_command.as_deref()) {
                    Ok(expanded) => input_line = expanded,
                    Err(err) => {
                        eprintln!("{}", err);
                        continue;
                    }
                }

                let _ = rl.add_history_entry(&input_line);

                // Skip comment lines
                if input_line.trim().starts_with('#') || input_line.trim().starts_with("//") {
                    continue;
                }

                let mut timed = false;
                let mut timed_line = input_line.clone();
                if let Some(rest) = strip_time_prefix(&input_line) {
                    timed = true;
                    timed_line = rest.to_string();
                }

                if timed_line.trim().starts_with("@define ") {
                    if let Some((name, value)) = parse_define_line(&timed_line) {
                        aliases.insert(name, value);
                    }
                    last_command = Some(input_line.clone());
                    continue;
                }

                timed_line = expand_aliases_in_line(&timed_line, &aliases);
                let mut parser = ControlFlowParser::new(&timed_line);
                match parser.parse() {
                    Ok(statements) => {
                        if timed {
                            let start = std::time::Instant::now();
                            if let Err(e) = executor.execute_statements(&statements) {
                                if e != "Interrupted" {
                                    eprintln!("Execution error: {}", e);
                                } else {
                                    println!();
                                }
                            }
                            let elapsed = start.elapsed();
                            print_time_summary(elapsed);
                        } else if let Err(e) = executor.execute_statements(&statements) {
                            if e != "Interrupted" {
                                eprintln!("Execution error: {}", e);
                            } else {
                                println!();
                            }
                        }
                        last_command = Some(input_line.clone());
                    }
                    Err(e) => {
                        eprintln!("Parse error: {}", e);
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C
                signals::clear_sigint();
                if in_control_flow {
                    in_control_flow = false;
                    multiline_buffer.clear();
                    println!("^C");
                }
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D
                println!();
                break;
            }
            Err(err) => {
                eprintln!("Error: {:?}", err);
                break;
            }
        }
    }

    Ok(())
}

fn is_tty() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) != 0 }
}

fn run_script(input: &str, script_args: &[String], script_path: Option<&std::path::Path>) -> Result<(), String> {
    setup_signals();

    let job_manager = Rc::new(RefCell::new(JobManager::new()));
    let registry = Rc::new(BuiltinRegistry::new());
    let mut executor = Executor::new(job_manager.clone(), registry.clone());
    set_universal_shell_env(&mut executor);

    let args_value = Value::List(script_args.to_vec());
    executor.env().set("args".to_string(), args_value);

    let (processed_input, main_function, just_runit, just_carry_on, stdlib_enabled, _) = preprocess_script(input, script_path, true)?;
    if stdlib_enabled {
        executor.env().set("__stdlib_enabled".to_string(), Value::Bool(true));
    }
    let mut parser = ControlFlowParser::new(&processed_input);
    let statements = parser
        .parse()
        .map_err(|e| format!("Parse error: {}", e))?;

    if just_carry_on {
        executor.set_carry_on_errors(true);
    }

    if let Some(main_fn) = main_function {
        // Only evaluate definitions needed to register functions and types.
        for stmt in statements.iter().filter(|stmt| matches!(stmt,
            Statement::FunctionDef { .. } |
            Statement::StructDef { .. } |
            Statement::EnumDef { .. }
        )) {
            executor.execute_statement(stmt)?;
        }

        let main_call = format!("{}()", main_fn);
        executor.execute_command(&main_call)?;
    } else if just_runit {
        executor.execute_statements(&statements)?;
    }

    Ok(())
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn parse_define_line(line: &str) -> Option<(String, String)> {
    let rest = line.trim().trim_start_matches("@define").trim();
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next()?.to_string();
    let value = parts.next()?.trim_start().to_string();
    if value.is_empty() {
        None
    } else {
        Some((name, value))
    }
}

fn strip_c_ffi_calls(line: &str) -> String {
    let mut output = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut in_double_quote = false;
    let mut in_single_quote = false;
    let mut escaped = false;

    while i < chars.len() {
        let c = chars[i];

        if escaped {
            output.push(c);
            escaped = false;
            i += 1;
            continue;
        }

        if c == '\\' && (in_double_quote || in_single_quote) {
            output.push(c);
            escaped = true;
            i += 1;
            continue;
        }

        if in_double_quote {
            output.push(c);
            if c == '"' {
                in_double_quote = false;
            }
            i += 1;
            continue;
        }

        if in_single_quote {
            output.push(c);
            if c == '\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }

        if c == '"' {
            in_double_quote = true;
            output.push(c);
            i += 1;
            continue;
        }

        if c == '\'' {
            in_single_quote = true;
            output.push(c);
            i += 1;
            continue;
        }

        if c == 'c' && i + 3 < chars.len() && chars[i + 1] == ':' && chars[i + 2] == ':' {
            let mut j = i + 3;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j < chars.len() && chars[j] == '(' {
                let mut depth = 0;
                let mut k = j;
                let mut inner_double = false;
                let mut inner_single = false;
                let mut inner_escaped = false;
                while k < chars.len() {
                    let ch = chars[k];
                    if inner_escaped {
                        inner_escaped = false;
                    } else if ch == '\\' && (inner_double || inner_single) {
                        inner_escaped = true;
                    } else if inner_double {
                        if ch == '"' {
                            inner_double = false;
                        }
                    } else if inner_single {
                        if ch == '\'' {
                            inner_single = false;
                        }
                    } else {
                        if ch == '"' {
                            inner_double = true;
                        } else if ch == '\'' {
                            inner_single = true;
                        } else if ch == '(' {
                            depth += 1;
                        } else if ch == ')' {
                            depth -= 1;
                            if depth == 0 {
                                k += 1;
                                break;
                            }
                        }
                    }
                    k += 1;
                }
                if depth == 0 {
                    i = k;
                    continue;
                }
            }
        }

        output.push(c);
        i += 1;
    }

    output
}

fn expand_aliases_in_line(line: &str, aliases: &HashMap<String, String>) -> String {
    if aliases.is_empty() {
        return line.to_string();
    }

    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return line.to_string();
    }

    let prefix_len = line.len() - trimmed.len();
    let prefix = &line[..prefix_len];

    let first_token_end = trimmed
        .char_indices()
        .find(|&(_, c)| c.is_whitespace() || (!c.is_alphanumeric() && c != '_' && c != ':'))
        .map(|(i, _)| i)
        .unwrap_or(trimmed.len());

    let first_token = &trimmed[..first_token_end];
    if let Some(expansion) = aliases.get(first_token) {
        let suffix = &trimmed[first_token_end..];
        format!("{}{}{}", prefix, expansion, suffix)
    } else {
        line.to_string()
    }
}

fn expand_history(line: &str, last_command: Option<&str>) -> Result<String, String> {
    if !line.contains("!!") {
        return Ok(line.to_string());
    }

    let prev = match last_command {
        Some(prev) => prev,
        None => return Err("alsh: !!: event not found".to_string()),
    };

    let mut result = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '!' {
            if let Some('!') = chars.peek() {
                let prev_char = result.chars().rev().next();
                let next_char = chars.clone().nth(1);
                let valid_before = prev_char.map_or(true, |c| c.is_whitespace());
                let valid_after = next_char.map_or(true, |c| c.is_whitespace());

                if valid_before && valid_after {
                    // Consume the second !
                    chars.next();
                    result.push_str(prev);
                    continue;
                }
            }
        }
        result.push(ch);
    }

    Ok(result)
}

fn strip_time_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed == "time" {
        return Some("");
    }
    if let Some(rest) = trimmed.strip_prefix("time ") {
        return Some(rest);
    }
    None
}

fn print_time_summary(duration: std::time::Duration) {
    let total_secs = duration.as_secs_f64();
    let minutes = (total_secs / 60.0).floor() as u64;
    let seconds = total_secs - (minutes as f64 * 60.0);
    println!("real \t{}m{:.3}s", minutes, seconds);
}

fn shorten_cwd(cwd: &Path, home_dir: Option<&Path>) -> String {
    if let Some(home) = home_dir {
        if let Ok(rel) = cwd.strip_prefix(home) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~{}", rel.display());
        }
    }
    cwd.display().to_string()
}

fn cwd_basename(cwd: &Path, home_dir: Option<&Path>) -> String {
    if let Some(home) = home_dir {
        if let Ok(rel) = cwd.strip_prefix(home) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
        }
    }

    cwd.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.display().to_string())
}

fn short_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().split('.').next().unwrap_or("host").to_string())
        .unwrap_or_else(|| "host".to_string())
}

fn format_prompt(prompt: &str) -> String {
    let cwd = env::current_dir().ok();
    let home_dir = env::var("HOME").ok().map(|s| Path::new(&s).to_path_buf());
    let cwd_str = cwd
        .as_ref()
        .map(|path| shorten_cwd(path, home_dir.as_deref()))
        .unwrap_or_else(|| "?".to_string());
    let user = env::var("USER").unwrap_or_else(|_| "user".to_string());
    let host = short_hostname();
    let is_root = geteuid().is_root();

    let mut result = String::new();
    let mut chars = prompt.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('u') => result.push_str(&user),
                Some('h') => result.push_str(&host),
                Some('w') => result.push_str(&cwd_str),
                Some('W') => {
                    let cwd = env::current_dir().ok();
                    let home_dir = env::var("HOME").ok().map(|s| Path::new(&s).to_path_buf());
                    let basename = cwd
                        .as_ref()
                        .map(|path| cwd_basename(path, home_dir.as_deref()))
                        .unwrap_or_else(|| "?".to_string());
                    result.push_str(&basename);
                }
                Some('$') => result.push(if is_root { '#' } else { '$' }),
                Some('n') => result.push('\n'),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn get_prompt_string() -> String {
    let raw = env::var("PS1").unwrap_or_else(|_| "[\\u@\\h \\W]$ ".to_string());
    format_prompt(&raw)
}

fn set_universal_shell_env(executor: &mut Executor) {
    let shell_path = "/usr/local/bin/alsh".to_string();
    std::env::set_var("SHELL", &shell_path);
    executor.env().set("SHELL".to_string(), Value::String(shell_path));

    let shlvl = std::env::var("SHLVL")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0)
        + 1;
    let shlvl_str = shlvl.to_string();
    std::env::set_var("SHLVL", &shlvl_str);
    executor.env().set("SHLVL".to_string(), Value::String(shlvl_str));

    if let Ok(cwd) = env::current_dir() {
        let cwd_str = cwd.to_string_lossy().to_string();
        std::env::set_var("PWD", &cwd_str);
        executor.env().set("PWD".to_string(), Value::String(cwd_str));
    }
}

fn source_startup_file(executor: &mut Executor, aliases: &mut HashMap<String, String>) {
    let home = match env::var("HOME") {
        Ok(path) => path,
        Err(_) => return,
    };

    let rc_path = Path::new(&home).join(".alshrc");
    if !rc_path.exists() {
        return;
    }

    let contents = match read_to_string(&rc_path) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("Failed to read {}: {}", rc_path.display(), err);
            return;
        }
    };

    match preprocess_script(&contents, Some(&rc_path), false) {
        Ok((processed, _, _, _, _, new_aliases)) => {
            aliases.extend(new_aliases);
            let mut parser = ControlFlowParser::new(&processed);
            match parser.parse() {
                Ok(statements) => {
                    if let Err(err) = executor.execute_statements(&statements) {
                        eprintln!("Error while sourcing {}: {}", rc_path.display(), err);
                    }
                }
                Err(err) => {
                    eprintln!("Parse error in {}: {}", rc_path.display(), err);
                }
            }
        }
        Err(err) => {
            eprintln!("Failed to preprocess {}: {}", rc_path.display(), err);
        }
    }
}

fn preprocess_script(input: &str, script_path: Option<&std::path::Path>, allow_main: bool) -> Result<(String, Option<String>, bool, bool, bool, HashMap<String, String>), String> {
    let mut output = String::new();
    let mut defines: HashMap<String, String> = HashMap::new();
    let mut main_function: Option<String> = None;
    let mut just_runit = false;
    let mut just_carry_on = false;
    let mut noffi = false;
    let mut expect_main = false;
    let mut stdlib_enabled = false;

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            output.push_str(line);
            output.push('\n');
            continue;
        }

        if trimmed == "@stdlib" {
            stdlib_enabled = true;
            continue;
        }

        if trimmed.starts_with("@define ") {
            if let Some((name, value)) = parse_define_line(trimmed) {
                defines.insert(name, value);
            }
            continue;
        }

        if trimmed.starts_with("@include ") {
            let path_token = trimmed[9..].trim();
            let include_path = strip_quotes(path_token);
            let include_file = if let Some(base) = script_path {
                base.parent().unwrap_or_else(|| Path::new(".")).join(include_path)
            } else {
                std::path::PathBuf::from(include_path)
            };
            let included = std::fs::read_to_string(&include_file)
                .map_err(|e| format!("Failed to include {}: {}", include_file.display(), e))?;
            let (processed_included, _, _, _, _, _) = preprocess_script(&included, Some(&include_file), false)?;
            output.push_str(&processed_included);
            output.push('\n');
            continue;
        }

        if trimmed == "@main" {
            if allow_main {
                expect_main = true;
            }
            continue;
        }

        if trimmed == "@justrunit" {
            just_runit = true;
            continue;
        }

        if trimmed == "@justcarryon" {
            just_carry_on = true;
            continue;
        }

        if trimmed == "@noffi" {
            noffi = true;
            continue;
        }

        if expect_main && (trimmed.to_lowercase().starts_with("function ") || trimmed.to_lowercase().starts_with("fn ")) {
            if let Some(name) = extract_function_name(trimmed) {
                main_function = Some(name);
            }
            expect_main = false;
        }

        let mut processed_line = line.to_string();
        processed_line = expand_aliases_in_line(&processed_line, &defines);
        if noffi {
            processed_line = strip_c_ffi_calls(&processed_line);
            if processed_line.trim().is_empty() {
                continue;
            }
        }
        output.push_str(&processed_line);
        output.push('\n');
    }

    if stdlib_enabled {
        output = format!("LET __stdlib_enabled = true\n{}", output);
    }

    Ok((output, main_function, just_runit, just_carry_on, stdlib_enabled, defines))
}

fn extract_function_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let remainder = if trimmed.to_lowercase().starts_with("function ") {
        trimmed[8..].trim()
    } else if trimmed.to_lowercase().starts_with("fn ") {
        trimmed[2..].trim()
    } else {
        return None;
    };

    if let Some(open_paren) = remainder.find('(') {
        return Some(remainder[..open_paren].trim().to_string());
    }

    let parts: Vec<&str> = remainder.split_whitespace().collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts[0].to_string())
    }
}

fn main() {
    // Set up panic hook to ensure terminal is restored
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Try to reset terminal
        let _ = std::process::Command::new("reset").status();
        default_panic(info);
    }));

    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        // Run script file
        match std::fs::read_to_string(&args[1]) {
            Ok(content) => {
                if let Err(e) = run_script(&content, &args[2..], Some(std::path::Path::new(&args[1]))) {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("Error reading file {}: {}", args[1], e);
                std::process::exit(1);
            }
        }
    } else if is_tty() {
        // Interactive mode
        if let Err(err) = run_shell() {
            eprintln!("Error: {:?}", err);
            std::process::exit(1);
        }
    } else {
        // Read from stdin (script piped to shell)
        let mut input = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut input) {
            eprintln!("Error reading stdin: {}", e);
            std::process::exit(1);
        }

        if !input.is_empty() {
            if let Err(e) = run_script(&input, &[], None) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }
}
