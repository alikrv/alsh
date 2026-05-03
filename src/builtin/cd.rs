use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::env;
use std::path::Path;
use std::rc::Rc;

pub fn builtin_cd(argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    let target = if argv.len() > 1 {
        argv[1].clone()
    } else {
        match env::var("HOME") {
            Ok(home) => home,
            Err(_) => {
                eprintln!("cd: HOME not set");
                return 1;
            }
        }
    };

    // Expand tilde if needed
    let expanded = if target.starts_with('~') {
        if let Ok(home) = env::var("HOME") {
            target.replacen('~', &home, 1)
        } else {
            target
        }
    } else {
        target
    };

    if let Err(e) = env::set_current_dir(Path::new(&expanded)) {
        eprintln!("cd: {}: {}", expanded, e);
        return 1;
    }

    0
}
