use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::env;
use std::rc::Rc;

pub fn builtin_export(argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    if argv.len() < 2 {
        eprintln!("export: usage: export VAR=value");
        return 1;
    }

    for arg in &argv[1..] {
        if let Some(eq_pos) = arg.find('=') {
            let (var, val) = arg.split_at(eq_pos);
            let val = &val[1..];
            env::set_var(var, val);
        } else {
            eprintln!("export: invalid format: {}", arg);
            return 1;
        }
    }

    0
}
