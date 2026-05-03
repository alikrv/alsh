use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::rc::Rc;
use nix::unistd::{getgroups, Gid, Uid};

pub fn builtin_id(_argv: &[String], _env: &mut Environment, _job_manager: &Rc<RefCell<JobManager>>) -> i32 {
    // nix 0.27: getuid/getgid no longer exist → use Uid::current(), Gid::current()
    let uid = Uid::current();
    let gid = Gid::current();

    let username = get_username(uid);
    let groupname = get_groupname(gid);

    print!("uid={}({})", uid.as_raw(), username);
    print!(" gid={}({})", gid.as_raw(), groupname);

    // getgroups() comes from nix::unistd with "user" feature enabled
    if let Ok(groups) = getgroups() {
        if !groups.is_empty() {
            print!(" groups=");
            for (i, group_gid) in groups.iter().enumerate() {
                if i > 0 {
                    print!(",");
                }
                let gname = get_groupname(*group_gid);
                print!("{}({})", group_gid.as_raw(), gname);
            }
        }
    }

    println!();
    0
}

fn get_username(uid: Uid) -> String {
    // simple fallback: real shells read passwd database
    std::env::var("USER").unwrap_or_else(|_| uid.as_raw().to_string())
}

fn get_groupname(gid: Gid) -> String {
    // simple fallback: real shells read /etc/group
    gid.as_raw().to_string()
}
