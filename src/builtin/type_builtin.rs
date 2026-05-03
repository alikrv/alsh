use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;

pub fn builtin_type(argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    if argv.len() < 2 {
        eprintln!("type: usage: type command [command ...]");
        return 1;
    }

    let builtins = vec![
        "cd",
        "exit",
        "export",
        "help",
        "pwd",
        "jobs",
        "fg",
        "foreground",
        "bg",
        "background",
        "disown",
        "type",
        "which",
        "realpath",
        "dirs",
        "logname",
        "id",
    ];

    for cmd in &argv[1..] {
        if builtins.contains(&cmd.as_str()) {
            println!("{} is a shell builtin", cmd);
        } else if let Some(path) = find_in_path(cmd) {
            println!("{} is {}", cmd, path);
        } else {
            println!("{}: not found", cmd);
        }
    }

    0
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
