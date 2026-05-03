use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

pub fn builtin_exit(argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    let code = if argv.len() > 1 {
        argv[1].parse::<i32>().unwrap_or(0)
    } else {
        0
    };

    // Ensure stdout is flushed
    io::stdout().flush().ok();

    std::process::exit(code);
}
