// obsolete
use crate::builtin::BuiltinRegistry;
use crate::control_flow::{ControlFlowParser, Environment, Value};
use crate::executor::Executor;
use crate::jobs::JobManager;
use nix::libc;
use nix::unistd::{close, dup, dup2, pipe};
use std::cell::RefCell;
use std::fs::File;
use std::io::Read;
use std::os::unix::io::FromRawFd;
use std::rc::Rc;

pub fn builtin_let(args: &[String], env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    if args.len() < 4 || args[2] != "=" {
        eprintln!("let: usage: let var = value");
        return 1;
    }

    let var_name = &args[1];
    let value_str = args[3..].join(" ");

    match parse_value(&value_str, env) {
        Ok(value) => {
            env.set(var_name.clone(), value);
            0
        }
        Err(e) => {
            eprintln!("let: {}", e);
            1
        }
    }
}

fn parse_value(expr: &str, env: &Environment) -> Result<Value, String> {
    let expr = expr.trim();

    if expr.starts_with('"') && expr.ends_with('"') {
        // Expandable string
        let inner = &expr[1..expr.len()-1];
        let expanded = expand_vars(inner, env);
        Ok(Value::String(expanded))
    } else if expr.starts_with('\'') && expr.ends_with('\'') {
        // Literal string
        let inner = &expr[1..expr.len()-1];
        Ok(Value::String(inner.to_string()))
    } else if expr.starts_with('(') && expr.ends_with(')') {
        // Arithmetic
        let inner = &expr[1..expr.len()-1];
        match eval_arithmetic(inner, env) {
            Ok(num) => Ok(Value::Number(num)),
            Err(e) => Err(format!("arithmetic error: {}", e)),
        }
    } else if expr.starts_with('{') && expr.ends_with('}') {
        // Block substitution - execute the block internally and capture output
        let inner = expr[1..expr.len() - 1].trim();

        let sub_env = Environment {
            variables: env.variables.clone(),
            functions: env.functions.clone(),
        };
        let sub_job_manager = Rc::new(RefCell::new(JobManager::new()));
        let sub_registry = Rc::new(BuiltinRegistry::new());
        let mut executor = Executor::new(sub_job_manager.clone(), sub_registry);
        *executor.env() = sub_env;

        let (run_result, output) = capture_output(|| {
            let mut parser = ControlFlowParser::new(inner);
            let statements = parser.parse().map_err(|e| e.to_string())?;
            executor.execute_statements_with_nonzero_error(&statements)
        })?;

        run_result.map(|_| Value::String(output.trim().to_string()))
    } else if let Ok(num) = expr.parse::<i64>() {
        Ok(Value::Number(num))
    } else if let Ok(float) = expr.parse::<f64>() {
        Ok(Value::Float(float))
    } else {
        // Treat as string, with variable expansion
        let expanded = expand_vars(expr, env);
        Ok(Value::String(expanded))
    }
}

fn capture_output<F, R>(f: F) -> Result<(R, String), String>
where
    F: FnOnce() -> R,
{
    let (read_fd, write_fd) = pipe().map_err(|e| e.to_string())?;
    let stdout_fd = dup(libc::STDOUT_FILENO).map_err(|e| e.to_string())?;
    let stderr_fd = dup(libc::STDERR_FILENO).map_err(|e| e.to_string())?;

    dup2(write_fd, libc::STDOUT_FILENO).map_err(|e| e.to_string())?;
    dup2(write_fd, libc::STDERR_FILENO).map_err(|e| e.to_string())?;
    close(write_fd).map_err(|e| e.to_string())?;

    let result = f();

    dup2(stdout_fd, libc::STDOUT_FILENO).map_err(|e| e.to_string())?;
    dup2(stderr_fd, libc::STDERR_FILENO).map_err(|e| e.to_string())?;
    close(stdout_fd).map_err(|e| e.to_string())?;
    close(stderr_fd).map_err(|e| e.to_string())?;

    let mut output = String::new();
    let mut file = unsafe { File::from_raw_fd(read_fd) };
    file.read_to_string(&mut output).map_err(|e| e.to_string())?;

    Ok((result, output))
}

fn expand_vars(s: &str, env: &Environment) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some('{') = chars.peek() {
                chars.next(); // consume {
                let mut var_name = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_alphanumeric() || ch == '_' {
                        var_name.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                if let Some('}') = chars.next() {
                    if let Some(value) = env.get(&var_name) {
                        result.push_str(&value.as_string());
                    }
                }
            } else {
                let mut var_name = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_alphanumeric() || ch == '_' {
                        var_name.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                if !var_name.is_empty() {
                    if let Some(value) = env.get(&var_name) {
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

fn eval_arithmetic(expr: &str, env: &Environment) -> Result<i64, String> {
    fn resolve_operand(token: &str, env: &Environment) -> Option<i64> {
        let token = token.trim();
        let token = if token.starts_with('$') {
            &token[1..]
        } else {
            token
        };

        if let Ok(num) = token.parse::<i64>() {
            Some(num)
        } else if let Some(value) = env.get(token) {
            value.as_number()
        } else {
            None
        }
    }

    // Simple arithmetic parser for + - * /
    let tokens: Vec<&str> = expr.split_whitespace().collect();
    if tokens.len() == 1 {
        if let Some(num) = resolve_operand(tokens[0], env) {
            return Ok(num);
        }
    }
    if tokens.len() == 3 {
        let left = resolve_operand(tokens[0], env).ok_or("invalid left operand".to_string())?;
        let right = resolve_operand(tokens[2], env).ok_or("invalid right operand".to_string())?;

        match tokens[1] {
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
        }
    } else {
        Err("complex arithmetic not supported".to_string())
    }
}