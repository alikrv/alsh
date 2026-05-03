// src/terminal.rs
use nix::sys::termios::{self, LocalFlags, InputFlags, ControlFlags, OutputFlags, SetArg, Termios};
use std::io::{self, Write};

const PROMPT: &str = "> ";

pub struct Terminal {
    orig_termios: Termios,
}

impl Terminal {
    pub fn new() -> io::Result<Self> {
        let stdin = io::stdin();
        let orig_termios = termios::tcgetattr(&stdin)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let mut terminal = Terminal { orig_termios };
        terminal.enable_raw_mode();

        Ok(terminal)
    }

    pub fn enable_raw_mode(&mut self) {
        let stdin = io::stdin();

        // First restore to original, then apply raw mode
        // This ensures a clean state transition
        termios::tcsetattr(&stdin, SetArg::TCSAFLUSH, &self.orig_termios).ok();

        let mut raw = self.orig_termios.clone();

        // Disable input processing
        raw.input_flags &= !(InputFlags::BRKINT | InputFlags::ICRNL | InputFlags::INPCK |
                            InputFlags::ISTRIP | InputFlags::IXON);

        // Disable output processing
        raw.output_flags &= !OutputFlags::OPOST;

        // Set character size to 8 bits
        raw.control_flags |= ControlFlags::CS8;

        // Disable canonical mode, echo, and signals
        raw.local_flags &= !(LocalFlags::ECHO | LocalFlags::ICANON |
                            LocalFlags::IEXTEN | LocalFlags::ISIG);

        // Set read timeout
        raw.control_chars[termios::SpecialCharacterIndices::VMIN as usize] = 0;
        raw.control_chars[termios::SpecialCharacterIndices::VTIME as usize] = 1;

        termios::tcsetattr(&stdin, SetArg::TCSAFLUSH, &raw).ok();

        // Flush any pending input
        io::
        stdout()
        .flush()
        .ok();
    }

    pub fn draw_prompt(&self) {
        let clear = "\r\x1b[2K";
        print!("{}{}", clear, PROMPT);
        io::stdout().flush().unwrap();
    }

    pub fn restore_for_child(&self) {
        let stdin = io::stdin();
        termios::tcsetattr(&stdin, SetArg::TCSAFLUSH, &self.orig_termios).ok();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let stdin = io::stdin();
        termios::tcsetattr(&stdin, SetArg::TCSAFLUSH, &self.orig_termios).ok();
    }
}
