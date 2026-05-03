use crate::control_flow::Environment;
use crate::jobs::{JobManager, JobState};
use nix::sys::signal::{kill, Signal};
use std::cell::RefCell;
use std::rc::Rc;

pub fn builtin_bg(argv: &[String], _env: &mut Environment, job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    let job_id = if argv.len() > 1 {
        match argv[1].trim_start_matches('%').parse::<usize>() {
            Ok(id) => id,
            Err(_) => {
                eprintln!("bg: invalid job id: {}", argv[1]);
                return 1;
            }
        }
    } else {
        // use most recent job
        match job_manager.borrow().get_last_job_id() {
            Some(id) => id,
            None => {
                eprintln!("bg: no current job");
                return 1;
            }
        }
    };

    let mut manager = job_manager.borrow_mut();

    match manager.get_job_mut(job_id) {
        Some(job) => {
            if job.state != JobState::Stopped {
                eprintln!("bg: job {} is already running", job_id);
                return 1;
            }

            // send SIGCONT to resume in background
            if let Err(e) = kill(job.pid, Signal::SIGCONT) {
                eprintln!("bg: failed to continue job: {}", e);
                return 1;
            }

            job.state = JobState::Running;
            println!("[{}] {} &", job.id, job.command);
            0
        }
        None => {
            eprintln!("bg: no such job: {}", job_id);
            1
        }
    }
}
