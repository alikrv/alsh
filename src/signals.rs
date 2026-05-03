use nix::libc;
use nix::sys::signal::{signal, SigHandler, Signal};
use std::sync::atomic::{AtomicBool, Ordering};

static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigint(_: libc::c_int) {
    SIGINT_RECEIVED.store(true, Ordering::SeqCst);
}

pub fn install_signal_handlers() {
    unsafe {
        signal(Signal::SIGINT, SigHandler::Handler(handle_sigint)).ok();
        signal(Signal::SIGQUIT, SigHandler::SigIgn).ok();
        signal(Signal::SIGTSTP, SigHandler::SigIgn).ok();
        signal(Signal::SIGTTIN, SigHandler::SigIgn).ok();
        signal(Signal::SIGTTOU, SigHandler::SigIgn).ok();
    }
}

pub fn reset_signals_for_child() {
    unsafe {
        signal(Signal::SIGINT, SigHandler::SigDfl).ok();
        signal(Signal::SIGQUIT, SigHandler::SigDfl).ok();
        signal(Signal::SIGTSTP, SigHandler::SigDfl).ok();
        signal(Signal::SIGTTIN, SigHandler::SigDfl).ok();
        signal(Signal::SIGTTOU, SigHandler::SigDfl).ok();
    }
}

pub fn take_sigint() -> bool {
    SIGINT_RECEIVED.swap(false, Ordering::SeqCst)
}

pub fn clear_sigint() {
    SIGINT_RECEIVED.store(false, Ordering::SeqCst);
}
