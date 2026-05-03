// src/completion.rs
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};
use std::env;
use std::fs;

pub struct ShellCompleter;

impl ShellCompleter {
    pub fn new() -> Self {
        ShellCompleter
    }

    fn complete_command(&self, prefix: &str) -> Vec<String> {
        let mut matches = Vec::new();

        // Search PATH
        if let Ok(path) = env::var("PATH") {
            for dir in path.split(':') {
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        if let Ok(name) = entry.file_name().into_string() {
                            if name.starts_with(prefix) && matches.len() < 512 {
                                if !matches.contains(&name) {
                                    matches.push(name);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Add builtins
        for builtin in &[
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
        ] {
            if builtin.starts_with(prefix) && !matches.contains(&builtin.to_string()) {
                matches.push(builtin.to_string());
            }
        }

        matches.sort();
        matches
    }

    fn complete_path(&self, prefix: &str) -> Vec<String> {
        let mut matches = Vec::new();

        let (dir, base) = if let Some(pos) = prefix.rfind('/') {
            (&prefix[..=pos], &prefix[pos + 1..])
        } else {
            (".", prefix)
        };

        let dir_path = if dir.starts_with('~') {
            if let Ok(home) = env::var("HOME") {
                dir.replacen('~', &home, 1)
            } else {
                dir.to_string()
            }
        } else {
            dir.to_string()
        };

        if let Ok(entries) = fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if (name.starts_with(base) || (base.is_empty() && !name.starts_with('.')))
                        && matches.len() < 512
                    {
                        let mut full = if dir == "." {
                            name.clone()
                        } else {
                            format!("{}{}", &prefix[..prefix.len() - base.len()], name.clone())
                        };
                        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                            full.push('/');
                        }
                        matches.push(full);
                    }
                }
            }
        }

        matches.sort();
        matches
    }
}

fn is_path_like(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.starts_with('~')
}

impl Completer for ShellCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> Result<(usize, Vec<Pair>), ReadlineError> {
        let line_to_cursor = &line[..pos];

        // Find the start of the current token
        let token_start = line_to_cursor
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);

        let prefix = &line_to_cursor[token_start..];
        let is_first_token = line_to_cursor[..token_start].trim().is_empty();

        let matches = if is_first_token && !is_path_like(prefix) {
            self.complete_command(prefix)
        } else {
            self.complete_path(prefix)
        };

        let pairs: Vec<Pair> = matches
            .into_iter()
            .map(|s| Pair {
                display: s.clone(),
                replacement: s,
            })
            .collect();

        Ok((token_start, pairs))
    }
}

impl Helper for ShellCompleter {}
impl Hinter for ShellCompleter {
    type Hint = String;
}
impl Highlighter for ShellCompleter {}
impl Validator for ShellCompleter {}
