use crate::control_flow::Environment;
use crate::jobs::{JobManager, JobState};
use std::cell::RefCell;
use std::rc::Rc;

pub fn builtin_jobs(_argv: &[String], _env: &mut Environment, job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    let jobs: Vec<_> = job_manager.borrow().list_jobs().into_iter().cloned().collect();

    if jobs.is_empty() {
        return 0;
    }

    for job in jobs {
        let state = match job.state {
            JobState::Running => "Running",
            JobState::Stopped => "Stopped",
            JobState::Done => "Done",
        };

        println!("[{}] {} {} {}", job.id, job.pid, state, job.command);
    }

    0
}
