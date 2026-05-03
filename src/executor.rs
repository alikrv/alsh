// src/executor.rs
use crate::builtin::BuiltinRegistry;
use crate::control_flow::*;
use crate::jobs::JobManager;
use crate::parser;
use nix::libc;
use nix::sys::wait::waitpid;
use nix::unistd::{dup, dup2, execvp, fork, pipe, ForkResult};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use crate::signals;
use std::ptr;
use std::rc::Rc;
use std::sync::OnceLock;

#[derive(Debug)]
enum ExecutionControl {
    Normal(i32),
    Break,
    Continue,
    Return(Value),
}

pub struct Executor {
    env: Environment,
    job_manager: Rc<RefCell<JobManager>>,
    registry: Rc<BuiltinRegistry>,
    carry_on_errors: bool,
}

impl Executor {
    pub fn new(job_manager: Rc<RefCell<JobManager>>, registry: Rc<BuiltinRegistry>) -> Self {
        Executor {
            env: Environment::new(),
            job_manager,
            registry,
            carry_on_errors: false,
        }
    }

    pub fn env(&mut self) -> &mut Environment {
        &mut self.env
    }

    pub fn set_carry_on_errors(&mut self, carry_on: bool) {
        self.carry_on_errors = carry_on;
    }

    pub fn execute_statements(&mut self, statements: &[Statement]) -> Result<i32, String> {
        match self.execute_statements_internal(statements, false) {
            Ok(ExecutionControl::Normal(code)) => Ok(code),
            Ok(ExecutionControl::Break) => Err("BREAK outside of loop".to_string()),
            Ok(ExecutionControl::Continue) => Err("CONTINUE outside of loop".to_string()),
            Ok(ExecutionControl::Return(_)) => Err("RETURN outside of function".to_string()),
            Err(e) => Err(e),
        }
    }

    pub fn execute_statement(&mut self, statement: &Statement) -> Result<i32, String> {
        self.execute_statements(std::slice::from_ref(statement))
    }

    pub fn execute_statements_with_nonzero_error(&mut self, statements: &[Statement]) -> Result<i32, String> {
        match self.execute_statements_internal(statements, true) {
            Ok(ExecutionControl::Normal(code)) => Ok(code),
            Ok(ExecutionControl::Break) => Err("BREAK outside of loop".to_string()),
            Ok(ExecutionControl::Continue) => Err("CONTINUE outside of loop".to_string()),
            Ok(ExecutionControl::Return(_)) => Err("RETURN outside of function".to_string()),
            Err(e) => Err(e),
        }
    }

    pub fn execute_line(&mut self, line: &str) -> Result<i32, String> {
        self.execute_command(line)
    }

    fn check_for_interrupt(&mut self) -> Result<(), String> {
        if signals::take_sigint() {
            Err("Interrupted".to_string())
        } else {
            Ok(())
        }
    }

