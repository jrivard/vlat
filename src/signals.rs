// vlat - A colorful, pretty and over-engineered yet easy to use ping monitoring utility.
// Copyright (C) 2026  Jason D. Rivard <code@jrivard.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Graceful handling of SIGINT, SIGTERM and SIGHUP.
//!
//! While the TUI's main loop is running it `claim`s signals: the first one is
//! forwarded over a channel so the loop can quit through its normal exit path
//! (session save, terminal restore, summary). Outside the loop - startup, the
//! session picker, final cleanup - or on a second signal, the process restores
//! the terminal as best it can and exits immediately with 128 + signal number.
//!
//! Interactive Ctrl-C never reaches here: the TUI runs in raw mode, where it
//! arrives as a key event. These signals come from `kill`, `timeout`, service
//! managers, or a closed terminal.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Signal { Int, Term, Hup }

impl Signal {
    pub fn name(self) -> &'static str {
        match self { Signal::Int => "SIGINT", Signal::Term => "SIGTERM", Signal::Hup => "SIGHUP" }
    }

    /// Conventional shell exit status for a process ended by this signal.
    pub fn exit_code(self) -> i32 {
        128 + match self { Signal::Int => 2, Signal::Term => 15, Signal::Hup => 1 }
    }
}

static FORWARD:    Mutex<Option<mpsc::UnboundedSender<Signal>>> = Mutex::new(None);
static ALT_SCREEN: AtomicBool = AtomicBool::new(false);

/// Install the signal listener. Call once, early, from inside the tokio runtime.
pub fn install() {
    #[cfg(unix)]
    tokio::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let (Ok(mut int), Ok(mut term), Ok(mut hup)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else { return };
        loop {
            let sig = tokio::select! {
                _ = int.recv()  => Signal::Int,
                _ = term.recv() => Signal::Term,
                _ = hup.recv()  => Signal::Hup,
            };
            // Forward at most once; `take` means any later signal force-exits.
            let forwarded = FORWARD.lock().unwrap().take().is_some_and(|tx| tx.send(sig).is_ok());
            if !forwarded { force_exit(sig); }
        }
    });
}

/// Route the next signal to the returned receiver instead of exiting.
pub fn claim() -> mpsc::UnboundedReceiver<Signal> {
    let (tx, rx) = mpsc::unbounded_channel();
    *FORWARD.lock().unwrap() = Some(tx);
    rx
}

/// Stop routing signals to the main loop; later signals exit immediately.
pub fn release() {
    FORWARD.lock().unwrap().take();
}

/// Record whether the alternate screen is active, so `force_exit` can leave it.
pub fn set_alt_screen(on: bool) {
    ALT_SCREEN.store(on, Ordering::Relaxed);
}

/// Immediate exit: best-effort terminal restore, then 128 + signal number.
fn force_exit(sig: Signal) -> ! {
    restore_terminal();
    crate::logfile::write(&format!("{} received, exiting immediately", sig.name()));
    std::process::exit(sig.exit_code());
}

/// Best-effort terminal restore (raw mode, alternate screen, cursor). Write
/// errors are ignored: after SIGHUP the terminal is usually gone.
pub fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let mut out = std::io::stdout();
    if ALT_SCREEN.load(Ordering::Relaxed) {
        let _ = crossterm::execute!(out, crossterm::terminal::LeaveAlternateScreen);
    }
    let _ = crossterm::execute!(out, crossterm::cursor::Show);
    let _ = write!(out, "\r\n");
    let _ = out.flush();
}
