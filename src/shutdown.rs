//! Signal-safe shutdown for SIGTERM, SIGHUP and SIGINT.
//!
//! The handlers only store atomics and never touch the terminal. The main loop
//! turns a pending signal into `Action::Quit`, so the terminal is restored by
//! the normal terminal-first teardown; afterwards [`terminate`] re-raises the
//! signal so the parent sees how the process ended. A second signal while the
//! first is pending runs the default action immediately, so a stuck loop stays
//! killable.

use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use signal_hook::{
    consts::{SIGHUP, SIGINT, SIGTERM},
    flag, low_level,
};

const SIGNALS: [i32; 3] = [SIGTERM, SIGHUP, SIGINT];

#[derive(Debug, Default)]
pub struct Shutdown {
    armed: Arc<AtomicBool>,
    signal: Arc<AtomicUsize>,
}

impl Shutdown {
    /// Registers the handlers. Call before the terminal is set up.
    pub fn install() -> io::Result<Self> {
        let shutdown = Self::default();
        for signal in SIGNALS {
            // Order matters: the default action must check the flag before
            // this delivery arms it.
            flag::register_conditional_default(signal, Arc::clone(&shutdown.armed))?;
            flag::register(signal, Arc::clone(&shutdown.armed))?;
            flag::register_usize(signal, Arc::clone(&shutdown.signal), signal as usize)?;
        }
        Ok(shutdown)
    }

    /// The signal that requested shutdown, if any.
    pub fn requested(&self) -> Option<i32> {
        match self.signal.load(Ordering::SeqCst) {
            0 => None,
            signal => i32::try_from(signal).ok(),
        }
    }
}

/// Ends the process with `signal`'s default action. Call only after the
/// terminal has been restored and the workers have stopped.
pub fn terminate(signal: i32) -> io::Result<()> {
    low_level::emulate_default_handler(signal)
}

#[cfg(test)]
impl Shutdown {
    pub(crate) fn simulate(&self, signal: i32) {
        self.armed.store(true, Ordering::SeqCst);
        self.signal.store(signal as usize, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_requested_until_a_signal_arrives() {
        assert_eq!(Shutdown::default().requested(), None);
    }

    #[test]
    fn the_received_signal_is_reported() {
        for signal in SIGNALS {
            let shutdown = Shutdown::default();
            shutdown.simulate(signal);
            assert_eq!(shutdown.requested(), Some(signal));
        }
    }
}
