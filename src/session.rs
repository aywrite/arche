// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The threads a session runs on, and what they share.
//!
//! A session is two threads: a reader that owns the input and acts on
//! `stop`, `quit` and `isready` at once, and the session loop, which hands
//! every line to the handler in the order sent. The engine stays on the
//! handler's thread.
//!
//! `wire` assembles both. A test that wants no reader calls `session_loop`
//! with the channel filled up front.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

/// A writer both threads say things through, locked a whole line at a time
/// so that an `info` line and a `readyok` cannot interleave.
pub struct SharedWriter<W: Write>(Arc<Mutex<W>>);

impl<W: Write> SharedWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self(Arc::new(Mutex::new(inner)))
    }
}

#[cfg(test)]
impl SharedWriter<Vec<u8>> {
    /// What has been said so far, while the session still holds the writer.
    pub(crate) fn read_back(&self) -> String {
        let buffer = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8(buffer.clone()).unwrap()
    }
}

// derived Clone would want W: Clone, which is not what is being cloned
impl<W: Write> Clone for SharedWriter<W> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<W: Write> Write for SharedWriter<W> {
    /// A poisoned lock is a panic already reported on the other thread, and
    /// the interface still wants its answer, so the buffer is used as it is.
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
    }

    /// One lock for a whole line, where the default takes one per piece.
    fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_fmt(args)
    }
}

/// The word a line opens with. The dispatcher and the reader both use it, so
/// they cannot disagree about what counts as a `stop`.
pub(crate) fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// What the reader thread and the session loop share.
#[derive(Clone)]
pub(crate) struct SessionControl {
    searching: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Whether a reader thread attends the session. A held answer waits on a
    /// stop only a reader can send, so a session without one answers at once.
    attended: bool,
    /// The session's thread, woken from a held answer.
    session: thread::Thread,
}

impl SessionControl {
    fn for_this_thread() -> Self {
        Self {
            searching: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            attended: true,
            session: thread::current(),
        }
    }

    #[cfg(test)]
    pub(crate) fn unattended() -> Self {
        Self {
            attended: false,
            ..Self::for_this_thread()
        }
    }

    pub(crate) fn searching(&self) -> bool {
        self.searching.load(Ordering::Acquire)
    }

    pub(crate) fn began_searching(&self) {
        self.searching.store(true, Ordering::Release);
    }

    /// The search has answered: both flags come down. A `stop` read after
    /// this still reaches the dispatch, which clears the flag again, so it
    /// does not stop the next search.
    pub(crate) fn answered(&self) {
        self.searching.store(false, Ordering::Release);
        self.stop.store(false, Ordering::Release);
    }

    /// Set whether or not a search is running: a `stop` typed just after a
    /// `go` may be read before the session begins searching, and must still
    /// stop that search.
    fn ask_to_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.session.unpark();
    }

    pub(crate) fn clear(&self) {
        self.stop.store(false, Ordering::Release);
    }

    pub(crate) fn handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Sit on a finished search's answer until a `stop` arrives, as `go
    /// infinite` promises. With no reader the answer is given at once.
    pub(crate) fn wait_for_stop(&self) {
        if !self.attended {
            return;
        }
        while !self.stop.load(Ordering::Acquire) {
            thread::park();
        }
    }
}

/// Says on the interface's own channel why the engine died.
///
/// A GUI discards stderr, so this writes one `info string` for its log and
/// then calls the previous hook, which keeps the backtrace on stderr.
///
/// The lock is only tried: a panic raised inside a write finds it held by
/// this thread, and blocking would hang the report and the backtrace both.
/// That panic is reported on stderr alone.
pub(crate) fn report_panics_to<W: Write + Send + 'static>(out: SharedWriter<W>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let message = if let Some(text) = panic.payload().downcast_ref::<&str>() {
            text
        } else if let Some(text) = panic.payload().downcast_ref::<String>() {
            text.as_str()
        } else {
            "no message"
        };
        // poisoned by another thread's panic, already reported
        let held = match out.0.try_lock() {
            Ok(out) => Some(out),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        if let Some(mut out) = held {
            match panic.location() {
                Some(at) => {
                    let _ = writeln!(out, "info string panicked at {}: {}", at, message);
                }
                None => {
                    let _ = writeln!(out, "info string panicked: {}", message);
                }
            }
        }
        previous(panic);
    }));
}

/// The reader thread. It answers `isready` during a search and passes every
/// other line on in order. Nothing is dropped: a `position` thrown away would
/// leave the interface and the engine silently on different games.
fn read_ahead<I, W>(input: I, mut out: W, control: &SessionControl, lines: Sender<String>)
where
    I: Iterator<Item = std::io::Result<String>>,
    W: Write,
{
    for line in input {
        let line = match line {
            Ok(line) => line,
            // leave as the pipe closing does
            Err(error) => {
                let _ = writeln!(out, "info string could not read input: {}", error);
                break;
            }
        };
        match first_word(&line) {
            // passed on as well, since the search still owes a bestmove
            "stop" | "quit" => control.ask_to_stop(),
            "isready" if control.searching() => {
                let _ = writeln!(out, "readyok");
                continue;
            }
            _ => {}
        }
        if lines.send(line).is_err() {
            return;
        }
    }
    // the pipe closing is the interface leaving, and reads as a quit: a
    // search or a held answer would otherwise outlive it
    control.ask_to_stop();
    let _ = lines.send("quit".to_string());
}

/// Hands lines to the handler in order until the input ends or the handler
/// returns false.
pub(crate) fn session_loop<H>(lines: Receiver<String>, control: &SessionControl, mut handle: H)
where
    H: FnMut(&str, &SessionControl) -> bool,
{
    for line in lines {
        if !handle(&line, control) {
            return;
        }
    }
}

/// Runs the reader on a thread of its own and the session loop on this one,
/// which is where the handler is called. The input is built on the reader's
/// thread (stdin's lock lives there), so what crosses is the function that
/// makes it.
pub(crate) fn wire<W, I, F, H>(out: SharedWriter<W>, input: F, handle: H)
where
    W: Write + Send + 'static,
    I: Iterator<Item = std::io::Result<String>>,
    F: FnOnce() -> I + Send + 'static,
    H: FnMut(&str, &SessionControl) -> bool,
{
    let control = SessionControl::for_this_thread();
    let (sender, lines) = channel();
    let reader = control.clone();
    thread::spawn(move || {
        read_ahead(input(), out, &reader, sender);
    });
    session_loop(lines, &control, handle);
}
