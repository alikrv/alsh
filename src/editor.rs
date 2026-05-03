// src/editor.rs
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

const LINE_MAX: usize = 1024;
const HIST_MAX: usize = 256;
const MATCH_MAX: usize = 512;

pub struct LineEditor {
    history: Vec<String>,
}

impl LineEditor {
    pub fn new() -> Self {
        LineEditor {
            history: Vec::new(),
        }
    }

    pub fn add_to_history(&mut self, line: String) {
        if self.history.len() >= HIST_MAX {
            self.history.remove(0);
        }
        self.history.push(line);
    }

    pub fn read_line(&mut self, terminal: &mut crate::terminal::Terminal) -> io::Result<String> {
        let mut buffer = String::new();
        let mut cursor = 0;
        let mut hist_idx: Option<usize> = None;

        loop {
            let mut c = [0u8; 1];
            let n = io::stdin().read(&mut c)?;
            if n == 0 {
                continue;
            }
            let ch = c[0];

            // Ctrl+C
            if ch == 3 {
                io::stdout().write_all(b"^C\r\n")?;
                io::stdout().flush()?;
                buffer.clear();
                cursor = 0;
                hist_idx = None;
                return Ok(String::new());
            }

            // Ctrl+D on empty line
            if ch == 4 && buffer.is_empty() {
                io::stdout().write_all(b"\r\n")?;
                io::stdout().flush()?;
                std::process::exit(0);
            }

            // Enter
            if ch == b'\r' || ch == b'\n' {
                io::stdout().write_all(b"\r\n")?;
                io::stdout().flush()?;
                return Ok(buffer);
            }

            // Backspace
            if ch == 127 || ch == 8 {
                if cursor > 0 {
                    buffer.remove(cursor - 1);
                    cursor -= 1;
                    self.redraw_line(&buffer, cursor)?;
                }
                continue;
            }

            // Tab completion
            if ch == b'\t' {
                self.complete_at(&mut buffer, &mut cursor)?;
                self.redraw_line(&buffer, cursor)?;
                continue;
            }

            // Escape sequences
            if ch == 27 {
                let mut seq = [0u8; 2];
                // Try to read the next two bytes
                let mut total = 0;
                while total < 2 {
                    match io::stdin().read(&mut seq[total..]) {
                        Ok(n) if n > 0 => total += n,
                        _ => break,
                    }
                }
                
                if total == 2 && seq[0] == b'[' {
                    match seq[1] {
                        b'C' if cursor < buffer.len() => {
                            // Right arrow
                            cursor += 1;
                            io::stdout().write_all(b"\x1b[C")?;
                            io::stdout().flush()?;
                        }
                        b'D' if cursor > 0 => {
                            // Left arrow
                            cursor -= 1;
                            io::stdout().write_all(b"\x1b[D")?;
                            io::stdout().flush()?;
                        }
                        b'A' => {
                            // Up arrow
                            if !self.history.is_empty() {
                                hist_idx = Some(match hist_idx {
                                    None => self.history.len() - 1,
                                    Some(idx) if idx > 0 => idx - 1,
                                    Some(idx) => idx,
                                });
                                buffer = self.history[hist_idx.unwrap()].clone();
                                cursor = buffer.len();
                                self.redraw_line(&buffer, cursor)?;
                            }
                        }
                        b'B' => {
                            // Down arrow
                            if let Some(idx) = hist_idx {
                                if idx + 1 >= self.history.len() {
                                    hist_idx = None;
                                    buffer.clear();
                                    cursor = 0;
                                } else {
                                    hist_idx = Some(idx + 1);
                                    buffer = self.history[hist_idx.unwrap()].clone();
                                    cursor = buffer.len();
                                }
                                self.redraw_line(&buffer, cursor)?;
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }

            // Printable characters
            if (32..127).contains(&ch) {
                if buffer.len() < LINE_MAX {
                    buffer.insert(cursor, ch as char);
                    cursor += 1;
                    self.redraw_line(&buffer, cursor)?;
                }
            }
        }
    }

    fn redraw_line(&self, buffer: &str, cursor: usize) -> io::Result<()> {
        let mut stdout = io::stdout();
        // Clear line and redraw
        stdout.write_all(b"\r\x1b[2K> ")?;
        stdout.write_all(buffer.as_bytes())?;
        
        // Move cursor to correct position
        if cursor < buffer.len() {
            let move_back = buffer.len() - cursor;
            for _ in 0..move_back {
                stdout.write_all(b"\x1b[D")?;
            }
        }
        stdout.flush()
    }

    fn complete_at(&self, buffer: &mut String, cursor: &mut usize) -> io::Result<()> {
        let token_start = buffer[..*cursor]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);

        let prefix = &buffer[token_start..*cursor];
        let is_first_token = buffer[..token_start].trim().is_empty();

        let matches = if is_first_token && !is_path_like(prefix) {
            self.complete_command(prefix)
        } else {
            self.complete_path(prefix)
        };

        if matches.len() == 1 {
            let completion = &matches[0][prefix.len()..];
            buffer.insert_str(*cursor, completion);
            *cursor += completion.len();
        }

        Ok(())
    }

    fn complete_command(&self, prefix: &str) -> Vec<String> {
        let mut matches = Vec::new();

        // Search PATH
        if let Ok(path) = env::var("PATH") {
            for dir in path.split(':') {
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        if let Ok(name) = entry.file_name().into_string() {
                            if name.starts_with(prefix) && matches.len() < MATCH_MAX {
                                matches.push(name);
                            }
                        }
                    }
                }
            }
        }

        // Add builtins
        for builtin in &["cd", "exit", "export", "help", "pwd"] {
            if builtin.starts_with(prefix) {
                matches.push(builtin.to_string());
            }
        }

        matches.sort();
        matches.dedup();
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
                        && matches.len() < MATCH_MAX
                    {
                        let full = if dir == "." {
                            name
                        } else {
                            format!("{}{}", prefix[..prefix.len() - base.len()].to_string(), name)
                        };
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
