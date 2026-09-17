// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The threads a session runs on, and what they share.
//!
//! A session is two threads: a reader that owns the input and answers what
//! must be answered while a search is running, and the session loop, which
//! hands lines to the handler in the order they were sent. What a line means
//! is the handler's business; the reader knows only the three words it must
//! act on before the loop would get to them. The engine stays on the
//! caller's side of the handler and never crosses a thread.
//!
//! `wire` is the one assembly of both threads. The binary enters it with
//! stdin and the driven tests with a channel. A test that wants no reader
//! calls `session_loop` on its own, filling the channel up front.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

/// A writer two threads say things through. The lock is taken for a whole
/// line at a time, or an `info` line and a `readyok` could meet halfway
/// through each other.
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
    /// A poisoned lock is a panic on the other thread, already reported where
    /// it happened; an interface still wants its answer, so the buffer is
    /// taken as it stands.
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

    /// One lock for a whole line. The default writes each piece of the format
    /// separately and would let the other thread in between two of them.
    fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_fmt(args)
    }
}

/// The word a line opens with, which is all a command is. The dispatcher and
/// the reader thread both read a line by this, so they cannot disagree about
/// what counts as a `stop`.
pub(crate) fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// What the reader thread and the session loop share: whether a search is
/// under way, the flag that stops it, and the thread to wake when that flag
/// is set.
#[derive(Clone)]
pub(crate) struct SessionControl {
    searching: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Whether a reader thread attends the session. A held answer waits on a
    /// stop only a reader can send, so a session without one answers at once.
    attended: bool,
    /// The session's own thread, to wake from a held answer.
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

    /// A control for a session no reader attends.
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

    /// The search has answered: both flags come down. A `stop` read in this
    /// gap is not lost, since the reader also passes every `stop` down the
    /// channel and the dispatch clears the flag again there.
    pub(crate) fn answered(&self) {
        self.searching.store(false, Ordering::Release);
        self.stop.store(false, Ordering::Release);
    }

    /// Ask the search to stop. Set whether or not one is running: a `stop`
    /// typed the instant after a `go` may be read before the session has
    /// begun searching, and a flag set early stops the search that follows
    /// rather than being lost.
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

    /// Sit on a finished search's answer until a `stop` arrives, which is
    /// what `go infinite` promises. With no reader to send one the answer is
    /// given at once.
    pub(crate) fn wait_for_stop(&self) {
        if !self.attended {
            return;
        }
        while !self.stop.load(Ordering::Acquire) {
            thread::park();
        }
    }
}

/// Says on the interface's own channel why the engine died, before it does.
///
/// A panic writes to stderr, which a chess GUI discards, so the process just
/// disappears mid game. This hook writes one `info string` to the session's
/// writer so the reason lands in the GUI's log, then hands over to the hook
/// already installed, which keeps the backtrace on stderr.
///
/// The lock is taken only if it is free: a panic raised by the very write the
/// lock was taken for would find it held by this thread, and a hook that
/// blocked there would hang the report and the backtrace both. An engine
/// dying that way says its piece on stderr alone.
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
        // a poisoned lock is some other thread's panic, already reported
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

/// The reader thread: every line the interface sends arrives here first.
///
/// While a search is running it answers what the protocol says must be
/// answered at once and passes everything else on to be handled after the
/// `bestmove`. Nothing is dropped: a `position` thrown away would leave the
/// interface's idea of the game and the engine's silently apart.
fn read_ahead<I, W>(input: I, mut out: W, control: &SessionControl, lines: Sender<String>)
where
    I: Iterator<Item = std::io::Result<String>>,
    W: Write,
{
    for line in input {
        let line = match line {
            Ok(line) => line,
            // leave the way the pipe closing does below
            Err(error) => {
                let _ = writeln!(out, "info string could not read input: {}", error);
                break;
            }
        };
        match first_word(&line) {
            // a quit stops the search as a stop does, and is then passed on:
            // the search still owes a bestmove
            "stop" | "quit" => control.ask_to_stop(),
            // the one answer the protocol requires mid-search
            "isready" if control.searching() => {
                let _ = writeln!(out, "readyok");
                continue;
            }
            _ => {}
        }
        if lines.send(line).is_err() {
            // the session has gone
            return;
        }
    }
    // the pipe closing is the interface leaving, and reads as the quit it
    // did not get to send: a search still running, or an answer held for a
    // stop that can no longer come, would outlive the only party that
    // wanted it
    control.ask_to_stop();
    let _ = lines.send("quit".to_string());
}

/// The session loop: lines the reader thread did not answer itself, handed
/// to the handler in order until the input ends or the handler says the
/// session is over.
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

/// Wire a session up: the input read on a thread of its own, the session
/// loop run on this one. The handler is called on this thread, so whatever
/// it closes over never crosses to the reader. The input is built on the
/// reader's thread too (stdin's lock lives its whole life there), so what
/// crosses is the recipe for it rather than the thing.
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
