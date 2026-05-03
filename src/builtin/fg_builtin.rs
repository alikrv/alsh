use crate::control_flow::Environment;
use crate::jobs::{JobManager, JobState};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::waitpid;
use std::cell::RefCell;
use std::rc::Rc;

pub fn builtin_fg(argv: &[String], _env: &mut Environment, job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    let job_id = if argv.len() > 1 {
        match argv[1].trim_start_matches('%').parse::<usize>() {
            Ok(id) => id,
            Err(_) => {
                eprintln!("fg: invalid job id: {}", argv[1]);
                return 1;
            }
        }
    } else {
        // Use most recent job
        match job_manager.borrow().get_last_job_id() {
            Some(id) => id,
            None => {
                eprintln!("fg: no current job");
                return 1;
            }
        }
    };

    let pid = {
        let manager = job_manager.borrow();
        match manager.get_job(job_id) {
            Some(job) => {
                println!("{}", job.command);

                // Send SIGCONT if stopped
                if job.state == JobState::Stopped {
                    if let Err(e) = kill(job.pid, Signal::SIGCONT) {
                        eprintln!("fg: failed to continue job: {}", e);
                        return 1;
                    }
                }

                job.pid
            }
            None => {
                eprintln!("fg: no such job: {}", job_id);
                return 1;
            }
        }
    };

    // Wait for the job to complete
    if let Err(e) = waitpid(pid, None) {
        eprintln!("fg: waitpid error: {}", e);
        return 1;
    }

    // Remove job from list
    job_manager.borrow_mut().remove_job(job_id);

    0
}
