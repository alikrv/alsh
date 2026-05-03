use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;

pub fn builtin_which(argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    if argv.len() < 2 {
        eprintln!("which: usage: which command [command ...]");
        return 1;
    }

    let mut exit_code = 0;

    for cmd in &argv[1..] {
        if let Some(path) = find_in_path(cmd) {
            println!("{}", path);
        } else {
            exit_code = 1;
        }
    }

    exit_code
}

fn find_in_path(cmd: &str) -> Option<String> {
    if let Ok(path) = env::var("PATH") {
        for dir in path.split(':') {
            let full_path = format!("{}/{}", dir, cmd);
            if let Ok(metadata) = fs::metadata(&full_path) {
                if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                    return Some(full_path);
                }
            }
        }
    }
    None
}