    fn execute_statements_internal(&mut self, statements: &[Statement], treat_nonzero_as_error: bool) -> Result<ExecutionControl, String> {
        let mut last_exit_code = 0;

        for stmt in statements {
            self.check_for_interrupt()?;
            match self.execute_statement_internal(stmt, treat_nonzero_as_error) {
                Ok(ExecutionControl::Normal(code)) => last_exit_code = code,
                Ok(ExecutionControl::Break) => return Ok(ExecutionControl::Break),
                Ok(ExecutionControl::Continue) => return Ok(ExecutionControl::Continue),
                Ok(ExecutionControl::Return(value)) => return Ok(ExecutionControl::Return(value)),
                Err(e) => {
                    if self.carry_on_errors {
                        eprintln!("Error: {}", e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        Ok(ExecutionControl::Normal(last_exit_code))
    }

    fn execute_statement_internal(&mut self, stmt: &Statement, treat_nonzero_as_error: bool) -> Result<ExecutionControl, String> {
        match stmt {
            Statement::Let { name, value } => {
                let evaluated = self.eval_expression(value)?;
                self.env.set(name.clone(), evaluated);
                Ok(ExecutionControl::Normal(0))
            }
            Statement::Command(cmd) => {
                let expanded = self.env.expand_vars(cmd);

                if expanded == "_bail" {
                    std::process::exit(1);
                }

                if let Some((name, args)) = Self::parse_function_call(&expanded) {
                    if name == "_bail" || name == "_exit" {
                        Self::handle_exit_builtin(&name, &args)?;
                    }

                    if name.starts_with("c::") {
                        let _ = self.execute_c_function(&name, &args)?;
                        return Ok(ExecutionControl::Normal(0));
                    }

                    if self.env.get_function(&name).is_some() {
                        self.call_function(&name, &args, treat_nonzero_as_error)?;
                        return Ok(ExecutionControl::Normal(0));
                    }
                }

                let code = self.execute_command(&expanded)?;
                if treat_nonzero_as_error && code != 0 {
                    return Err(format!("Command failed with exit code {}: {}", code, expanded));
                }
                Ok(ExecutionControl::Normal(code))
            }
            Statement::Return { value } => {
                let result = if let Some(expr) = value {
                    self.eval_expression(expr)?
                } else {
                    Value::String(String::new())
                };
                Ok(ExecutionControl::Return(result))
            }
            Statement::Foreach { var, iterable, body } => {
                let items_value = self.eval_expression(iterable)?;
                let iter_items = match items_value {
                    Value::List(list) => list,
                    Value::String(s) => vec![s],
                    Value::Number(n) => vec![n.to_string()],
                    Value::Float(f) => vec![f.to_string()],
                    Value::Bool(b) => vec![if b { "true".to_string() } else { "false".to_string() }],
                    Value::Struct(_, _) => vec![items_value.as_string()],
                    Value::Enum(_, _) => vec![items_value.as_string()],
                };

                let mut last_exit_code = 0;
                for item in iter_items {
                    self.env.set(var.clone(), Value::String(item));
                    match self.execute_statements_internal(body, treat_nonzero_as_error)? {
                        ExecutionControl::Normal(code) => last_exit_code = code,
                        ExecutionControl::Break => break,
                        ExecutionControl::Continue => continue,
                        ExecutionControl::Return(value) => return Ok(ExecutionControl::Return(value)),
                    }
                }
                Ok(ExecutionControl::Normal(last_exit_code))
            }
            Statement::ForLoop { init, condition, update, body } => {
                if let Some(init_expr) = init {
                    if let Some(stmt) = self.parse_single_statement(init_expr.as_str())? {
                        self.execute_statement_internal(&stmt, treat_nonzero_as_error)?;
                    }
                }

                let mut last_exit_code = 0;
                while self.eval_condition(condition)? {
                    match self.execute_statements_internal(body, treat_nonzero_as_error)? {
                        ExecutionControl::Normal(code) => last_exit_code = code,
                        ExecutionControl::Break => break,
                        ExecutionControl::Continue => {
                            if let Some(update_expr) = update {
                                if let Some(stmt) = self.parse_single_statement(update_expr.as_str())? {
                                    match self.execute_statement_internal(&stmt, treat_nonzero_as_error)? {
                                        ExecutionControl::Normal(_) => {}
                                        ExecutionControl::Break => break,
                                        ExecutionControl::Continue => continue,
                                        ExecutionControl::Return(value) => return Ok(ExecutionControl::Return(value)),
                                    }
                                }
                            }
                            continue;
                        }
                        ExecutionControl::Return(value) => return Ok(ExecutionControl::Return(value)),
                    }

                    if let Some(update_expr) = update {
                        if let Some(stmt) = self.parse_single_statement(update_expr.as_str())? {
                            match self.execute_statement_internal(&stmt, treat_nonzero_as_error)? {
                                ExecutionControl::Normal(_) => {}
                                ExecutionControl::Break => break,
                                ExecutionControl::Continue => continue,
                                ExecutionControl::Return(value) => return Ok(ExecutionControl::Return(value)),
                            }
                        }
                    }
                }
                Ok(ExecutionControl::Normal(last_exit_code))
            }
            Statement::Break { condition } => {
                let should_break = if let Some(cond) = condition {
                    self.eval_condition(cond)?
                } else {
                    true
                };

                if should_break {
                    Ok(ExecutionControl::Break)
                } else {
                    Ok(ExecutionControl::Normal(0))
                }
            }
            Statement::Continue => Ok(ExecutionControl::Continue),
            Statement::If {
                condition,
                then_block,
                elif_blocks,
                else_block,
            } => {
                if self.eval_condition(condition)? {
                    self.execute_statements_internal(then_block, treat_nonzero_as_error)
                } else {
                    for (elif_cond, elif_body) in elif_blocks {
                        if self.eval_condition(elif_cond)? {
                            return self.execute_statements_internal(elif_body, treat_nonzero_as_error);
                        }
                    }

                    if let Some(else_body) = else_block {
                        self.execute_statements_internal(else_body, treat_nonzero_as_error)
                    } else {
                        Ok(ExecutionControl::Normal(0))
                    }
                }
            }
            Statement::While { condition, body } => {
                let mut last_exit_code = 0;
                while self.eval_condition(condition)? {
                    match self.execute_statements_internal(body, treat_nonzero_as_error)? {
                        ExecutionControl::Normal(code) => last_exit_code = code,
                        ExecutionControl::Break => break,
                        ExecutionControl::Continue => continue,
                        ExecutionControl::Return(value) => return Ok(ExecutionControl::Return(value)),
                    }
                }
                Ok(ExecutionControl::Normal(last_exit_code))
            }
            Statement::For { var, items, body } => {
                let mut last_exit_code = 0;
                let iter_items: Vec<String> = if items.len() == 1 {
                    if let Some(value) = self.env.get(&items[0]) {
                        if let Some(list) = value.as_list() {
                            list.clone()
                        } else {
                            vec![items[0].clone()]
                        }
                    } else {
                        vec![items[0].clone()]
                    }
                } else {
                    items.iter().map(|s| self.env.expand_vars(s)).collect()
                };

                for item in iter_items {
                    self.env.set(var.clone(), Value::String(item));
                    match self.execute_statements_internal(body, treat_nonzero_as_error)? {
                        ExecutionControl::Normal(code) => last_exit_code = code,
                        ExecutionControl::Break => break,
                        ExecutionControl::Continue => continue,
                        ExecutionControl::Return(value) => return Ok(ExecutionControl::Return(value)),
                    }
                }
                Ok(ExecutionControl::Normal(last_exit_code))
            }
            Statement::Loop { count, interval, body } => {
                let mut last_exit_code = 0;
                let mut iteration = 0u64;

                loop {
                    self.check_for_interrupt()?;

                    // Check if we've hit the iteration limit
                    if let Some(max_count) = count {
                        if iteration >= *max_count {
                            break;
                        }
                    }

                    // Execute loop body
                    match self.execute_statements_internal(body, treat_nonzero_as_error)? {
                        ExecutionControl::Normal(code) => last_exit_code = code,
                        ExecutionControl::Break => break,
                        ExecutionControl::Continue => {
                            iteration += 1;
                            if let Some(seconds) = interval {
                                std::thread::sleep(std::time::Duration::from_secs(*seconds));
                            } else if count.is_none() {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            self.check_for_interrupt()?;
                            continue;
                        }
                        ExecutionControl::Return(value) => return Ok(ExecutionControl::Return(value)),
                    }

                    iteration += 1;

                    // Sleep if interval is specified, otherwise small delay to prevent CPU spinning
                    if let Some(seconds) = interval {
                        std::thread::sleep(std::time::Duration::from_secs(*seconds));
                    } else if count.is_none() {
                        // For infinite loops without interval, add tiny delay to allow Ctrl+C
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }

                    self.check_for_interrupt()?;
                }

                Ok(ExecutionControl::Normal(last_exit_code))
            }
            Statement::Try {
                try_block,
                catch_block,
            } => {
                match self.execute_statements_internal(try_block, true) {
                    Ok(ExecutionControl::Normal(code)) => Ok(ExecutionControl::Normal(code)),
                    Ok(ExecutionControl::Break) => Ok(ExecutionControl::Break),
                    Ok(ExecutionControl::Continue) => Ok(ExecutionControl::Continue),
                    Ok(ExecutionControl::Return(value)) => Ok(ExecutionControl::Return(value)),
                    Err(_) => {
                        if let Some(catch_body) = catch_block {
                            self.execute_statements_internal(&catch_body, false)
                        } else {
                            Err("Uncaught exception in try block".to_string())
                        }
                    }
                }
            }
            Statement::StructDef { .. } => Ok(ExecutionControl::Normal(0)),
            Statement::EnumDef { .. } => Ok(ExecutionControl::Normal(0)),
            Statement::Scan {
                expr,
                enum_type,
                branches,
            } => {
                let value = if let Some(expr_value) = expr {
                    self.eval_expression(expr_value)?
                } else {
                    self.find_enum_value_by_type(&enum_type)?
                };

                for (label, branch_body) in branches {
                    let candidate = Value::Enum(enum_type.clone(), label.clone());
                    if value == candidate {
                        return self.execute_statements_internal(branch_body, treat_nonzero_as_error);
                    }
                }

                Ok(ExecutionControl::Normal(0))
            }
            Statement::Switch {
                expr,
                branches,
                default_branch,
            } => {
                let value = self.eval_expression(expr)?;

                for (label, branch_body) in branches {
                    let candidate = match &value {
                        Value::Enum(enum_type, _) => Value::Enum(enum_type.clone(), label.clone()),
                        Value::Number(_) => {
                            if let Ok(parsed) = label.parse::<i64>() {
                                Value::Number(parsed)
                            } else {
                                Value::String(label.clone())
                            }
                        }
                        Value::Float(_) => {
                            if let Ok(parsed) = label.parse::<f64>() {
                                Value::Float(parsed)
                            } else {
                                Value::String(label.clone())
                            }
                        }
                        Value::Bool(_) => {
                            match label.to_lowercase().as_str() {
                                "true" => Value::Bool(true),
                                "false" => Value::Bool(false),
                                _ => Value::String(label.clone()),
                            }
                        }
                        _ => Value::String(label.clone()),
                    };

                    if value == candidate {
                        return self.execute_statements_internal(branch_body, treat_nonzero_as_error);
                    }
                }

                if let Some(default_body) = default_branch {
                    return self.execute_statements_internal(default_body, treat_nonzero_as_error);
                }

                Ok(ExecutionControl::Normal(0))
            }
            Statement::Chain { steps } => {
                let _ = self.execute_chain_steps(steps)?;
                Ok(ExecutionControl::Normal(0))
            }
            Statement::FunctionDef { name, params, body } => {
                let function_def = FunctionDef {
                    params: params.clone(),
                    body: body.clone(),
                };
                self.env.set_function(name.clone(), function_def);
                Ok(ExecutionControl::Normal(0))
            }
        }
    }

    fn eval_condition(&mut self, cond: &Condition) -> Result<bool, String> {
        match cond {
            Condition::Command(cmd) => {
                let expanded = self.env.expand_vars(cmd);
                let exit_code = self.execute_command(&expanded)?;
                Ok(exit_code == 0)
            }
            Condition::Is(var, value) => {
                let var_expanded = self.env.expand_vars(var);
                let value_expanded = self.env.expand_vars(value);

                // Simple string comparison
                Ok(var_expanded == value_expanded)
            }
            Condition::IsNot(var, value) => {
                Ok(!self.eval_condition(&Condition::Is(var.clone(), value.clone()))?)
            }
            Condition::And(left, right) => {
                Ok(self.eval_condition(left)? && self.eval_condition(right)?)
            }
            Condition::Or(left, right) => {
                Ok(self.eval_condition(left)? || self.eval_condition(right)?)
            }
            Condition::Compare(left, op, right) => {
                let left_value = self.eval_expression(left)?;
                let right_value = self.eval_expression(right)?;

                let result = match (left_value.as_float(), right_value.as_float()) {
                    (Some(lhs), Some(rhs)) => match op {
                        CompareOp::Eq => lhs == rhs,
                        CompareOp::Ne => lhs != rhs,
                        CompareOp::Lt => lhs < rhs,
                        CompareOp::Gt => lhs > rhs,
                        CompareOp::Le => lhs <= rhs,
                        CompareOp::Ge => lhs >= rhs,
                    },
                    _ => match op {
                        CompareOp::Eq => left_value.as_string() == right_value.as_string(),
                        CompareOp::Ne => left_value.as_string() != right_value.as_string(),
                        _ => return Err("Non-numeric comparison requires == or !=".to_string()),
                    },
                };

                Ok(result)
            }
        }
    }

    pub fn execute_command(&mut self, cmd: &str) -> Result<i32, String> {
        if let Some((name, args)) = Self::parse_function_call(cmd) {
            if name == "_bail" || name == "_exit" {
                Self::handle_exit_builtin(&name, &args)?;
            }

            if name.starts_with("c::") {
                let _ = self.execute_c_function(&name, &args)?;
                return Ok(0);
            }

            if let Some(std_name) = self.normalize_std_function_name(&name) {
                if name == "readfile" || name == "writefile" || name == "appendfile" {
                    eprintln!("DEBUG execute_command std call {} args={:?}", name, args);
                }
                let _ = self.call_std_function(&std_name, &args)?;
                return Ok(0);
            }

            if self.env.get_function(&name).is_some() {
                if name == "feature_showcase" {
                    eprintln!("DEBUG execute_command user function call: {} args={:?}", name, args);
                }
                let _ = self.call_function(&name, &args, false)?;
                return Ok(0);
            }
        }

        let mut pipeline = parser::parse_pipeline(cmd);

        if pipeline.commands.is_empty() {
            return Ok(0);
        }

        // Expand variables in argv
        for command in &mut pipeline.commands {
            for arg in &mut command.argv {
                *arg = self.expand_vars(arg);
            }
        }

        if pipeline.mode == parser::PipelineMode::Stream {
            return self.execute_stream_pipeline(&pipeline);
        }

        // Check if single command that's a builtin
        if pipeline.commands.len() == 1 && !pipeline.background {
            let command = &pipeline.commands[0];
            if let Some(exit_code) = self.registry.run_builtin(&command.argv, &mut self.env, &self.job_manager) {
                return Ok(exit_code);
            }
        }

        // Execute as external command
        self.execute_external_pipeline(&pipeline)
    }

    fn parse_function_call(cmd: &str) -> Option<(String, Vec<String>)> {
        let trimmed = cmd.trim();
        let open_paren = trimmed.find('(')?;
        if !trimmed.ends_with(')') {
            return None;
        }

        let name = trimmed[..open_paren].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':') {
            return None;
        }

        let args_text = &trimmed[open_paren + 1..trimmed.len() - 1];
        let mut args = Vec::new();
        let mut current = String::new();
        let mut in_quote: Option<char> = None;
        let mut nesting_level = 0;
        let mut chars = args_text.chars().peekable();

        while let Some(c) = chars.next() {
            if let Some(q) = in_quote {
                if c == '\\' {
                    current.push(c);
                    if let Some(next_ch) = chars.next() {
                        current.push(next_ch);
                    }
                } else if c == q {
                    in_quote = None;
                    current.push(c);
                } else {
                    current.push(c);
                }
            } else if c == '"' || c == '\'' {
                in_quote = Some(c);
                current.push(c);
            } else if c == '(' || c == '[' || c == '{' {
                nesting_level += 1;
                current.push(c);
            } else if c == ')' || c == ']' || c == '}' {
                if nesting_level > 0 {
                    nesting_level -= 1;
                }
                current.push(c);
            } else if c == ',' && nesting_level == 0 {
                let arg = current.trim().to_string();
                if !arg.is_empty() {
                    args.push(arg);
                }
                current.clear();
            } else {
                current.push(c);
            }
        }

        let arg = current.trim().to_string();
        if !arg.is_empty() {
            args.push(arg);
        }

        Some((name.to_string(), args))
    }

    fn is_bare_identifier(s: &str) -> bool {
        s.chars().all(|c| c.is_alphanumeric() || c == '_') && !s.is_empty() && !s.chars().next().unwrap().is_digit(10)
    }

    fn handle_exit_builtin(name: &str, args: &[String]) -> Result<(), String> {
        match name {
            "_bail" => {
                if !args.is_empty() {
                    return Err("_bail does not take arguments".to_string());
                }
                std::process::exit(1);
            }
            "_exit" => {
                if args.len() != 1 {
                    return Err("_exit requires one numeric argument".to_string());
                }
                let code = args[0]
                    .parse::<i32>()
                    .map_err(|_| "_exit requires a numeric exit code".to_string())?;
                if code < 1 || code > 128 {
                    return Err("_exit requires a return code between 1 and 128".to_string());
                }
                std::process::exit(code);
            }
            _ => Ok(()),
        }
    }

    fn normalize_std_function_name(&self, name: &str) -> Option<String> {
        const STD_NAMES: &[&str] = &[
            "print",
            "println",
            "eprint",
            "eprintln",
            "input",
            "exit",
            "env",
            "readfile",
            "writefile",
            "appendfile",
            "exists",
            "strlen",
            "upper",
            "lower",
            "trim",
            "contains",
            "replace",
            "split",
            "lines",
            "startswith",
            "endswith",
            "padleft",
            "padright",
            "repeat",
            "strip",
            "len",
            "push",
            "pop",
            "first",
            "last",
            "slice",
            "reverse",
            "join",
            "sizeof",
            "format_gb",
            "format_gib",
            "min",
            "max",
            "clamp",
            "even",
            "odd",
            "basename",
            "dirname",
            "joinpath",
            "isfile",
            "isdir",
            "mkdir",
            "listdir",
            "which",
            "pid",
            "sleep",
            "getuser",
        ];

        if name.starts_with("std::") {
            return Some(name.to_string());
        }

        if self.env.get("__stdlib_enabled").is_some() && STD_NAMES.contains(&name) {
            return Some(format!("std::{}", name));
        }

        None
    }

    fn call_std_function(&mut self, name: &str, args: &[String]) -> Result<Value, String> {
        let evaluated_args: Result<Vec<Value>, String> = args.iter().map(|arg| self.eval_expression(arg)).collect();
        let args = evaluated_args?;

        match name {
            "std::print" => {
                let mut stdout = std::io::stdout();
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        stdout.write_all(b" ").map_err(|e| e.to_string())?;
                    }
                    let text = arg.as_string();
                    stdout.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                }
                stdout.flush().map_err(|e| e.to_string())?;
                Ok(Value::String(String::new()))
            }
            "std::println" => {
                let mut stdout = std::io::stdout();
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        stdout.write_all(b" ").map_err(|e| e.to_string())?;
                    }
                    let text = arg.as_string();
                    stdout.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                }
                stdout.write_all(b"\n").map_err(|e| e.to_string())?;
                Ok(Value::String(String::new()))
            }
            "std::eprint" => {
                let mut stderr = std::io::stderr();
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        stderr.write_all(b" ").map_err(|e| e.to_string())?;
                    }
                    let text = arg.as_string();
                    stderr.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                }
                stderr.flush().map_err(|e| e.to_string())?;
                Ok(Value::String(String::new()))
            }
            "std::eprintln" => {
                let mut stderr = std::io::stderr();
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        stderr.write_all(b" ").map_err(|e| e.to_string())?;
                    }
                    let text = arg.as_string();
                    stderr.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                }
                stderr.write_all(b"\n").map_err(|e| e.to_string())?;
                Ok(Value::String(String::new()))
            }
            "std::input" => {
                let prompt = if args.is_empty() {
                    String::new()
                } else if args.len() == 1 {
                    args[0].as_string()
                } else {
                    return Err("std::input takes at most one argument".to_string());
                };
                print!("{}", prompt);
                std::io::stdout().flush().map_err(|e| e.to_string())?;
                let mut line = String::new();
                std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                Ok(Value::String(line))
            }
            "std::exit" => {
                if args.len() != 1 {
                    return Err("std::exit requires one numeric argument".to_string());
                }
                let code = args[0]
                    .as_string()
                    .parse::<i32>()
                    .map_err(|_| "std::exit requires a numeric exit code".to_string())?;
                std::process::exit(code);
            }
            "std::env" => {
                match args.len() {
                    1 => {
                        let key = args[0].as_string();
                        Ok(Value::String(std::env::var(&key).unwrap_or_default()))
                    }
                    2 => {
                        let key = args[0].as_string();
                        let default = args[1].as_string();
                        Ok(Value::String(std::env::var(&key).unwrap_or(default)))
                    }
                    _ => Err("std::env requires one or two arguments".to_string()),
                }
            }
            "std::readfile" => {
                if args.len() != 1 {
                    return Err("std::readfile requires one path argument".to_string());
                }
                let path = args[0].as_string();
                eprintln!("DEBUG std::readfile path={}", path);
                let contents = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                Ok(Value::String(contents))
            }
            "std::writefile" => {
                if args.len() != 2 {
                    return Err("std::writefile requires a path and a string".to_string());
                }
                let path = args[0].as_string();
                let contents = args[1].as_string();
                std::fs::write(&path, contents).map_err(|e| e.to_string())?;
                Ok(Value::String(String::new()))
            }
            "std::appendfile" => {
                if args.len() != 2 {
                    return Err("std::appendfile requires a path and a string".to_string());
                }
                let path = args[0].as_string();
                let contents = args[1].as_string();
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| e.to_string())?;
                file.write_all(contents.as_bytes()).map_err(|e| e.to_string())?;
                Ok(Value::String(String::new()))
            }
            "std::exists" => {
                if args.len() != 1 {
                    return Err("std::exists requires one path argument".to_string());
                }
                let path = args[0].as_string();
                Ok(Value::Bool(std::path::Path::new(&path).exists()))
            }
            "std::strlen" => {
                if args.len() != 1 {
                    return Err("std::strlen requires one string argument".to_string());
                }
                Ok(Value::Number(args[0].as_string().len() as i64))
            }
            "std::upper" => {
                if args.len() != 1 {
                    return Err("std::upper requires one string argument".to_string());
                }
                Ok(Value::String(args[0].as_string().to_ascii_uppercase()))
            }
            "std::lower" => {
                if args.len() != 1 {
                    return Err("std::lower requires one string argument".to_string());
                }
                Ok(Value::String(args[0].as_string().to_ascii_lowercase()))
            }
            "std::trim" => {
                if args.len() != 1 {
                    return Err("std::trim requires one string argument".to_string());
                }
                Ok(Value::String(args[0].as_string().trim().to_string()))
            }
            "std::contains" => {
                if args.len() != 2 {
                    return Err("std::contains requires two arguments".to_string());
                }
                match &args[0] {
                    Value::String(a) => Ok(Value::Bool(a.contains(&args[1].as_string()))),
                    Value::List(items) => Ok(Value::Bool(items.iter().any(|item| item == &args[1].as_string()))),
                    _ => Err("std::contains requires a string or array as first argument".to_string()),
                }
            }
            "std::replace" => {
                if args.len() != 3 {
                    return Err("std::replace requires three string arguments".to_string());
                }
                Ok(Value::String(args[0].as_string().replace(&args[1].as_string(), &args[2].as_string())))
            }
            "std::split" => {
                if args.len() != 2 {
                    return Err("std::split requires two string arguments".to_string());
                }
                let haystack = args[0].as_string();
                let sep = args[1].as_string();
                let items = if sep.is_empty() {
                    haystack.chars().map(|c| c.to_string()).collect()
                } else {
                    haystack.split(&sep).map(|s| s.to_string()).collect()
                };
                Ok(Value::List(items))
            }
            "std::padleft" => {
                if args.len() != 3 {
                    return Err("std::padleft requires a string, width, and padding character".to_string());
                }
                let text = args[0].as_string();
                let width = args[1].as_string().parse::<usize>().map_err(|_| "std::padleft requires a numeric width".to_string())?;
                let pad_char = args[2].as_string().chars().next().unwrap_or(' ');
                if text.len() >= width {
                    Ok(Value::String(text))
                } else {
                    let pad = pad_char.to_string().repeat(width - text.len());
                    Ok(Value::String(format!("{}{}", pad, text)))
                }
            }
            "std::padright" => {
                if args.len() != 3 {
                    return Err("std::padright requires a string, width, and padding character".to_string());
                }
                let text = args[0].as_string();
                let width = args[1].as_string().parse::<usize>().map_err(|_| "std::padright requires a numeric width".to_string())?;
                let pad_char = args[2].as_string().chars().next().unwrap_or(' ');
                if text.len() >= width {
                    Ok(Value::String(text))
                } else {
                    let pad = pad_char.to_string().repeat(width - text.len());
                    Ok(Value::String(format!("{}{}", text, pad)))
                }
            }
            "std::repeat" => {
                if args.len() != 2 {
                    return Err("std::repeat requires a string and a count".to_string());
                }
                let text = args[0].as_string();
                let count = args[1].as_string().parse::<usize>().map_err(|_| "std::repeat requires a numeric count".to_string())?;
                Ok(Value::String(text.repeat(count)))
            }
            "std::strip" => {
                if args.len() != 2 {
                    return Err("std::strip requires a string and a strip character set".to_string());
                }
                let text = args[0].as_string();
                let chars = args[1].as_string();
                Ok(Value::String(text.trim_matches(|c| chars.contains(c)).to_string()))
            }
            "std::len" => {
                if args.len() != 1 {
                    return Err("std::len requires one argument".to_string());
                }
                match &args[0] {
                    Value::String(s) => Ok(Value::Number(s.len() as i64)),
                    Value::List(items) => Ok(Value::Number(items.len() as i64)),
                    _ => Err("std::len requires a string or array".to_string()),
                }
            }
            "std::push" => {
                if args.len() != 2 {
                    return Err("std::push requires an array and a value".to_string());
                }
                if let Value::List(mut items) = args[0].clone() {
                    items.push(args[1].as_string());
                    Ok(Value::List(items))
                } else {
                    Err("std::push requires an array as first argument".to_string())
                }
            }
            "std::pop" => {
                if args.len() != 1 {
                    return Err("std::pop requires an array".to_string());
                }
                if let Value::List(mut items) = args[0].clone() {
                    if items.is_empty() {
                        return Err("std::pop requires a non-empty array".to_string());
                    }
                    items.pop();
                    Ok(Value::List(items))
                } else {
                    Err("std::pop requires an array".to_string())
                }
            }
            "std::first" => {
                if args.len() != 1 {
                    return Err("std::first requires an array".to_string());
                }
                if let Value::List(items) = &args[0] {
                    if let Some(first) = items.first() {
                        Ok(Value::String(first.clone()))
                    } else {
                        Err("std::first requires a non-empty array".to_string())
                    }
                } else {
                    Err("std::first requires an array".to_string())
                }
            }
            "std::last" => {
                if args.len() != 1 {
                    return Err("std::last requires an array".to_string());
                }
                if let Value::List(items) = &args[0] {
                    if let Some(last) = items.last() {
                        Ok(Value::String(last.clone()))
                    } else {
                        Err("std::last requires a non-empty array".to_string())
                    }
                } else {
                    Err("std::last requires an array".to_string())
                }
            }
            "std::slice" => {
                if args.len() != 3 {
                    return Err("std::slice requires an array or string, from, and to".to_string());
                }
                let start = args[1].as_string().parse::<i64>().map_err(|_| "std::slice requires numeric from and to indexes".to_string())?;
                let end = args[2].as_string().parse::<i64>().map_err(|_| "std::slice requires numeric from and to indexes".to_string())?;
                match &args[0] {
                    Value::List(items) => {
                        let len = items.len() as i64;
                        let start = if start < 0 { (len + start).max(0) } else { start.min(len) };
                        let end = if end < 0 { (len + end).max(0) } else { end.min(len) };
                        let slice = if start < end {
                            items[start as usize..end as usize].to_vec()
                        } else {
                            Vec::new()
                        };
                        Ok(Value::List(slice))
                    }
                    Value::String(text) => {
                        let chars: Vec<char> = text.chars().collect();
                        let len = chars.len() as i64;
                        let start = if start < 0 { (len + start).max(0) } else { start.min(len) };
                        let end = if end < 0 { (len + end).max(0) } else { end.min(len) };
                        let slice = if start < end {
                            chars[start as usize..end as usize].iter().collect()
                        } else {
                            String::new()
                        };
                        Ok(Value::String(slice))
                    }
                    _ => Err("std::slice requires an array or string as first argument".to_string()),
                }
            }
            "std::reverse" => {
                if args.len() != 1 {
                    return Err("std::reverse requires an array".to_string());
                }
                if let Value::List(mut items) = args[0].clone() {
                    items.reverse();
                    Ok(Value::List(items))
                } else {
                    Err("std::reverse requires an array".to_string())
                }
            }
            "std::join" => {
                if args.len() != 2 {
                    return Err("std::join requires an array and a separator".to_string());
                }
                if let Value::List(items) = &args[0] {
                    Ok(Value::String(items.join(&args[1].as_string())))
                } else {
                    Err("std::join requires an array as first argument".to_string())
                }
            }
            "std::min" => {
                if args.len() != 2 {
                    return Err("std::min requires two numeric arguments".to_string());
                }
                let a = args[0].as_number().ok_or("std::min requires numeric arguments".to_string())?;
                let b = args[1].as_number().ok_or("std::min requires numeric arguments".to_string())?;
                Ok(Value::Number(std::cmp::min(a, b)))
            }
            "std::max" => {
                if args.len() != 2 {
                    return Err("std::max requires two numeric arguments".to_string());
                }
                let a = args[0].as_number().ok_or("std::max requires numeric arguments".to_string())?;
                let b = args[1].as_number().ok_or("std::max requires numeric arguments".to_string())?;
                Ok(Value::Number(std::cmp::max(a, b)))
            }
            "std::clamp" => {
                if args.len() != 3 {
                    return Err("std::clamp requires a value, min, and max".to_string());
                }
                let value = args[0].as_number().ok_or("std::clamp requires numeric arguments".to_string())?;
                let min = args[1].as_number().ok_or("std::clamp requires numeric arguments".to_string())?;
                let max = args[2].as_number().ok_or("std::clamp requires numeric arguments".to_string())?;
                Ok(Value::Number(value.clamp(min, max)))
            }
            "std::even" => {
                if args.len() != 1 {
                    return Err("std::even requires one numeric argument".to_string());
                }
                let value = args[0].as_number().ok_or("std::even requires a numeric argument".to_string())?;
                Ok(Value::Bool(value % 2 == 0))
            }
            "std::odd" => {
                if args.len() != 1 {
                    return Err("std::odd requires one numeric argument".to_string());
                }
                let value = args[0].as_number().ok_or("std::odd requires a numeric argument".to_string())?;
                Ok(Value::Bool(value % 2 != 0))
            }
            "std::basename" => {
                if args.len() != 1 {
                    return Err("std::basename requires one path argument".to_string());
                }
                let path_str = args[0].as_string();
                let path = std::path::Path::new(&path_str);
                Ok(Value::String(path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()))
            }
            "std::dirname" => {
                if args.len() != 1 {
                    return Err("std::dirname requires one path argument".to_string());
                }
                let path_str = args[0].as_string();
                let path = std::path::Path::new(&path_str);
                Ok(Value::String(path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| ".".to_string())))
            }
            "std::joinpath" => {
                if args.len() != 2 {
                    return Err("std::joinpath requires two path arguments".to_string());
                }
                let joined = std::path::Path::new(&args[0].as_string()).join(&args[1].as_string());
                Ok(Value::String(joined.to_string_lossy().into_owned()))
            }
            "std::isfile" => {
                if args.len() != 1 {
                    return Err("std::isfile requires one path argument".to_string());
                }
                Ok(Value::Bool(std::path::Path::new(&args[0].as_string()).is_file()))
            }
            "std::isdir" => {
                if args.len() != 1 {
                    return Err("std::isdir requires one path argument".to_string());
                }
                Ok(Value::Bool(std::path::Path::new(&args[0].as_string()).is_dir()))
            }
            "std::mkdir" => {
                if args.len() != 1 {
                    return Err("std::mkdir requires one path argument".to_string());
                }
                let path = args[0].as_string();
                std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
                Ok(Value::String(String::new()))
            }
            "std::listdir" => {
                if args.len() != 1 {
                    return Err("std::listdir requires one path argument".to_string());
                }
                let path = args[0].as_string();
                let entries = std::fs::read_dir(&path).map_err(|e| e.to_string())?;
                let mut names = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    names.push(name);
                }
                Ok(Value::List(names))
            }
            "std::which" => {
                if args.len() != 1 {
                    return Err("std::which requires one command name".to_string());
                }
                let command = args[0].as_string();
                let candidate = std::path::Path::new(&command);
                if command.contains('/') {
                    if candidate.is_file() {
                        return Ok(Value::String(command));
                    }
                    return Ok(Value::String(String::new()));
                }

                let path_env = std::env::var("PATH").unwrap_or_default();
                for dir in path_env.split(':') {
                    let candidate = std::path::Path::new(dir).join(&command);
                    if candidate.is_file() {
                        return Ok(Value::String(candidate.to_string_lossy().into_owned()));
                    }
                }
                Ok(Value::String(String::new()))
            }
            "std::pid" => {
                if !args.is_empty() {
                    return Err("std::pid does not take arguments".to_string());
                }
                Ok(Value::Number(std::process::id() as i64))
            }
            "std::sleep" => {
                if args.len() != 1 {
                    return Err("std::sleep requires one numeric argument".to_string());
                }
                let ms = args[0].as_number().ok_or("std::sleep requires a numeric argument".to_string())?;
                if ms < 0 {
                    return Err("std::sleep requires a non-negative duration".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(ms as u64));
                Ok(Value::String(String::new()))
            }
            "std::getuser" => {
                if !args.is_empty() {
                    return Err("std::getuser does not take arguments".to_string());
                }
                Ok(Value::String(std::env::var("USER").unwrap_or_else(|_| "unknown".to_string())))
            }
            "std::lines" => {
                if args.len() != 1 {
                    return Err("std::lines requires one string argument".to_string());
                }
                let haystack = args[0].as_string();
                let items = haystack.lines().map(|s| s.to_string()).collect();
                Ok(Value::List(items))
            }
            "std::startswith" => {
                if args.len() != 2 {
                    return Err("std::startswith requires two string arguments".to_string());
                }
                Ok(Value::Bool(args[0].as_string().starts_with(&args[1].as_string())))
            }
            "std::endswith" => {
                if args.len() != 2 {
                    return Err("std::endswith requires two string arguments".to_string());
                }
                Ok(Value::Bool(args[0].as_string().ends_with(&args[1].as_string())))
            }
            "std::sizeof" => {
                if args.len() != 1 {
                    return Err("std::sizeof requires one argument".to_string());
                }
                Ok(Value::Number(args[0].size_in_memory() as i64))
            }
            "std::format_gb" => {
                if args.len() != 1 {
                    return Err("std::format_gb requires one numeric argument".to_string());
                }
                let kb = args[0]
                    .as_string();
                let kb = Self::parse_kb_value(&kb)
                    .ok_or("std::format_gb requires a numeric argument".to_string())?;
                Ok(Value::String(format!("{:.2} GB", kb / 1_000_000.0)))
            }
            "std::format_gib" => {
                if args.len() != 1 {
                    return Err("std::format_gib requires one numeric argument".to_string());
                }
                let kb = args[0]
                    .as_string();
                let kb = Self::parse_kb_value(&kb)
                    .ok_or("std::format_gib requires a numeric argument".to_string())?;
                Ok(Value::String(format!("{:.2} GiB", kb / 1_048_576.0)))
            }
            _ => Err(format!("Unknown std function: {}", name)),
        }
    }

    fn parse_kb_value(input: &str) -> Option<f64> {
        let trimmed = input.trim();
        let trimmed = trimmed.trim_end_matches("kB").trim_end_matches("KB").trim_end_matches("kb").trim();
        if let Ok(num) = trimmed.parse::<f64>() {
            return Some(num);
        }
        let digits: String = trimmed.chars().filter(|c| c.is_digit(10) || *c == '.' || *c == '-').collect();
        digits.parse::<f64>().ok()
    }

    fn strip_quotes(arg: &str) -> String {
        let arg = arg.trim();
        if (arg.starts_with('"') && arg.ends_with('"')) || (arg.starts_with('\'') && arg.ends_with('\'')) {
            Self::unescape_string(&arg[1..arg.len() - 1])
        } else {
            arg.to_string()
        }
    }

    fn unescape_string(value: &str) -> String {
        let mut result = String::new();
        let mut chars = value.chars();

        while let Some(ch) = chars.next() {
            if ch != '\\' {
                result.push(ch);
                continue;
            }

            match chars.next() {
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some('0') => result.push('\0'),
                Some('\\') => result.push('\\'),
                Some('\'') => result.push('\''),
                Some('"') => result.push('"'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        }

        result
    }

    fn eval_expression(&mut self, expr: &str) -> Result<Value, String> {
        let expr = expr.trim();

        if expr.starts_with('"') && expr.ends_with('"') {
            let inner = &expr[1..expr.len() - 1];
            let expanded = self.expand_vars(inner);
            return Ok(Value::String(Self::unescape_string(&expanded)));
        }

        if expr.starts_with('\'') && expr.ends_with('\'') {
            let inner = &expr[1..expr.len() - 1];
            return Ok(Value::String(Self::unescape_string(inner)));
        }

        if expr.starts_with('(') && expr.ends_with(')') {
            let inner = &expr[1..expr.len() - 1];
            let number = self.eval_arithmetic(inner)?;
            return Ok(Value::Number(number));
        }

        if expr.starts_with("chain") {
            return self.eval_chain_expression(expr);
        }

        if let Some(open_brace) = expr.find('{') {
            let type_name = expr[..open_brace].trim();
            if !type_name.is_empty() && expr.ends_with('}') {
                let inner = expr[open_brace + 1..expr.len() - 1].trim();
                return self.parse_struct_literal(type_name, inner);
            }
        }

        if expr.starts_with('{') && expr.ends_with('}') {
            let inner = expr[1..expr.len() - 1].trim();
            let sub_env = self.env.clone();
            let sub_job_manager = Rc::new(RefCell::new(JobManager::new()));
            let sub_registry = Rc::new(BuiltinRegistry::new());
            let mut executor = Executor::new(sub_job_manager.clone(), sub_registry);
            *executor.env() = sub_env;

            let (run_result, output) = Self::capture_output(|| {
                let mut parser = ControlFlowParser::new(inner);
                let statements = parser.parse().map_err(|e| e.to_string())?;
                executor.execute_statements_with_nonzero_error(&statements)
            })?;

            return run_result.map(|_| Value::String(output.trim().to_string()));
        }

        if expr.starts_with('[') && expr.ends_with(']') {
            return self.parse_array(expr);
        }

        if expr == "_bail" {
            std::process::exit(1);
        }

        if let Some((name, args)) = Self::parse_function_call(expr) {
            if name == "_bail" || name == "_exit" {
                Self::handle_exit_builtin(&name, &args)?;
            }
            if name == "write_demo_file" || name == "joinpath" || name == "readfile" {
                eprintln!("DEBUG eval_expression function call: {} args={:?}", name, args);
            }
            if name.starts_with("c::") {
                return self.execute_c_function(&name, &args);
            }

            if let Some(std_name) = self.normalize_std_function_name(&name) {
                return self.call_std_function(&std_name, &args);
            }

            if self.env.get_function(&name).is_some() {
                if name == "write_demo_file" {
                    eprintln!("DEBUG eval_expression found user function: {}", name);
                }
                return self.call_function(&name, &args, false);
            }
        }

        if let Some(std_name) = self.normalize_std_function_name(expr) {
            return self.call_std_function(&std_name, &[]);
        }

        if self.env.get_function(expr).is_some() {
            return self.call_function(expr, &[], false);
        }

        if expr.starts_with('$') {
            let var_name = expr[1..].trim();
            if let Some(value) = self.resolve_value_path(var_name) {
                return Ok(value);
            }
        }

        if let Some(value) = self.resolve_value_path(expr) {
            return Ok(value);
        }

        if let Some(value) = self.env.get(expr) {
            return Ok(value.clone());
        }

        if let Ok(num) = expr.parse::<i64>() {
            return Ok(Value::Number(num));
        }

        if let Ok(float) = expr.parse::<f64>() {
            return Ok(Value::Float(float));
        }

        if expr.contains('.') {
            let mut parts = expr.splitn(2, '.');
            let type_name = parts.next().unwrap().trim();
            let member = parts.next().unwrap().trim();
            if !type_name.is_empty() && !member.is_empty() {
                return Ok(Value::Enum(type_name.to_string(), member.to_string()));
            }
        }

        let expanded = self.expand_vars(expr);
        Ok(Value::String(expanded))
    }

    fn parse_array(&self, expr: &str) -> Result<Value, String> {
        let inner = expr[1..expr.len() - 1].trim();
        let mut items = Vec::new();
        let mut current = String::new();
        let mut in_quote: Option<char> = None;

        for c in inner.chars() {
            if let Some(q) = in_quote {
                if c == q {
                    in_quote = None;
                    current.push(c);
                } else {
                    current.push(c);
                }
            } else if c == '"' || c == '\'' {
                in_quote = Some(c);
                current.push(c);
            } else if c == ',' {
                let item = current.trim();
                if !item.is_empty() {
                    items.push(Self::strip_quotes(item));
                }
                current.clear();
            } else {
                current.push(c);
            }
        }

        let item = current.trim();
        if !item.is_empty() {
            items.push(Self::strip_quotes(item));
        }

        Ok(Value::List(items))
    }

    fn eval_arithmetic(&self, expr: &str) -> Result<i64, String> {
        fn resolve_operand(token: &str, env: &Environment) -> Option<i64> {
            let token = token.trim();
            let token = if token.starts_with('$') { &token[1..] } else { token };

            if let Ok(num) = token.parse::<i64>() {
                Some(num)
            } else if let Some(value) = env.get(token) {
                value.as_number()
            } else if let Some(value) = env.get_path(token) {
                value.as_number()
            } else {
                None
            }
        }

        let tokens: Vec<&str> = expr.split_whitespace().collect();
        if tokens.len() == 1 {
            if let Some(num) = resolve_operand(tokens[0], &self.env) {
                return Ok(num);
            }
        }

        if tokens.len() == 3 {
            let left = resolve_operand(tokens[0], &self.env).ok_or("invalid left operand".to_string())?;
            let right = resolve_operand(tokens[2], &self.env).ok_or("invalid right operand".to_string())?;

            return match tokens[1] {
                "+" => Ok(left + right),
                "-" => Ok(left - right),
                "*" => Ok(left * right),
                "/" => {
                    if right == 0 {
                        Err("division by zero".to_string())
                    } else {
                        Ok(left / right)
                    }
                }
                "%" => {
                    if right == 0 {
                        Err("division by zero".to_string())
                    } else {
                        Ok(left % right)
                    }
                }
                _ => Err("unknown operator".to_string()),
            };
        }

        Err("complex arithmetic not supported".to_string())
    }

    fn resolve_value_path(&self, path: &str) -> Option<Value> {
        self.env.get_path(path)
    }

    fn find_enum_value_by_type(&self, enum_type: &str) -> Result<Value, String> {
        for value in self.env.values() {
            if let Value::Enum(ref type_name, ref variant) = value {
                if type_name == enum_type {
                    return Ok(Value::Enum(type_name.clone(), variant.clone()));
                }
            }
        }
        Err(format!("No enum value found for type {}", enum_type))
    }

    fn parse_struct_literal(&mut self, type_name: &str, inner: &str) -> Result<Value, String> {
        let mut fields: HashMap<String, Value> = HashMap::new();
        let mut chars = inner.chars().peekable();

        while let Some(_) = chars.peek() {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }

            let mut key = String::new();
            while let Some(&ch) = chars.peek() {
                if ch == ':' {
                    break;
                }
                key.push(ch);
                chars.next();
            }
            if key.is_empty() {
                break;
            }

            if chars.next() != Some(':') {
                return Err("Invalid struct literal: expected ':'".to_string());
            }

            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }

            let mut value_text = String::new();
            let mut nested = 0;
            let mut in_quote: Option<char> = None;

            while let Some(&ch) = chars.peek() {
                if let Some(q) = in_quote {
                    value_text.push(ch);
                    chars.next();
                    if ch == q {
                        in_quote = None;
                    }
                    continue;
                }

                if ch == '"' || ch == '\'' {
                    in_quote = Some(ch);
                    value_text.push(ch);
                    chars.next();
                    continue;
                }

                if ch == '(' || ch == '{' || ch == '[' {
                    nested += 1;
                } else if ch == ')' || ch == '}' || ch == ']' {
                    if nested > 0 {
                        nested -= 1;
                    }
                }

                if nested == 0 && ch.is_whitespace() {
                    let lookahead: String = chars.clone().collect();
                    let lookahead = lookahead.trim_start();
                    if let Some(idx) = lookahead.find(':') {
                        let possible_key = lookahead[..idx].trim();
                        if !possible_key.is_empty() && possible_key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                            break;
                        }
                    }
                }

                value_text.push(ch);
                chars.next();
            }

            let value_expr = value_text.trim();
            let value = self.eval_expression(value_expr)?;
            fields.insert(key.trim().to_string(), value);
        }

        Ok(Value::Struct(type_name.to_string(), fields))
    }

    fn parse_single_statement(&self, line: &str) -> Result<Option<Statement>, String> {
        let mut parser = ControlFlowParser::new(line);
        let statements = parser.parse()?;
        Ok(statements.into_iter().next())
    }

    fn call_function(&mut self, name: &str, args: &[String], treat_nonzero_as_error: bool) -> Result<Value, String> {
        if let Some(function_def) = self.env.get_function(name).cloned() {
            let mut saved_args: Vec<(String, Option<Value>)> = Vec::new();
            for param in &function_def.params {
                saved_args.push((param.clone(), self.env.get(param).cloned()));
            }

            for (idx, param) in function_def.params.iter().enumerate() {
                let value = args
                    .get(idx)
                    .map(|arg| Value::String(arg.clone()))
                    .unwrap_or_else(|| Value::String(String::new()));
                self.env.set(param.clone(), value);
            }

            let result = self.execute_statements_internal(&function_def.body, treat_nonzero_as_error);

            for (name, original_value) in saved_args {
                if let Some(value) = original_value {
                    self.env.set(name, value);
                } else {
                    self.env.remove(&name);
                }
            }

            return match result? {
                ExecutionControl::Normal(_) => Ok(Value::String(String::new())),
                ExecutionControl::Break => Ok(Value::String(String::new())),
                ExecutionControl::Continue => Err("CONTINUE outside of loop".to_string()),
                ExecutionControl::Return(value) => Ok(value),
            };
        }

        Err(format!("Function not found: {}", name))
    }

    fn execute_c_function(&mut self, name: &str, args: &[String]) -> Result<Value, String> {
        let evaluated_args = self.evaluate_c_function_args(args)?;
        if let Some(func) = Self::lookup_c_function(name) {
            func(self, &evaluated_args)
        } else if name.starts_with("c::") {
            self.c_generic(name, &evaluated_args)
        } else {
            Err(format!("Unknown C function: {}", name))
        }
    }

    fn evaluate_c_function_args(&mut self, args: &[String]) -> Result<Vec<Value>, String> {
        args.iter().map(|arg| self.eval_expression(arg)).collect()
    }

    fn lookup_c_function(name: &str) -> Option<fn(&mut Executor, &[Value]) -> Result<Value, String>> {
        for (key, func) in Self::c_function_table() {
            if *key == name {
                return Some(*func);
            }
        }
        None
    }

    fn c_generic(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        let symbol_name = name.strip_prefix("c::").unwrap_or(name);
        let (raw_args, _strings) = Self::c_args_to_raw(args)?;

        let symbol_ptr = unsafe { Self::load_c_symbol(symbol_name)? };
        let result = unsafe {
            match raw_args.len() {
                0 => {
                    let func: extern "C" fn() -> isize = std::mem::transmute(symbol_ptr);
                    func()
                }
                1 => {
                    let func: extern "C" fn(usize) -> isize = std::mem::transmute(symbol_ptr);
                    func(raw_args[0])
                }
                2 => {
                    let func: extern "C" fn(usize, usize) -> isize = std::mem::transmute(symbol_ptr);
                    func(raw_args[0], raw_args[1])
                }
                3 => {
                    let func: extern "C" fn(usize, usize, usize) -> isize = std::mem::transmute(symbol_ptr);
                    func(raw_args[0], raw_args[1], raw_args[2])
                }
                4 => {
                    let func: extern "C" fn(usize, usize, usize, usize) -> isize = std::mem::transmute(symbol_ptr);
                    func(raw_args[0], raw_args[1], raw_args[2], raw_args[3])
                }
                5 => {
                    let func: extern "C" fn(usize, usize, usize, usize, usize) -> isize = std::mem::transmute(symbol_ptr);
                    func(raw_args[0], raw_args[1], raw_args[2], raw_args[3], raw_args[4])
                }
                6 => {
                    let func: extern "C" fn(usize, usize, usize, usize, usize, usize) -> isize = std::mem::transmute(symbol_ptr);
                    func(raw_args[0], raw_args[1], raw_args[2], raw_args[3], raw_args[4], raw_args[5])
                }
                _ => return Err("Generic c:: calls support up to 6 arguments".to_string()),
            }
        };

        Ok(Value::Number(result as i64))
    }

    fn c_args_to_raw(args: &[Value]) -> Result<(Vec<usize>, Vec<CString>), String> {
        let mut raw_args = Vec::new();
        let mut cstrings = Vec::new();

        for arg in args {
            match arg {
                Value::String(value) => {
                    let c_string = CString::new(value.clone()).map_err(|_| "C string argument contains null byte".to_string())?;
                    raw_args.push(c_string.as_ptr() as usize);
                    cstrings.push(c_string);
                }
                Value::Number(value) => raw_args.push(*value as usize),
                Value::Float(value) => raw_args.push(*value as usize),
                Value::Bool(value) => raw_args.push(if *value { 1 } else { 0 }),
                _ => return Err("Unsupported argument type for generic c:: call".to_string()),
            }
        }

        Ok((raw_args, cstrings))
    }

    fn c_function_table() -> &'static [(&'static str, fn(&mut Executor, &[Value]) -> Result<Value, String>)] {
        &[
            ("c::puts", Self::c_puts),
            ("c::printf", Self::c_printf),
            ("c::fprintf", Self::c_fprintf),
            ("c::putchar", Self::c_putchar),
            ("c::getenv", Self::c_getenv),
            ("c::strlen", Self::c_strlen),
            ("c::atoi", Self::c_atoi),
            ("c::atol", Self::c_atol),
            ("c::atof", Self::c_atof),
            ("c::abs", Self::c_abs),
            ("c::srand", Self::c_srand),
            ("c::rand", Self::c_rand),
            ("c::system", Self::c_system),
            ("c::strcmp", Self::c_strcmp),
            ("c::strncmp", Self::c_strncmp),
            ("c::strchr", Self::c_strchr),
            ("c::strstr", Self::c_strstr),
            ("c::toupper", Self::c_toupper),
            ("c::tolower", Self::c_tolower),
            ("c::sqrt", Self::c_sqrt),
            ("c::pow", Self::c_pow),
            ("c::log", Self::c_log),
            ("c::log2", Self::c_log2),
            ("c::sin", Self::c_sin),
            ("c::cos", Self::c_cos),
            ("c::floor", Self::c_floor),
            ("c::ceil", Self::c_ceil),
            ("c::round", Self::c_round),
            ("c::fabs", Self::c_fabs),
            ("c::time", Self::c_time),
            ("c::clock", Self::c_clock),
            ("c::isdigit", Self::c_isdigit),
            ("c::isalpha", Self::c_isalpha),
            ("c::isspace", Self::c_isspace),
            ("c::isupper", Self::c_isupper),
            ("c::islower", Self::c_islower),
            ("c::isalnum", Self::c_isalnum),
            ("c::memcpy", Self::c_memcpy),
            ("c::memmove", Self::c_memmove),
            ("c::memcmp", Self::c_memcmp),
            ("c::memset", Self::c_memset),
            ("c::malloc", Self::c_malloc),
            ("c::calloc", Self::c_calloc),
            ("c::realloc", Self::c_realloc),
            ("c::free", Self::c_free),
            ("c::remove", Self::c_remove),
            ("c::fopen", Self::c_fopen),
            ("c::fclose", Self::c_fclose),
            ("c::fgets", Self::c_fgets),
            ("c::fputs", Self::c_fputs),
            ("c::fflush", Self::c_fflush),
            ("c::perror", Self::c_perror),
            ("c::dlopen", Self::c_dlopen),
            ("c::dlsym", Self::c_dlsym),
            ("c::dlerror", Self::c_dlerror),
        ]
    }

    fn dlerror_message() -> String {
        unsafe {
            let err = libc::dlerror();
            if err.is_null() {
                "Unknown dlerror".to_string()
            } else {
                CStr::from_ptr(err).to_string_lossy().into_owned()
            }
        }
    }

    fn value_to_ptr(value: &Value) -> Result<*mut libc::c_void, String> {
        let num = value
            .as_number()
            .ok_or("C pointer arguments must be numeric handles".to_string())?;
        Ok(num as usize as *mut libc::c_void)
    }

    fn ptr_to_value(ptr: *mut libc::c_void) -> Value {
        Value::Number(ptr as isize as i64)
    }

    fn c_string_from_value(value: &Value, arg_name: &str) -> Result<CString, String> {
        CString::new(value.as_string()).map_err(|_| format!("{} contains null byte", arg_name))
    }

    fn default_dynamic_library_handle() -> Result<*mut libc::c_void, String> {
        static LIBRARY_HANDLE: OnceLock<Result<usize, String>> = OnceLock::new();
        let handle_entry = LIBRARY_HANDLE.get_or_init(|| {
            let dl = unsafe { libc::dlopen(ptr::null(), libc::RTLD_LAZY) };
            if dl.is_null() {
                Err(Self::dlerror_message())
            } else {
                Ok(dl as usize)
            }
        });

        match handle_entry {
            Ok(handle) => Ok(*handle as *mut libc::c_void),
            Err(err) => Err(err.clone()),
        }
    }

    unsafe fn load_c_symbol(name: &str) -> Result<*mut libc::c_void, String> {
        let symbol = CString::new(name).map_err(|_| "Symbol name contains null byte".to_string())?;
        let handle = Self::default_dynamic_library_handle()?;
        let symbol_ptr = libc::dlsym(handle, symbol.as_ptr());
        if !symbol_ptr.is_null() {
            return Ok(symbol_ptr);
        }

        // Some math functions live in libm rather than the main executable/libc,
        // so try a libm fallback for symbols like sqrt, pow, sin, cos, etc.
        for lib_name in ["libm.so.6", "libm.so"] {
            let lib_cstr = CString::new(lib_name).unwrap();
            let lib_handle = libc::dlopen(lib_cstr.as_ptr(), libc::RTLD_LAZY);
            if lib_handle.is_null() {
                continue;
            }
            let symbol_ptr = libc::dlsym(lib_handle, symbol.as_ptr());
            if !symbol_ptr.is_null() {
                return Ok(symbol_ptr);
            }
        }

        Err(Self::dlerror_message())
    }

    fn c_dlopen(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let path = CString::new(arg.as_string()).map_err(|_| "c::dlopen argument contains null byte".to_string())?;
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) };
            if handle.is_null() {
                Err(Self::dlerror_message())
            } else {
                Ok(Self::ptr_to_value(handle))
            }
        } else {
            Err("c::dlopen requires a library path".to_string())
        }
    }

    fn c_dlsym(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::dlsym requires a handle and a symbol name".to_string());
        }

        let handle = Self::value_to_ptr(&args[0])?;
        let symbol_name = CString::new(args[1].as_string()).map_err(|_| "c::dlsym symbol argument contains null byte".to_string())?;
        let symbol_ptr = unsafe { libc::dlsym(handle, symbol_name.as_ptr()) };
        if symbol_ptr.is_null() {
            Err(Self::dlerror_message())
        } else {
            Ok(Self::ptr_to_value(symbol_ptr))
        }
    }

    fn c_dlerror(&mut self, _args: &[Value]) -> Result<Value, String> {
        Ok(Value::String(Self::dlerror_message()))
    }

    fn c_malloc(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(size_arg) = args.get(0) {
            let size = size_arg.as_number().ok_or("c::malloc size must be a number".to_string())? as libc::size_t;
            let malloc_ptr = unsafe { Self::load_c_symbol("malloc")? };
            let malloc_fn: unsafe extern "C" fn(libc::size_t) -> *mut libc::c_void = unsafe { std::mem::transmute(malloc_ptr) };
            let ptr = unsafe { malloc_fn(size) };
            Ok(Self::ptr_to_value(ptr))
        } else {
            Err("c::malloc requires a size".to_string())
        }
    }

    fn c_calloc(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::calloc requires count and size arguments".to_string());
        }

        let nmemb = args[0].as_number().ok_or("c::calloc count must be a number".to_string())? as libc::size_t;
        let size = args[1].as_number().ok_or("c::calloc size must be a number".to_string())? as libc::size_t;
        let calloc_ptr = unsafe { Self::load_c_symbol("calloc")? };
        let calloc_fn: unsafe extern "C" fn(libc::size_t, libc::size_t) -> *mut libc::c_void = unsafe { std::mem::transmute(calloc_ptr) };
        let ptr = unsafe { calloc_fn(nmemb, size) };
        Ok(Self::ptr_to_value(ptr))
    }

    fn c_realloc(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::realloc requires a pointer and a size".to_string());
        }

        let ptr = Self::value_to_ptr(&args[0])?;
        let size = args[1].as_number().ok_or("c::realloc size must be a number".to_string())? as libc::size_t;
        let realloc_ptr = unsafe { Self::load_c_symbol("realloc")? };
        let realloc_fn: unsafe extern "C" fn(*mut libc::c_void, libc::size_t) -> *mut libc::c_void = unsafe { std::mem::transmute(realloc_ptr) };
        let new_ptr = unsafe { realloc_fn(ptr, size) };
        Ok(Self::ptr_to_value(new_ptr))
    }

    fn c_free(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(ptr_arg) = args.get(0) {
            let ptr = Self::value_to_ptr(ptr_arg)?;
            let free_ptr = unsafe { Self::load_c_symbol("free")? };
            let free_fn: unsafe extern "C" fn(*mut libc::c_void) = unsafe { std::mem::transmute(free_ptr) };
            unsafe { free_fn(ptr) };
            Ok(Value::Number(0))
        } else {
            Err("c::free requires a pointer".to_string())
        }
    }

    fn c_atoi(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let c_string = Self::c_string_from_value(arg, "c::atoi argument")?;
            let atoi_ptr = unsafe { Self::load_c_symbol("atoi")? };
            let atoi_fn: unsafe extern "C" fn(*const libc::c_char) -> libc::c_int = unsafe { std::mem::transmute(atoi_ptr) };
            let result = unsafe { atoi_fn(c_string.as_ptr()) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::atoi requires one argument".to_string())
        }
    }

    fn c_atol(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let c_string = Self::c_string_from_value(arg, "c::atol argument")?;
            let atol_ptr = unsafe { Self::load_c_symbol("atol")? };
            let atol_fn: unsafe extern "C" fn(*const libc::c_char) -> libc::c_long = unsafe { std::mem::transmute(atol_ptr) };
            let result = unsafe { atol_fn(c_string.as_ptr()) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::atol requires one argument".to_string())
        }
    }

    fn c_atof(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let c_string = Self::c_string_from_value(arg, "c::atof argument")?;
            let atof_ptr = unsafe { Self::load_c_symbol("atof")? };
            let atof_fn: unsafe extern "C" fn(*const libc::c_char) -> libc::c_double = unsafe { std::mem::transmute(atof_ptr) };
            let result = unsafe { atof_fn(c_string.as_ptr()) };
            Ok(Value::Float(result))
        } else {
            Err("c::atof requires one argument".to_string())
        }
    }

    fn c_abs(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_number().ok_or("c::abs requires a numeric argument".to_string())? as libc::c_int;
            let abs_ptr = unsafe { Self::load_c_symbol("abs")? };
            let abs_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(abs_ptr) };
            let result = unsafe { abs_fn(value) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::abs requires one argument".to_string())
        }
    }

    fn c_srand(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let seed = arg.as_number().ok_or("c::srand requires a numeric seed".to_string())? as libc::c_uint;
            let srand_ptr = unsafe { Self::load_c_symbol("srand")? };
            let srand_fn: unsafe extern "C" fn(libc::c_uint) = unsafe { std::mem::transmute(srand_ptr) };
            unsafe { srand_fn(seed) };
            Ok(Value::Number(0))
        } else {
            Err("c::srand requires one argument".to_string())
        }
    }

    fn c_rand(&mut self, _args: &[Value]) -> Result<Value, String> {
        let rand_ptr = unsafe { Self::load_c_symbol("rand")? };
        let rand_fn: unsafe extern "C" fn() -> libc::c_int = unsafe { std::mem::transmute(rand_ptr) };
        let result = unsafe { rand_fn() };
        Ok(Value::Number(result as i64))
    }

    fn c_strchr(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::strchr requires a string and a character code".to_string());
        }

        let string = Self::c_string_from_value(&args[0], "c::strchr string")?;
        let ch = args[1].as_number().ok_or("c::strchr second argument must be a numeric character code".to_string())? as libc::c_int;
        let strchr_ptr = unsafe { Self::load_c_symbol("strchr")? };
        let strchr_fn: unsafe extern "C" fn(*const libc::c_char, libc::c_int) -> *mut libc::c_char = unsafe { std::mem::transmute(strchr_ptr) };
        let result = unsafe { strchr_fn(string.as_ptr(), ch) };
        if result.is_null() {
            Ok(Value::String(String::new()))
        } else {
            let cstr = unsafe { CStr::from_ptr(result) };
            Ok(Value::String(cstr.to_string_lossy().into_owned()))
        }
    }

    fn c_strstr(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::strstr requires a haystack and needle".to_string());
        }

        let haystack = Self::c_string_from_value(&args[0], "c::strstr haystack")?;
        let needle = Self::c_string_from_value(&args[1], "c::strstr needle")?;
        let strstr_ptr = unsafe { Self::load_c_symbol("strstr")? };
        let strstr_fn: unsafe extern "C" fn(*const libc::c_char, *const libc::c_char) -> *mut libc::c_char = unsafe { std::mem::transmute(strstr_ptr) };
        let result = unsafe { strstr_fn(haystack.as_ptr(), needle.as_ptr()) };
        if result.is_null() {
            Ok(Value::String(String::new()))
        } else {
            let cstr = unsafe { CStr::from_ptr(result) };
            Ok(Value::String(cstr.to_string_lossy().into_owned()))
        }
    }

    fn c_toupper(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let ch = arg.as_number().ok_or("c::toupper requires a numeric character code".to_string())? as libc::c_int;
            let toupper_ptr = unsafe { Self::load_c_symbol("toupper")? };
            let toupper_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(toupper_ptr) };
            let result = unsafe { toupper_fn(ch) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::toupper requires one argument".to_string())
        }
    }

    fn c_tolower(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let ch = arg.as_number().ok_or("c::tolower requires a numeric character code".to_string())? as libc::c_int;
            let tolower_ptr = unsafe { Self::load_c_symbol("tolower")? };
            let tolower_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(tolower_ptr) };
            let result = unsafe { tolower_fn(ch) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::tolower requires one argument".to_string())
        }
    }

    fn c_sqrt(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::sqrt requires a numeric argument".to_string())?;
            let sqrt_ptr = unsafe { Self::load_c_symbol("sqrt")? };
            let sqrt_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(sqrt_ptr) };
            let result = unsafe { sqrt_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::sqrt requires one argument".to_string())
        }
    }

    fn c_pow(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::pow requires two numeric arguments".to_string());
        }

        let base = args[0].as_float().ok_or("c::pow first argument must be numeric".to_string())?;
        let exp = args[1].as_float().ok_or("c::pow second argument must be numeric".to_string())?;
        let pow_ptr = unsafe { Self::load_c_symbol("pow")? };
        let pow_fn: unsafe extern "C" fn(libc::c_double, libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(pow_ptr) };
        let result = unsafe { pow_fn(base, exp) };
        Ok(Value::Float(result))
    }

    fn c_log(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::log requires a numeric argument".to_string())?;
            let log_ptr = unsafe { Self::load_c_symbol("log")? };
            let log_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(log_ptr) };
            let result = unsafe { log_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::log requires one argument".to_string())
        }
    }

    fn c_log2(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::log2 requires a numeric argument".to_string())?;
            let log2_ptr = unsafe { Self::load_c_symbol("log2")? };
            let log2_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(log2_ptr) };
            let result = unsafe { log2_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::log2 requires one argument".to_string())
        }
    }

    fn c_sin(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::sin requires a numeric argument".to_string())?;
            let sin_ptr = unsafe { Self::load_c_symbol("sin")? };
            let sin_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(sin_ptr) };
            let result = unsafe { sin_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::sin requires one argument".to_string())
        }
    }

    fn c_cos(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::cos requires a numeric argument".to_string())?;
            let cos_ptr = unsafe { Self::load_c_symbol("cos")? };
            let cos_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(cos_ptr) };
            let result = unsafe { cos_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::cos requires one argument".to_string())
        }
    }

    fn c_floor(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::floor requires a numeric argument".to_string())?;
            let floor_ptr = unsafe { Self::load_c_symbol("floor")? };
            let floor_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(floor_ptr) };
            let result = unsafe { floor_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::floor requires one argument".to_string())
        }
    }

    fn c_ceil(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::ceil requires a numeric argument".to_string())?;
            let ceil_ptr = unsafe { Self::load_c_symbol("ceil")? };
            let ceil_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(ceil_ptr) };
            let result = unsafe { ceil_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::ceil requires one argument".to_string())
        }
    }

    fn c_round(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::round requires a numeric argument".to_string())?;
            let round_ptr = unsafe { Self::load_c_symbol("round")? };
            let round_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(round_ptr) };
            let result = unsafe { round_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::round requires one argument".to_string())
        }
    }

    fn c_fabs(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_float().ok_or("c::fabs requires a numeric argument".to_string())?;
            let fabs_ptr = unsafe { Self::load_c_symbol("fabs")? };
            let fabs_fn: unsafe extern "C" fn(libc::c_double) -> libc::c_double = unsafe { std::mem::transmute(fabs_ptr) };
            let result = unsafe { fabs_fn(value) };
            Ok(Value::Float(result))
        } else {
            Err("c::fabs requires one argument".to_string())
        }
    }

    fn c_time(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let time_ptr = if arg.as_number().ok_or("c::time argument must be numeric".to_string())? as i64 == 0 {
                ptr::null_mut()
            } else {
                arg.as_number().map(|v| v as usize as *mut libc::time_t).unwrap_or(ptr::null_mut())
            };
            let time_ptr_fn = unsafe { Self::load_c_symbol("time")? };
            let time_fn: unsafe extern "C" fn(*mut libc::time_t) -> libc::time_t = unsafe { std::mem::transmute(time_ptr_fn) };
            let result = unsafe { time_fn(time_ptr) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::time requires one argument".to_string())
        }
    }

    fn c_clock(&mut self, _args: &[Value]) -> Result<Value, String> {
        let clock_ptr = unsafe { Self::load_c_symbol("clock")? };
        let clock_fn: unsafe extern "C" fn() -> libc::clock_t = unsafe { std::mem::transmute(clock_ptr) };
        let result = unsafe { clock_fn() };
        Ok(Value::Number(result as i64))
    }

    fn c_isdigit(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let ch = arg.as_number().ok_or("c::isdigit requires a numeric character code".to_string())? as libc::c_int;
            let isdigit_ptr = unsafe { Self::load_c_symbol("isdigit")? };
            let isdigit_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(isdigit_ptr) };
            let result = unsafe { isdigit_fn(ch) };
            Ok(Value::Number(if result != 0 { 1 } else { 0 }))
        } else {
            Err("c::isdigit requires one argument".to_string())
        }
    }

    fn c_isalpha(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let ch = arg.as_number().ok_or("c::isalpha requires a numeric character code".to_string())? as libc::c_int;
            let isalpha_ptr = unsafe { Self::load_c_symbol("isalpha")? };
            let isalpha_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(isalpha_ptr) };
            let result = unsafe { isalpha_fn(ch) };
            Ok(Value::Number(if result != 0 { 1 } else { 0 }))
        } else {
            Err("c::isalpha requires one argument".to_string())
        }
    }

    fn c_isspace(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let ch = arg.as_number().ok_or("c::isspace requires a numeric character code".to_string())? as libc::c_int;
            let isspace_ptr = unsafe { Self::load_c_symbol("isspace")? };
            let isspace_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(isspace_ptr) };
            let result = unsafe { isspace_fn(ch) };
            Ok(Value::Number(if result != 0 { 1 } else { 0 }))
        } else {
            Err("c::isspace requires one argument".to_string())
        }
    }

    fn c_isupper(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let ch = arg.as_number().ok_or("c::isupper requires a numeric character code".to_string())? as libc::c_int;
            let isupper_ptr = unsafe { Self::load_c_symbol("isupper")? };
            let isupper_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(isupper_ptr) };
            let result = unsafe { isupper_fn(ch) };
            Ok(Value::Number(if result != 0 { 1 } else { 0 }))
        } else {
            Err("c::isupper requires one argument".to_string())
        }
    }

    fn c_islower(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let ch = arg.as_number().ok_or("c::islower requires a numeric character code".to_string())? as libc::c_int;
            let islower_ptr = unsafe { Self::load_c_symbol("islower")? };
            let islower_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(islower_ptr) };
            let result = unsafe { islower_fn(ch) };
            Ok(Value::Number(if result != 0 { 1 } else { 0 }))
        } else {
            Err("c::islower requires one argument".to_string())
        }
    }

    fn c_isalnum(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let ch = arg.as_number().ok_or("c::isalnum requires a numeric character code".to_string())? as libc::c_int;
            let isalnum_ptr = unsafe { Self::load_c_symbol("isalnum")? };
            let isalnum_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(isalnum_ptr) };
            let result = unsafe { isalnum_fn(ch) };
            Ok(Value::Number(if result != 0 { 1 } else { 0 }))
        } else {
            Err("c::isalnum requires one argument".to_string())
        }
    }

    fn c_system(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let c_string = Self::c_string_from_value(arg, "c::system argument")?;
            let system_ptr = unsafe { Self::load_c_symbol("system")? };
            let system_fn: unsafe extern "C" fn(*const libc::c_char) -> libc::c_int = unsafe { std::mem::transmute(system_ptr) };
            let result = unsafe { system_fn(c_string.as_ptr()) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::system requires one argument".to_string())
        }
    }

    fn c_strcmp(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::strcmp requires two string arguments".to_string());
        }

        let left = Self::c_string_from_value(&args[0], "c::strcmp first argument")?;
        let right = Self::c_string_from_value(&args[1], "c::strcmp second argument")?;
        let strcmp_ptr = unsafe { Self::load_c_symbol("strcmp")? };
        let strcmp_fn: unsafe extern "C" fn(*const libc::c_char, *const libc::c_char) -> libc::c_int = unsafe { std::mem::transmute(strcmp_ptr) };
        let result = unsafe { strcmp_fn(left.as_ptr(), right.as_ptr()) };
        Ok(Value::Number(result as i64))
    }

    fn c_strncmp(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 3 {
            return Err("c::strncmp requires two strings and a count".to_string());
        }

        let left = Self::c_string_from_value(&args[0], "c::strncmp first argument")?;
        let right = Self::c_string_from_value(&args[1], "c::strncmp second argument")?;
        let count = args[2].as_number().ok_or("c::strncmp count must be a number".to_string())? as libc::size_t;
        let strncmp_ptr = unsafe { Self::load_c_symbol("strncmp")? };
        let strncmp_fn: unsafe extern "C" fn(*const libc::c_char, *const libc::c_char, libc::size_t) -> libc::c_int = unsafe { std::mem::transmute(strncmp_ptr) };
        let result = unsafe { strncmp_fn(left.as_ptr(), right.as_ptr(), count) };
        Ok(Value::Number(result as i64))
    }

    fn c_memcpy(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 3 {
            return Err("c::memcpy requires dest, src, and count".to_string());
        }

        let mut dest = args[0].as_string().into_bytes();
        let src = args[1].as_string().into_bytes();
        let count = args[2].as_number().ok_or("c::memcpy third argument must be a number".to_string())? as usize;
        let copy_len = count.min(src.len()).min(dest.len());
        let memcpy_ptr = unsafe { Self::load_c_symbol("memcpy")? };
        let memcpy_fn: unsafe extern "C" fn(*mut libc::c_void, *const libc::c_void, libc::size_t) -> *mut libc::c_void = unsafe { std::mem::transmute(memcpy_ptr) };
        unsafe { memcpy_fn(dest.as_mut_ptr() as *mut libc::c_void, src.as_ptr() as *const libc::c_void, copy_len) };
        Ok(Value::String(String::from_utf8_lossy(&dest).into_owned()))
    }

    fn c_memcmp(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 3 {
            return Err("c::memcmp requires two buffers and a count".to_string());
        }

        let left = args[0].as_string().into_bytes();
        let right = args[1].as_string().into_bytes();
        let count = args[2].as_number().ok_or("c::memcmp third argument must be a number".to_string())? as usize;
        let compare_len = count.min(left.len()).min(right.len());
        let memcmp_ptr = unsafe { Self::load_c_symbol("memcmp")? };
        let memcmp_fn: unsafe extern "C" fn(*const libc::c_void, *const libc::c_void, libc::size_t) -> libc::c_int = unsafe { std::mem::transmute(memcmp_ptr) };
        let result = unsafe { memcmp_fn(left.as_ptr() as *const libc::c_void, right.as_ptr() as *const libc::c_void, compare_len) };
        Ok(Value::Number(result as i64))
    }

    fn c_memset(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 3 {
            return Err("c::memset requires buffer, value, and count".to_string());
        }

        let mut buffer = args[0].as_string().into_bytes();
        let value = args[1].as_number().ok_or("c::memset value must be a number".to_string())? as i32;
        let count = args[2].as_number().ok_or("c::memset third argument must be a number".to_string())? as usize;
        let fill_len = count.min(buffer.len());
        let memset_ptr = unsafe { Self::load_c_symbol("memset")? };
        let memset_fn: unsafe extern "C" fn(*mut libc::c_void, libc::c_int, libc::size_t) -> *mut libc::c_void = unsafe { std::mem::transmute(memset_ptr) };
        unsafe { memset_fn(buffer.as_mut_ptr() as *mut libc::c_void, value, fill_len) };
        Ok(Value::String(String::from_utf8_lossy(&buffer).into_owned()))
    }

    fn c_fopen(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::fopen requires a path and mode".to_string());
        }

        let path = Self::c_string_from_value(&args[0], "c::fopen path")?;
        let mode = Self::c_string_from_value(&args[1], "c::fopen mode")?;
        let fopen_ptr = unsafe { Self::load_c_symbol("fopen")? };
        let fopen_fn: unsafe extern "C" fn(*const libc::c_char, *const libc::c_char) -> *mut libc::FILE = unsafe { std::mem::transmute(fopen_ptr) };
        let file = unsafe { fopen_fn(path.as_ptr(), mode.as_ptr()) };
        Ok(Self::ptr_to_value(file as *mut libc::c_void))
    }

    fn c_fclose(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let file = Self::value_to_ptr(arg)? as *mut libc::FILE;
            let fclose_ptr = unsafe { Self::load_c_symbol("fclose")? };
            let fclose_fn: unsafe extern "C" fn(*mut libc::FILE) -> libc::c_int = unsafe { std::mem::transmute(fclose_ptr) };
            let result = unsafe { fclose_fn(file) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::fclose requires a file handle".to_string())
        }
    }

    fn c_fputs(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("c::fputs requires a string and a file handle".to_string());
        }

        let string = Self::c_string_from_value(&args[0], "c::fputs string")?;
        let file = Self::value_to_ptr(&args[1])? as *mut libc::FILE;
        let fputs_ptr = unsafe { Self::load_c_symbol("fputs")? };
        let fputs_fn: unsafe extern "C" fn(*const libc::c_char, *mut libc::FILE) -> libc::c_int = unsafe { std::mem::transmute(fputs_ptr) };
        let result = unsafe { fputs_fn(string.as_ptr(), file) };
        Ok(Value::Number(result as i64))
    }

    fn c_fflush(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let file = Self::value_to_ptr(arg)? as *mut libc::FILE;
            let fflush_ptr = unsafe { Self::load_c_symbol("fflush")? };
            let fflush_fn: unsafe extern "C" fn(*mut libc::FILE) -> libc::c_int = unsafe { std::mem::transmute(fflush_ptr) };
            let result = unsafe { fflush_fn(file) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::fflush requires a file handle".to_string())
        }
    }

    fn c_perror(&mut self, args: &[Value]) -> Result<Value, String> {
        let message = if let Some(arg) = args.get(0) {
            Self::c_string_from_value(arg, "c::perror message")?
        } else {
            CString::new("").unwrap()
        };

        let perror_ptr = unsafe { Self::load_c_symbol("perror")? };
        let perror_fn: unsafe extern "C" fn(*const libc::c_char) = unsafe { std::mem::transmute(perror_ptr) };
        unsafe { perror_fn(message.as_ptr()) };
        Ok(Value::Number(0))
    }

    fn c_fprintf(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("c::fprintf requires at least a file descriptor and a format string".to_string());
        }

        let fd = args[0].as_number().ok_or("c::fprintf first argument must be a numeric file descriptor".to_string())? as libc::c_int;
        let format = Self::c_string_from_value(&args[1], "c::fprintf format")?;
        let mode = CString::new("w").map_err(|_| "c::fprintf mode contains null byte".to_string())?;

        let fdopen_ptr = unsafe { Self::load_c_symbol("fdopen")? };
        let fdopen_fn: unsafe extern "C" fn(libc::c_int, *const libc::c_char) -> *mut libc::FILE = unsafe { std::mem::transmute(fdopen_ptr) };
        let stream = unsafe { fdopen_fn(fd, mode.as_ptr()) };
        if stream.is_null() {
            return Err("c::fprintf failed to open file descriptor".to_string());
        }

        let fprintf_ptr = unsafe { Self::load_c_symbol("fprintf")? };
        let fprintf_fn: unsafe extern "C" fn(*mut libc::FILE, *const libc::c_char, ...) -> libc::c_int = unsafe { std::mem::transmute(fprintf_ptr) };
        let result = unsafe { fprintf_fn(stream, format.as_ptr()) };
        unsafe { libc::fflush(stream) };
        Ok(Value::Number(result as i64))
    }

    fn c_putchar(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let value = arg.as_number().ok_or("c::putchar requires a numeric argument".to_string())? as libc::c_int;
            let putchar_ptr = unsafe { Self::load_c_symbol("putchar")? };
            let putchar_fn: unsafe extern "C" fn(libc::c_int) -> libc::c_int = unsafe { std::mem::transmute(putchar_ptr) };
            let result = unsafe { putchar_fn(value) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::putchar requires one argument".to_string())
        }
    }

    fn c_remove(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let path = Self::c_string_from_value(arg, "c::remove path")?;
            let remove_ptr = unsafe { Self::load_c_symbol("remove")? };
            let remove_fn: unsafe extern "C" fn(*const libc::c_char) -> libc::c_int = unsafe { std::mem::transmute(remove_ptr) };
            let result = unsafe { remove_fn(path.as_ptr()) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::remove requires a path".to_string())
        }
    }

    fn c_puts(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let c_string = CString::new(arg.as_string()).map_err(|_| "c::puts argument contains null byte".to_string())?;
            let puts_ptr = unsafe { Self::load_c_symbol("puts")? };
            let puts_fn: unsafe extern "C" fn(*const libc::c_char) -> libc::c_int = unsafe { std::mem::transmute(puts_ptr) };
            let result = unsafe { puts_fn(c_string.as_ptr()) };
            Ok(Value::Number(result as i64))
        } else {
            Err("c::puts requires one argument".to_string())
        }
    }

    fn c_printf(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.is_empty() {
            return Err("c::printf requires at least a format string".to_string());
        }

        let format_str = Self::unescape_string(&args[0].as_string());
        let mut formatted = String::new();
        let mut arg_index = 1;
        let mut chars = format_str.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '%' {
                match chars.next() {
                    Some('%') => formatted.push('%'),
                    Some(first) => {
                        let mut spec_token = first.to_string();
                        while let Some(&next_ch) = chars.peek() {
                            if next_ch == '.' || next_ch == 'l' || next_ch.is_ascii_digit() {
                                spec_token.push(next_ch);
                                chars.next();
                                continue;
                            }
                            if matches!(next_ch, 'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'f' | 's' | 'c') {
                                spec_token.push(next_ch);
                                chars.next();
                            }
                            break;
                        }

                        let (spec, zero_pad, width, precision) = Self::parse_printf_format(&spec_token)?;
                        let arg = args.get(arg_index).ok_or(format!("c::printf missing argument for %{}", spec))?;

                        let formatted_arg = match spec {
                            'd' | 'i' => {
                                let value = arg.as_number().ok_or("c::printf %d requires a numeric argument".to_string())?;
                                if let Some(prec) = precision {
                                    format!("{:0width$.prec$}", value, width = width.unwrap_or(0), prec = prec)
                                } else if let Some(w) = width {
                                    if zero_pad {
                                        format!("{:0width$}", value, width = w)
                                    } else {
                                        format!("{:width$}", value, width = w)
                                    }
                                } else {
                                    value.to_string()
                                }
                            }
                            'u' => {
                                let value = arg.as_number().ok_or("c::printf %u requires a numeric argument".to_string())? as u64;
                                if let Some(prec) = precision {
                                    format!("{:0width$.prec$}", value, width = width.unwrap_or(0), prec = prec)
                                } else if let Some(w) = width {
                                    if zero_pad {
                                        format!("{:0width$}", value, width = w)
                                    } else {
                                        format!("{:width$}", value, width = w)
                                    }
                                } else {
                                    value.to_string()
                                }
                            }
                            'o' => {
                                let value = arg.as_number().ok_or("c::printf %o requires a numeric argument".to_string())? as u64;
                                if let Some(prec) = precision {
                                    format!("{:0width$.prec$o}", value, width = width.unwrap_or(0), prec = prec)
                                } else if let Some(w) = width {
                                    if zero_pad {
                                        format!("{:0width$o}", value, width = w)
                                    } else {
                                        format!("{:width$o}", value, width = w)
                                    }
                                } else {
                                    format!("{:o}", value)
                                }
                            }
                            'x' => {
                                let value = arg.as_number().ok_or("c::printf %x requires a numeric argument".to_string())? as u64;
                                if let Some(prec) = precision {
                                    format!("{:0width$.prec$x}", value, width = width.unwrap_or(0), prec = prec)
                                } else if let Some(w) = width {
                                    if zero_pad {
                                        format!("{:0width$x}", value, width = w)
                                    } else {
                                        format!("{:width$x}", value, width = w)
                                    }
                                } else {
                                    format!("{:x}", value)
                                }
                            }
                            'X' => {
                                let value = arg.as_number().ok_or("c::printf %X requires a numeric argument".to_string())? as u64;
                                if let Some(prec) = precision {
                                    format!("{:0width$.prec$X}", value, width = width.unwrap_or(0), prec = prec)
                                } else if let Some(w) = width {
                                    if zero_pad {
                                        format!("{:0width$X}", value, width = w)
                                    } else {
                                        format!("{:width$X}", value, width = w)
                                    }
                                } else {
                                    format!("{:X}", value)
                                }
                            }
                            'f' => {
                                let value = arg.as_float().ok_or("c::printf %f requires a numeric argument".to_string())?;
                                let precision = precision.unwrap_or(6);
                                if let Some(w) = width {
                                    if zero_pad {
                                        format!("{:0width$.prec$}", value, width = w, prec = precision)
                                    } else {
                                        format!("{:width$.prec$}", value, width = w, prec = precision)
                                    }
                                } else {
                                    format!("{:.prec$}", value, prec = precision)
                                }
                            }
                            's' => {
                                let value = arg.as_string();
                                if let Some(w) = width {
                                    if zero_pad {
                                        format!("{:0width$}", value, width = w)
                                    } else {
                                        format!("{:width$}", value, width = w)
                                    }
                                } else {
                                    value
                                }
                            }
                            'c' => {
                                let code = arg.as_number().ok_or("c::printf %c requires a numeric argument".to_string())?;
                                let ch = std::char::from_u32(code as u32).ok_or("c::printf %c argument not a valid Unicode scalar value".to_string())?;
                                ch.to_string()
                            }
                            _ => return Err(format!("c::printf unsupported format specifier: %{}", spec)),
                        };

                        formatted.push_str(&formatted_arg);
                        arg_index += 1;
                    }
                    None => return Err("c::printf incomplete format string".to_string()),
                }
            } else {
                formatted.push(ch);
            }
        }

        let c_string = CString::new(formatted).map_err(|_| "c::printf formatted string contains null byte".to_string())?;
        let printf_ptr = unsafe { Self::load_c_symbol("printf")? };
        let printf_fn: unsafe extern "C" fn(*const libc::c_char) -> libc::c_int = unsafe { std::mem::transmute(printf_ptr) };
        let result = unsafe { printf_fn(c_string.as_ptr()) };
        Ok(Value::Number(result as i64))
    }

    fn parse_printf_format(format_spec: &str) -> Result<(char, bool, Option<usize>, Option<usize>), String> {
        let mut chars = format_spec.chars().peekable();
        let mut zero_pad = false;
        let mut width: Option<usize> = None;
        let mut precision: Option<usize> = None;

        if let Some('0') = chars.peek() {
            zero_pad = true;
            chars.next();
        }

        while let Some(&ch) = chars.peek() {
            if ch.is_ascii_digit() {
                let digit = ch.to_digit(10).unwrap() as usize;
                width = Some(width.unwrap_or(0) * 10 + digit);
                chars.next();
            } else {
                break;
            }
        }

        if let Some('.') = chars.peek() {
            chars.next();
            let mut value = 0;
            let mut found = false;
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_digit() {
                    let digit = ch.to_digit(10).unwrap() as usize;
                    value = value * 10 + digit;
                    found = true;
                    chars.next();
                } else {
                    break;
                }
            }
            if found {
                precision = Some(value);
            }
        }

        if let Some('l') = chars.peek() {
            chars.next();
        }

        let spec = chars.next().ok_or_else(|| "c::printf incomplete format string".to_string())?;
        if !matches!(spec, 'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'f' | 's' | 'c') {
            return Err(format!("c::printf unsupported format specifier: %{}", spec));
        }

        Ok((spec, zero_pad, width, precision))
    }

    fn c_getenv(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let key = CString::new(arg.as_string()).map_err(|_| "c::getenv argument contains null byte".to_string())?;
            let getenv_ptr = unsafe { Self::load_c_symbol("getenv")? };
            let getenv_fn: unsafe extern "C" fn(*const libc::c_char) -> *mut libc::c_char = unsafe { std::mem::transmute(getenv_ptr) };
            let result = unsafe { getenv_fn(key.as_ptr()) };
            if result.is_null() {
                Ok(Value::String(String::new()))
            } else {
                let cstr = unsafe { CStr::from_ptr(result) };
                Ok(Value::String(cstr.to_string_lossy().into_owned()))
            }
        } else {
            Err("c::getenv requires one argument".to_string())
        }
    }

    fn c_strlen(&mut self, args: &[Value]) -> Result<Value, String> {
        if let Some(arg) = args.get(0) {
            let c_string = CString::new(arg.as_string()).map_err(|_| "c::strlen argument contains null byte".to_string())?;
            let strlen_ptr = unsafe { Self::load_c_symbol("strlen")? };
            let strlen_fn: unsafe extern "C" fn(*const libc::c_char) -> libc::size_t = unsafe { std::mem::transmute(strlen_ptr) };
            let len = unsafe { strlen_fn(c_string.as_ptr()) };
            Ok(Value::Number(len as i64))
        } else {
            Err("c::strlen requires one argument".to_string())
        }
    }

    fn c_memmove(&mut self, args: &[Value]) -> Result<Value, String> {
        if args.len() != 3 {
            return Err("c::memmove requires three arguments: dest, src, len".to_string());
        }

        let mut dest = args[0].as_string().into_bytes();
        let src = args[1].as_string().into_bytes();
        let len = args[2]
            .as_number()
            .ok_or("c::memmove third argument must be a number".to_string())? as usize;

        let count = len.min(src.len()).min(dest.len());
        let memmove_ptr = unsafe { Self::load_c_symbol("memmove")? };
        let memmove_fn: unsafe extern "C" fn(*mut libc::c_void, *const libc::c_void, libc::size_t) -> *mut libc::c_void = unsafe { std::mem::transmute(memmove_ptr) };
        unsafe {
            memmove_fn(dest.as_mut_ptr() as *mut libc::c_void, src.as_ptr() as *const libc::c_void, count);
        }
        let result = String::from_utf8_lossy(&dest).into_owned();
        Ok(Value::String(result))
    }

    fn c_fgets(&mut self, args: &[Value]) -> Result<Value, String> {
        let size = if let Some(size_arg) = args.get(0) {
            size_arg
                .as_number()
                .ok_or("c::fgets first argument must be a positive integer".to_string())?
                as i32
        } else {
            return Err("c::fgets requires a buffer size".to_string());
        };

        if size <= 0 {
            return Err("c::fgets buffer size must be greater than zero".to_string());
        }

        let mut buffer = vec![0u8; size as usize];
        let mode = CString::new("r").map_err(|_| "c::fgets mode string contains null byte".to_string())?;
        let fdopen_ptr = unsafe { Self::load_c_symbol("fdopen")? };
        let fdopen_fn: unsafe extern "C" fn(libc::c_int, *const libc::c_char) -> *mut libc::FILE = unsafe { std::mem::transmute(fdopen_ptr) };
        let stream = unsafe { fdopen_fn(libc::STDIN_FILENO, mode.as_ptr()) };
        if stream.is_null() {
            return Err("c::fgets failed to open stdin".to_string());
        }

        let fgets_ptr = unsafe { Self::load_c_symbol("fgets")? };
        let fgets_fn: unsafe extern "C" fn(*mut libc::c_char, libc::c_int, *mut libc::FILE) -> *mut libc::c_char = unsafe { std::mem::transmute(fgets_ptr) };
        let result = unsafe { fgets_fn(buffer.as_mut_ptr() as *mut libc::c_char, size, stream) };
        if result.is_null() {
            return Ok(Value::String(String::new()));
        }

        let cstr = unsafe { CStr::from_ptr(buffer.as_ptr() as *const libc::c_char) };
        Ok(Value::String(cstr.to_string_lossy().into_owned()))
    }

    fn capture_output<F, R>(f: F) -> Result<(R, String), String>
    where
        F: FnOnce() -> R,
    {
        let (read_fd, write_fd) = pipe().map_err(|e| e.to_string())?;
        let stdout_fd = dup(libc::STDOUT_FILENO).map_err(|e| e.to_string())?;

        dup2(write_fd, libc::STDOUT_FILENO).map_err(|e| e.to_string())?;
        nix::unistd::close(write_fd).map_err(|e| e.to_string())?;

        let result = f();

        dup2(stdout_fd, libc::STDOUT_FILENO).map_err(|e| e.to_string())?;
        nix::unistd::close(stdout_fd).map_err(|e| e.to_string())?;

        let mut output = String::new();
        let mut file = unsafe { File::from_raw_fd(read_fd) };
        file.read_to_string(&mut output).map_err(|e| e.to_string())?;

        Ok((result, output))
    }

    fn execute_external_command_capture(&mut self, cmd: &parser::Command, stdin_input: Option<&str>) -> Result<(i32, String), String> {
        let (stdout_read, stdout_write) = pipe().map_err(|e| e.to_string())?;
        let stdin_pipe = if stdin_input.is_some() {
            Some(pipe().map_err(|e| e.to_string())?)
        } else {
            None
        };

        match unsafe { fork() } {
            Ok(ForkResult::Child) => {
                if let Some((read_fd, _write_fd)) = stdin_pipe {
                    unsafe {
                        libc::dup2(read_fd, libc::STDIN_FILENO);
                    }
                }

                unsafe {
                    libc::dup2(stdout_write, libc::STDOUT_FILENO);
                }

                unsafe {
                    libc::close(stdout_read);
                    libc::close(stdout_write);
                }
                if let Some((read_fd, write_fd)) = stdin_pipe {
                    unsafe {
                        libc::close(write_fd);
                        libc::close(read_fd);
                    }
                }

                if let Some((ref path, append)) = &cmd.stdout_redirect {
                    let mut options = std::fs::OpenOptions::new();
                    options.write(true).create(true);
                    if *append {
                        options.append(true);
                    } else {
                        options.truncate(true);
                    }

                    match options.open(path) {
                        Ok(file) => {
                            let fd = file.as_raw_fd();
                            unsafe { libc::dup2(fd, libc::STDOUT_FILENO) };
                        }
                        Err(e) => {
                            eprintln!("redirect open {}: {}", path, e);
                            std::process::exit(1);
                        }
                    }
                }

                let command = CString::new(cmd.argv[0].as_str()).map_err(|e| e.to_string()).unwrap();
                let args: Vec<CString> = cmd
                    .argv
                    .iter()
                    .map(|s| CString::new(s.as_str()).unwrap())
                    .collect();

                let Err(e) = execvp(&command, &args);
                eprintln!("{}: {}", cmd.argv[0], e);
                std::process::exit(127);
            }
            Ok(ForkResult::Parent { child }) => {
                unsafe {
                    libc::close(stdout_write);
                }
                if let Some((_read_fd, write_fd)) = stdin_pipe {
                    let mut file = unsafe { File::from_raw_fd(write_fd) };
                    if let Some(input) = stdin_input {
                        file.write_all(input.as_bytes()).map_err(|e| e.to_string())?;
                    }
                    drop(file);
                }

                let mut output = String::new();
                let mut file = unsafe { File::from_raw_fd(stdout_read) };
                file.read_to_string(&mut output).map_err(|e| e.to_string())?;

                if let Ok(status) = waitpid(child, None) {
                    use nix::sys::wait::WaitStatus;
                    match status {
                        WaitStatus::Exited(_, code) => Ok((code, output)),
                        _ => Ok((1, output)),
                    }
                } else {
                    Ok((1, output))
                }
            }
            Err(_) => Err("Fork failed".to_string()),
        }
    }

    fn execute_external_pipeline(&mut self, pipeline: &parser::Pipeline) -> Result<i32, String> {
        if pipeline.commands.is_empty() {
            return Ok(0);
        }

        if pipeline.commands.len() == 1 {
            let cmd = &pipeline.commands[0];
            return match unsafe { fork() } {
                Ok(ForkResult::Child) => {
                    if let Some((ref path, append)) = &cmd.stdout_redirect {
                        let mut options = std::fs::OpenOptions::new();
                        options.write(true).create(true);
                        if *append {
                            options.append(true);
                        } else {
                            options.truncate(true);
                        }

                        match options.open(path) {
                            Ok(file) => {
                                let fd = file.as_raw_fd();
                                unsafe { libc::dup2(fd, libc::STDOUT_FILENO) };
                            }
                            Err(e) => {
                                eprintln!("redirect open {}: {}", path, e);
                                std::process::exit(1);
                            }
                        }
                    }

                    let command = CString::new(cmd.argv[0].as_str()).unwrap();
                    let args: Vec<CString> = cmd
                        .argv
                        .iter()
                        .map(|s| CString::new(s.as_str()).unwrap())
                        .collect();

                    let Err(e) = execvp(&command, &args);
                    eprintln!("{}: {}", cmd.argv[0], e);
                    std::process::exit(127);
                }
                Ok(ForkResult::Parent { child }) => {
                    if let Ok(status) = waitpid(child, None) {
                        use nix::sys::wait::WaitStatus;
                        match status {
                            WaitStatus::Exited(_, code) => return Ok(code),
                            _ => return Ok(1),
                        }
                    }
                    Ok(0)
                }
                Err(_) => Err("Fork failed".to_string()),
            }
        }

        let num_commands = pipeline.commands.len();
        let mut pipes: Vec<(libc::c_int, libc::c_int)> = Vec::new();
        for _ in 0..num_commands - 1 {
            let (read_fd, write_fd) = pipe().map_err(|e| e.to_string())?;
            pipes.push((read_fd, write_fd));
        }

        let mut pids = Vec::new();
        for (i, cmd) in pipeline.commands.iter().enumerate() {
            match unsafe { fork() } {
                Ok(ForkResult::Child) => {
                    if i > 0 {
                        unsafe {
                            libc::dup2(pipes[i - 1].0, libc::STDIN_FILENO);
                        }
                    }
                    if i < num_commands - 1 {
                        unsafe {
                            libc::dup2(pipes[i].1, libc::STDOUT_FILENO);
                        }
                    }

                    if let Some((ref path, append)) = &cmd.stdout_redirect {
                        let mut options = std::fs::OpenOptions::new();
                        options.write(true).create(true);
                        if *append {
                            options.append(true);
                        } else {
                            options.truncate(true);
                        }

                        match options.open(path) {
                            Ok(file) => {
                                let fd = file.as_raw_fd();
                                unsafe { libc::dup2(fd, libc::STDOUT_FILENO) };
                            }
                            Err(e) => {
                                eprintln!("redirect open {}: {}", path, e);
                                std::process::exit(1);
                            }
                        }
                    }

                    for (read_fd, write_fd) in &pipes {
                        unsafe {
                            libc::close(*read_fd);
                            libc::close(*write_fd);
                        }
                    }

                    let command = CString::new(cmd.argv[0].as_str()).unwrap();
                    let args: Vec<CString> = cmd
                        .argv
                        .iter()
                        .map(|s| CString::new(s.as_str()).unwrap())
                        .collect();

                    let Err(e) = execvp(&command, &args);
                    eprintln!("{}: {}", cmd.argv[0], e);
                    std::process::exit(127);
                }
                Ok(ForkResult::Parent { child }) => {
                    pids.push(child);
                }
                Err(_) => {
                    return Err("Fork failed".to_string());
                }
            }
        }

        for (read_fd, write_fd) in &pipes {
            unsafe {
                libc::close(*read_fd);
                libc::close(*write_fd);
            }
        }

        let mut last_exit = 0;
        for pid in pids {
            if let Ok(status) = waitpid(pid, None) {
                use nix::sys::wait::WaitStatus;
                if let WaitStatus::Exited(_, code) = status {
                    last_exit = code;
                }
            }
        }

        Ok(last_exit)
    }

    fn value_to_expr_literal(value: &Value) -> String {
        match value {
            Value::String(s) => {
                let mut escaped = String::new();
                for ch in s.chars() {
                    match ch {
                        '\\' => escaped.push_str("\\\\"),
                        '"' => escaped.push_str("\\\""),
                        '\n' => escaped.push_str("\\n"),
                        '\r' => escaped.push_str("\\r"),
                        '\t' => escaped.push_str("\\t"),
                        other => escaped.push(other),
                    }
                }
                format!("\"{}\"", escaped)
            }
            Value::Number(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::List(items) => {
                let contents: Vec<String> = items.iter().map(|item| format!("\"{}\"", item.replace('"', "\\\""))).collect();
                format!("[{}]", contents.join(", "))
            }
            Value::Struct(type_name, fields) => {
                let mut parts: Vec<String> = Vec::new();
                for (key, value) in fields {
                    parts.push(format!("{}: {}", key, Self::value_to_expr_literal(value)));
                }
                format!("{} {{ {} }}", type_name, parts.join(", "))
            }
            Value::Enum(type_name, variant) => format!("{}.{}", type_name, variant),
        }
    }

    fn replace_at_in_expr(expr: &str, value: &str) -> String {
        let mut result = String::new();
        let mut chars = expr.chars().peekable();
        let mut in_quote: Option<char> = None;

        while let Some(ch) = chars.next() {
            if let Some(quote) = in_quote {
                if ch == quote {
                    in_quote = None;
                }
                result.push(ch);
                continue;
            }

            if ch == '"' || ch == '\'' {
                in_quote = Some(ch);
                result.push(ch);
                continue;
            }

            if ch == '@' {
                result.push_str(value);
                continue;
            }

            result.push(ch);
        }

        result
    }

    fn function_accepts_single_arg(&self, name: &str) -> bool {
        if let Some(func) = self.env.get_function(name) {
            return func.params.len() == 1;
        }

        let normalized = if name.starts_with("std::") {
            name.to_string()
        } else {
            self.normalize_std_function_name(name).unwrap_or_else(|| name.to_string())
        };

        matches!(
            normalized.as_str(),
            "std::strlen"
                | "std::upper"
                | "std::lower"
                | "std::trim"
                | "std::lines"
                | "std::len"
                | "std::first"
                | "std::last"
                | "std::reverse"
                | "std::sizeof"
                | "std::basename"
                | "std::dirname"
                | "std::isfile"
                | "std::isdir"
                | "std::which"
                | "std::sleep"
                | "std::getuser"
                | "std::exists"
                | "std::input"
                | "std::env"
                | "std::format_gb"
                | "std::format_gib"
                | "std::even"
                | "std::odd"
                | "std::print"
                | "std::println"
                | "std::eprint"
                | "std::eprintln"
                | "std::exit"
        )
    }

    fn bind_stream_input(&self, expr: &str, input: &str) -> Result<String, String> {
        if expr.contains('@') {
            return Ok(Self::replace_at_in_expr(expr, &Self::value_to_expr_literal(&Value::String(input.to_string()))));
        }

        if let Some((name, args)) = Self::parse_function_call(expr) {
            if args.is_empty() && self.function_accepts_single_arg(&name) {
                return Ok(format!("{}({})", name, Self::value_to_expr_literal(&Value::String(input.to_string()))));
            }
        }

        if Self::is_bare_identifier(expr) && self.normalize_std_function_name(expr).is_some() && self.function_accepts_single_arg(expr) {
            return Ok(format!("{}({})", expr, Self::value_to_expr_literal(&Value::String(input.to_string()))));
        }

        Ok(expr.to_string())
    }

    fn bind_chain_step(&self, expr: &str, input: &Value) -> Result<String, String> {
        if expr.contains('@') {
            return Ok(Self::replace_at_in_expr(expr, &Self::value_to_expr_literal(input)));
        }

        if let Some((name, args)) = Self::parse_function_call(expr) {
            if args.is_empty() && self.function_accepts_single_arg(&name) {
                return Ok(format!("{}({})", name, Self::value_to_expr_literal(input)));
            }
        }

        Ok(expr.to_string())
    }

    fn execute_chain_steps(&mut self, steps: &[String]) -> Result<Value, String> {
        let mut current_value: Option<Value> = None;

        for step in steps {
            let trimmed = step.trim();
            if trimmed.is_empty() {
                continue;
            }

            let bound = if let Some(ref current) = current_value {
                if trimmed.contains('@') {
                    self.bind_chain_step(trimmed, current)?
                } else {
                    self.bind_chain_step(trimmed, current)?
                }
            } else {
                trimmed.to_string()
            };

            if bound.contains('@') && current_value.is_none() {
                return Err("Cannot use @ in first chain step".to_string());
            }

            let value = self.eval_expression(&bound)?;
            current_value = Some(value);
        }

        Ok(current_value.unwrap_or(Value::String(String::new())))
    }

    fn eval_chain_expression(&mut self, expr: &str) -> Result<Value, String> {
        let trimmed = expr.trim();
        let chain_start = trimmed.find("chain").ok_or("Invalid chain expression")?;
        let after_chain = &trimmed[chain_start + 5..];
        let block_start = chain_start + 5 + after_chain.find('{').ok_or("Invalid chain syntax")?;
        let mut depth = 0;
        let mut end_index = None;

        for (idx, ch) in trimmed[block_start..].char_indices() {
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    end_index = Some(block_start + idx);
                    break;
                }
            }
        }

        let end_index = end_index.ok_or("Unterminated chain block")?;
        let body = &trimmed[block_start + 1..end_index];
        
        // Parse steps from the chain body
        // Steps can be separated by newlines or just whitespace
        let steps = self.parse_chain_steps(body)?;

        self.execute_chain_steps(&steps)
    }

    fn parse_chain_steps(&self, body: &str) -> Result<Vec<String>, String> {
        let mut steps = Vec::new();
        let mut current_step = String::new();
        let mut in_quote: Option<char> = None;
        let mut in_parens = 0;
        let mut in_brackets = 0;
        let mut in_braces = 0;
        let mut chars = body.chars().peekable();

        while let Some(ch) = chars.next() {
            // Handle quotes
            if let Some(quote) = in_quote {
                current_step.push(ch);
                if ch == quote && (current_step.len() < 2 || current_step.chars().rev().nth(1) != Some('\\')) {
                    in_quote = None;
                }
                continue;
            }

            if ch == '"' || ch == '\'' {
                in_quote = Some(ch);
                current_step.push(ch);
                continue;
            }

            // Track nesting
            match ch {
                '(' => { in_parens += 1; current_step.push(ch); }
                ')' => { in_parens -= 1; current_step.push(ch); }
                '[' => { in_brackets += 1; current_step.push(ch); }
                ']' => { in_brackets -= 1; current_step.push(ch); }
                '{' => { in_braces += 1; current_step.push(ch); }
                '}' => { in_braces -= 1; current_step.push(ch); }
                c if c.is_whitespace() && in_parens == 0 && in_brackets == 0 && in_braces == 0 => {
                    // Whitespace at top level - potential step separator
                    let trimmed = current_step.trim().to_string();
                    if !trimmed.is_empty() {
                        steps.push(trimmed);
                        current_step.clear();
                    }
                }
                _ => current_step.push(ch),
            }
        }

        let trimmed = current_step.trim().to_string();
        if !trimmed.is_empty() {
            steps.push(trimmed);
        }

        Ok(steps)
    }

    fn execute_stream_pipeline(&mut self, pipeline: &parser::Pipeline) -> Result<i32, String> {
        if pipeline.commands.is_empty() {
            return Ok(0);
        }

        let mut current_input: Option<String> = None;
        let mut last_exit_code = 0;

        for (idx, cmd) in pipeline.commands.iter().enumerate() {
            let is_last = idx + 1 == pipeline.commands.len();
            let input = current_input.as_deref();
            let mut stage_output = String::new();
            let mut exit_code = 0;
            let mut builtin_executed = false;

            if !cmd.argv.is_empty() && self.registry.has_builtin(&cmd.argv[0]) {
                builtin_executed = true;
                let (result, output) = Self::capture_output(|| {
                    self.registry
                        .run_builtin(&cmd.argv, &mut self.env, &self.job_manager)
                        .unwrap_or(0)
                })?;
                stage_output = output;
                exit_code = result;
            }

            if !builtin_executed {
                let raw_expr = cmd.raw.trim();
                let bound_expr = if let Some(input_value) = current_input.as_ref() {
                    self.bind_stream_input(raw_expr, input_value)?
                } else {
                    raw_expr.to_string()
                };

                if let Some((name, args)) = Self::parse_function_call(&bound_expr) {
                    if name == "_bail" || name == "_exit" {
                        Self::handle_exit_builtin(&name, &args)?;
                    }
                }

                if Self::parse_function_call(&bound_expr).is_some()
                    || self.normalize_std_function_name(&bound_expr).is_some()
                    || self.env.get_function(&bound_expr).is_some()
                    || bound_expr.starts_with("c::")
                {
                    let value = self.eval_expression(&bound_expr)?;
                    stage_output = value.as_string();
                    exit_code = 0;
                } else {
                    let (code, output) = self.execute_external_command_capture(cmd, input)?;
                    stage_output = output;
                    exit_code = code;
                }
            }

            current_input = Some(stage_output);
            last_exit_code = exit_code;

            if is_last {
                if let Some(output) = current_input.as_ref() {
                    if let Some((ref path, append)) = &cmd.stdout_redirect {
                        let mut options = OpenOptions::new();
                        options.write(true).create(true);
                        if *append {
                            options.append(true);
                        } else {
                            options.truncate(true);
                        }

                        let mut file = options.open(path).map_err(|e| e.to_string())?;
                        file.write_all(output.as_bytes()).map_err(|e| e.to_string())?;
                        return Ok(last_exit_code);
                    }

                    if !output.is_empty() {
                        print!("{}", output);
                    }
                }
            }
        }

        Ok(last_exit_code)
    }

    fn expand_vars(&self, s: &str) -> String {
        let mut result = String::new();
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '$' {
                if let Some('{') = chars.peek() {
                    chars.next(); // consume {
                    let mut var_name = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                            var_name.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                    if let Some('}') = chars.next() {
                        if let Some(value) = self.env.get_path(&var_name) {
                            result.push_str(&value.as_string());
                        }
                    }
                } else {
                    let mut var_name = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                            var_name.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                    if !var_name.is_empty() {
                        if let Some(value) = self.env.get_path(&var_name) {
                            result.push_str(&value.as_string());
                        }
                    } else {
                        result.push('$');
                    }
                }
            } else if c == '~' {
                if let Ok(home) = std::env::var("HOME") {
                    result.push_str(&home);
                } else {
                    result.push('~');
                }
            } else {
                result.push(c);
            }
        }

        result
    }

    pub fn job_manager(&self) -> &Rc<RefCell<JobManager>> {
        &self.job_manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::BuiltinRegistry;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn make_executor() -> Executor {
        Executor::new(Rc::new(RefCell::new(JobManager::new())), Rc::new(BuiltinRegistry::new()))
    }

    #[test]
    fn chain_implicit_single_arg_function() {
        let mut executor = make_executor();
        let result = executor
            .eval_chain_expression("chain { \"hello\" std::upper() }")
            .expect("chain evaluation failed");
        assert_eq!(result.as_string(), "HELLO");
    }

    #[test]
    fn chain_explicit_at_binding() {
        let mut executor = make_executor();
        let result = executor
            .eval_chain_expression("chain { \"hello world\" std::split(@, \" \") std::len() }")
            .expect("chain evaluation failed");
        assert_eq!(result.as_number(), Some(2));
    }

    #[test]
    fn chain_first_step_literal_then_function() {
        let mut executor = make_executor();
        let result = executor
            .eval_chain_expression("chain { \"  trimmed  \" std::trim() }")
            .expect("chain evaluation failed");
        assert_eq!(result.as_string(), "trimmed");
    }
}
