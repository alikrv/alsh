// src/parser.rs
use std::env;

#[derive(Debug, Clone)]
pub struct Command {
    pub argv: Vec<String>,
    pub raw: String,
    pub stdout_redirect: Option<(String, bool)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PipelineMode {
    Shell,
    Stream,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<Command>,
    pub background: bool,
    pub mode: PipelineMode,
}

pub fn parse_line(line: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut chars = line.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }

        let mut token = String::new();

        if c == '"' || c == '\'' {
            let quote = chars.next().unwrap();
            while let Some(&ch) = chars.peek() {
                chars.next();
                if ch == quote {
                    break;
                }
                token.push(ch);
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || ch == '|' || ch == '#' {
                    break;
                }
                chars.next();
                token.push(ch);
            }
        }

        if !token.is_empty() {
            argv.push(token);
        }
    }

    argv
}

fn split_unquoted(line: &str, sep: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        if let Some(quote) = in_quote {
            if ch == quote {
                in_quote = None;
            }
            current.push(ch);
            continue;
        }

        if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
            current.push(ch);
            continue;
        }

        if sep == "|" && ch == '|' {
            parts.push(current.trim().to_string());
            current.clear();
            continue;
        }

        if sep == "->" && ch == '-' {
            if let Some(&'>') = chars.peek() {
                chars.next();
                parts.push(current.trim().to_string());
                current.clear();
                continue;
            }
        }

        current.push(ch);
    }

    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }

    parts
}

pub fn parse_pipeline(line: &str) -> Pipeline {
    let background = line.trim().ends_with('&');
    let line = if background {
        line.trim_end_matches('&').trim()
    } else {
        line
    };

    let mode = if split_unquoted(line, "->").len() > 1 {
        PipelineMode::Stream
    } else {
        PipelineMode::Shell
    };

    let commands: Vec<Command> = if mode == PipelineMode::Stream {
        split_unquoted(line, "->")
    } else {
        split_unquoted(line, "|")
    }
    .into_iter()
    .map(|cmd| {
        let raw = cmd.trim().to_string();
        let mut argv = parse_line(&raw);
        expand_tilde(&mut argv);

        let mut stdout_redirect = None;
        let mut cleaned_argv = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            if argv[i] == ">" || argv[i] == ">>" {
                if i + 1 < argv.len() {
                    stdout_redirect = Some((argv[i + 1].clone(), argv[i] == ">>"));
                    i += 2;
                    continue;
                }
            }
            cleaned_argv.push(argv[i].clone());
            i += 1;
        }

        Command {
            argv: cleaned_argv,
            raw,
            stdout_redirect,
        }
    })
    .filter(|cmd| !cmd.argv.is_empty())
    .collect();

    Pipeline {
        commands,
        background,
        mode,
    }
}

pub fn expand_tilde(argv: &mut Vec<String>) {
    if let Ok(home) = env::var("HOME") {
        for arg in argv.iter_mut() {
            if arg.starts_with('~') {
                *arg = arg.replacen('~', &home, 1);
            }
        }
    }
}
pub fn expand_env_vars(argv: &mut Vec<String>) {
    for arg in argv.iter_mut() {
        if arg.contains('$') {
            *arg = expand_env_token(arg);
        }
    }
}

fn expand_env_token(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some(&'{') = chars.peek() {
                chars.next(); // consume '{'
                let mut var_name = String::new();

                while let Some(&ch) = chars.peek() {
                    if ch.is_alphanumeric() || ch == '_' {
                        var_name.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }

                // expect closing }
                if let Some('}') = chars.next() {
                    if let Ok(val) = std::env::var(&var_name) {
                        result.push_str(&val);
                        continue;
                    }
                }
            }
        }

        result.push(c);
    }

    result
}
