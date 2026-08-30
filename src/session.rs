// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The threads a session runs on, and what they share.
//!
//! A session is two threads: a reader that owns the input and answers what
//! must be answered while a search is running, and the session loop, which
//! hands lines to the handler in the order they were sent. This module owns
//! the reader, the loop, the control they share, and the writer both speak
//! through. What a line means is none of its business: the loop hands each
//! line to the handler it was wired with, and the reader knows only the
//! three words it must act on before the loop would get to them. The engine
//! stays on the caller's side of that handler and never crosses a thread.
//!
//! `wire` is the one assembly of both threads. The binary enters it with
//! stdin and the driven tests enter it with a channel, and the two differ in
//! nothing else. A test that wants no reader calls `session_loop` on its
//! own, filling the channel up front.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

/// A writer two threads say things through.
///
/// The search speaks from the session thread and the reader thread answers
/// `isready` for itself, so both have one of these and the lock is taken for
/// a whole line at a time. Without that an `info` line and a `readyok` could
/// meet halfway through each other, and an interface reads lines.
pub struct SharedWriter<W: Write>(Arc<Mutex<W>>);

impl<W: Write> SharedWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self(Arc::new(Mutex::new(inner)))
    }
}

#[cfg(test)]
impl SharedWriter<Vec<u8>> {
    /// What has been said so far, for a test that reads the output while
    /// the session still holds the writer.
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
    /// A poisoned lock is a panic on the other thread, which has already
    /// been reported where it happened. There is nothing this can do about
    /// it and an interface still wants its answer, so the buffer is taken
    /// as it stands.
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

    /// One lock for a whole line. The default writes each piece of the
    /// format separately, which would take the lock several times and let
    /// the other thread in between two of them.
    fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_fmt(args)
    }
}

/// The word a line opens with, which is all a command is: the dispatcher
/// and the reader thread both read a line by this one function, so they
/// cannot disagree about what counts as a `stop`, and `stopwatch` is not
/// one.
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
    /// Whether a reader thread attends the session. A held answer waits
    /// on a stop only a reader can send, so a session without one answers
    /// at once: the reading the pipe closing gets, applied to a session
    /// that never had a pipe.
    attended: bool,
    /// The session's own thread. A search that has run out of depths to
    /// search sits waiting for a `stop`, and nothing else would wake it.
    session: thread::Thread,
}

impl SessionControl {
    /// A control whose session runs on the calling thread, with a reader
    /// attending it.
    fn for_this_thread() -> Self {
        Self {
            searching: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            attended: true,
            session: thread::current(),
        }
    }

    /// A control for a session no reader attends, which is how the tests
    /// drive the dispatcher one thread at a time.
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

    /// The search has answered: both flags come down. A `stop` read in
    /// this gap is no longer lost either way, since the reader also passes
    /// every `stop` down the channel and the dispatch clears the flag
    /// again there.
    pub(crate) fn answered(&self) {
        self.searching.store(false, Ordering::Release);
        self.stop.store(false, Ordering::Release);
    }

    /// Ask the search to stop. Set whether or not one is running: a `stop`
    /// typed the instant after a `go` may be read before the session has
    /// begun searching, and a flag set early stops the search that follows
    /// rather than being lost. The session clears it once it has answered,
    /// and the `stop` itself is passed on to clear it if it did not.
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
    /// what `go infinite` promises the interface. With no reader to send
    /// one the answer is given at once instead of held for ever.
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
/// A panic writes its message and a backtrace to stderr, which a chess GUI
/// reads never and discards always: the process just disappears mid game.
/// This hook writes one `info string` to the writer given (stdout, in the
/// binary) so the reason lands in the GUI's log, then hands over to the hook
/// that was already installed, which keeps the backtrace on stderr for
/// anyone running the engine by hand.
///
/// Takes the session's writer rather than assuming stdout, so the report
/// goes out through the same lock as every other line, and so a test can
/// install it against a buffer and read back what a panic would have said.
/// The lock is taken only if it is free: a panic raised by the very write
/// the lock was taken for would find it still held by this thread, and a
/// hook that blocked there would hang the report and the backtrace both.
/// An engine dying that way says its piece on stderr alone.
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
        // a poisoned lock is some other thread's panic, already reported;
        // this one still wants its say
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
/// interface's idea of the game and the engine's silently apart, and the
/// next `go` would search the wrong position.
fn read_ahead<I, W>(input: I, mut out: W, control: &SessionControl, lines: Sender<String>)
where
    I: Iterator<Item = std::io::Result<String>>,
    W: Write,
{
    for line in input {
        let line = match line {
            Ok(line) => line,
            // there is nothing left to read and no one to tell, so leave
            // the way the pipe closing does below
            Err(error) => {
                let _ = writeln!(out, "info string could not read input: {}", error);
                break;
            }
        };
        match first_word(&line) {
            // a quit stops the search as a stop does, and is then passed
            // on: the search still owes a bestmove, and it is emitted
            // before we exit
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
    // the pipe closing is the interface leaving. A search still running, or
    // an answer held for a stop that can no longer come, would outlive the
    // only party that wanted it, so the ending reads as the quit the
    // interface did not get to send
    control.ask_to_stop();
    let _ = lines.send("quit".to_string());
}

/// The session loop: lines the reader thread did not answer itself, handed
/// to the handler in the order they were sent, until the input ends or the
/// handler says the session is over.
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
/// loop run on this one, and the two joined by the channel, the control and
/// the shared writer. This is the one assembly of those parts: a driven test
/// session and a stdin session differ only in the input they hand it.
///
/// The handler is called on this thread, so whatever it closes over never
/// crosses to the reader; the reader is over there so that a `stop` is read
/// while a search is running rather than after it. The input is built on
/// the reader's thread too — stdin's lock lives its whole life there — so
/// what crosses is the recipe for it rather than the thing.
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
