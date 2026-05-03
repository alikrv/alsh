use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::env;
use std::rc::Rc;

pub fn builtin_logname(_argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    match env::var("USER").or_else(|_| env::var("LOGNAME")) {
        Ok(user) => {
            println!("{}", user);
            0
        }
        Err(_) => {
            eprintln!("logname: cannot find name for user");
            1
        }
    }
}
