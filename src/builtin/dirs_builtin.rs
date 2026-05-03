use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::env;
use std::rc::Rc;

pub fn builtin_dirs(_argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    match env::current_dir() {
        Ok(path) => {
            println!("{}", path.display());
            0
        }
        Err(e) => {
            eprintln!("dirs: {}", e);
            1
        }
    }
}
