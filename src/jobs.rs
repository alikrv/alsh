// src/jobs.rs
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Running,
    Stopped,
    Done,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: usize,
    pub pid: Pid,
    pub command: String,
    pub state: JobState,
}

pub struct JobManager {
    jobs: HashMap<usize, Job>,
    next_id: usize,
}

impl JobManager {
    pub fn new() -> Self {
        JobManager {
            jobs: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn add_job(&mut self, pid: Pid, command: String, state: JobState) -> usize {
        let id = self.next_id;
        self.next_id += 1;

        let job = Job {
            id,
            pid,
            command,
            state,
        };

        self.jobs.insert(id, job);
        id
    }

    pub fn remove_job(&mut self, id: usize) -> Option<Job> {
        self.jobs.remove(&id)
    }

    pub fn get_job(&self, id: usize) -> Option<&Job> {
        self.jobs.get(&id)
    }

    pub fn get_job_mut(&mut self, id: usize) -> Option<&mut Job> {
        self.jobs.get_mut(&id)
    }

    pub fn list_jobs(&self) -> Vec<&Job> {
        let mut jobs: Vec<&Job> = self.jobs.values().collect();
        jobs.sort_by_key(|j| j.id);
        jobs
    }

    pub fn find_by_pid(&self, pid: Pid) -> Option<&Job> {
        self.jobs.values().find(|j| j.pid == pid)
    }

    pub fn find_by_pid_mut(&mut self, pid: Pid) -> Option<&mut Job> {
        self.jobs.values_mut().find(|j| j.pid == pid)
    }

    pub fn update_job_states(&mut self) {
        let mut to_remove = Vec::new();

        for (id, job) in self.jobs.iter_mut() {
            match waitpid(
                job.pid,
                Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED | WaitPidFlag::WCONTINUED),
            ) {
                Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => {
                    job.state = JobState::Done;
                    to_remove.push(*id);
                }
                Ok(WaitStatus::Stopped(_, _)) => {
                    job.state = JobState::Stopped;
                }
                Ok(WaitStatus::Continued(_)) => {
                    job.state = JobState::Running;
                }
                Ok(WaitStatus::StillAlive) => {
                    // No change
                }
                _ => {}
            }
        }

        // Remove completed jobs
        for id in to_remove {
            self.jobs.remove(&id);
        }
    }

    pub fn send_signal(&self, id: usize, signal: Signal) -> Result<(), String> {
        if let Some(job) = self.get_job(id) {
            kill(job.pid, signal).map_err(|e| format!("Failed to send signal: {}", e))
        } else {
            Err(format!("No such job: {}", id))
        }
    }

    pub fn get_last_job_id(&self) -> Option<usize> {
        self.jobs.keys().max().copied()
    }
}
