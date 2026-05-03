// src/builtin/mod.rs
mod cd;
mod exit;
mod export;
mod help;
mod pwd;
mod set_builtin;

// Job control builtins
mod bg_builtin;
mod disown_builtin;
mod fg_builtin;
mod jobs_builtin;

// Utility builtins
mod dirs_builtin;
mod id_builtin;
mod logname_builtin;
mod realpath_builtin;
mod type_builtin;
mod which_builtin;

use crate::control_flow::Environment;
use crate::jobs::JobManager;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub type BuiltinFn = fn(&[String], &mut Environment, &Rc<RefCell<JobManager>>) -> i32;

pub struct BuiltinRegistry {
    builtins: HashMap<String, BuiltinFn>,
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            builtins: HashMap::new(),
        };

        // Register all builtins
        registry.register("cd", cd::builtin_cd);
        registry.register("exit", exit::builtin_exit);
        registry.register("export", export::builtin_export);
        registry.register("help", help::builtin_help);
        registry.register("pwd", pwd::builtin_pwd);
        registry.register("jobs", jobs_builtin::builtin_jobs);
        registry.register("fg", fg_builtin::builtin_fg);
        registry.register("foreground", fg_builtin::builtin_fg);
        registry.register("bg", bg_builtin::builtin_bg);
        registry.register("background", bg_builtin::builtin_bg);
        registry.register("disown", disown_builtin::builtin_disown);
        registry.register("type", type_builtin::builtin_type);
        registry.register("which", which_builtin::builtin_which);
        registry.register("realpath", realpath_builtin::builtin_realpath);
        registry.register("dirs", dirs_builtin::builtin_dirs);
        registry.register("logname", logname_builtin::builtin_logname);
        registry.register("id", id_builtin::builtin_id);
        registry.register("set", set_builtin::builtin_set);

        registry
    }

    fn register(&mut self, name: &str, func: BuiltinFn) {
        self.builtins.insert(name.to_string(), func);
    }

    pub fn run_builtin(&self, argv: &[String], env: &mut Environment, job_manager: &Rc<RefCell<JobManager>>) -> Option<i32> {
        if argv.is_empty() {
            return None;
        }

        self.builtins
            .get(&argv[0])
            .map(|func| func(argv, env, job_manager))
    }

    pub fn has_builtin(&self, name: &str) -> bool {
        self.builtins.contains_key(name)
    }

    pub fn list_builtins(&self) -> Vec<String> {
        let mut names: Vec<String> = self.builtins.keys().cloned().collect();
        names.sort();
        names
    }
}
