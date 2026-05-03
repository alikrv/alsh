// src/builtin/help.rs
use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::rc::Rc;

pub fn builtin_help(_argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    println!("alsh-rs - Custom Shell with Job Control");
    println!();
    println!("Built-in commands:");
    println!("  cd [dir]        - Change directory");
    println!("  exit [code]     - Exit the shell");
    println!("  export VAR=val  - Set environment variable");
    println!("  let var = val   - Set shell variable");
    println!("  help            - Show this help");
    println!("  pwd             - Print working directory");
    println!();
    println!("Job control:");
    println!("  jobs            - List background jobs");
    println!("  fg [%job]       - Bring job to foreground (alias: foreground)");
    println!("  bg [%job]       - Resume job in background (alias: background)");
    println!("  disown [%job]   - Remove job from job table");
    println!("  command &       - Run command in background");
    println!();
    println!("Features:");
    println!("  - Tab completion for commands and paths");
    println!("  - Command history (up/down arrows)");
    println!("  - Tilde expansion (~/)");
    println!("  - Environment variable expansion ($VAR)");
    println!("  - Ctrl+C to cancel current line");
    println!("  - Ctrl+D on empty line to exit");

    0
}
