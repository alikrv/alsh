use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::env;
use std::rc::Rc;

pub fn builtin_set(argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    if argv.len() < 3 {
        eprintln!("set: usage: set VAR value");
        return 1;
    }

    let var = &argv[1];
    let value = argv[2..].join(" ");

    env::set_var(var, value);
    0
}
