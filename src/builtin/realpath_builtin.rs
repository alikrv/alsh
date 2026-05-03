use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::fs;
use std::rc::Rc;

pub fn builtin_realpath(argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    if argv.len() < 2 {
        eprintln!("realpath: usage: realpath path [path ...]");
        return 1;
    }

    let mut exit_code = 0;

    for path in &argv[1..] {
        match fs::canonicalize(path) {
            Ok(real_path) => println!("{}", real_path.display()),
            Err(e) => {
                eprintln!("realpath: {}: {}", path, e);
                exit_code = 1;
            }
        }
    }

    exit_code
}
