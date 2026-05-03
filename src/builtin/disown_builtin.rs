    use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::rc::Rc;

pub fn builtin_disown(argv: &[String], _env: &mut Environment, job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    let job_id = if argv.len() > 1 {
        match argv[1].trim_start_matches('%').parse::<usize>() {
            Ok(id) => id,
            Err(_) => {
                eprintln!("disown: invalid job id: {}", argv[1]);
                return 1;
            }
        }
    } else {
        // Use most recent job
        match job_manager.borrow().get_last_job_id() {
            Some(id) => id,
            None => {
                eprintln!("disown: no current job");
                return 1;
            }
        }
    };

    let mut manager = job_manager.borrow_mut();

    match manager.remove_job(job_id) {
        Some(job) => {
            println!("Job [{}] {} disowned", job.id, job.command);
            0
        }
        None => {
            eprintln!("disown: no such job: {}", job_id);
            1
        }
    }
}
