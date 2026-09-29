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

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{LineWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

/// A writer both threads say things through, locked a whole line at a time
/// so that an `info` line and a `readyok` cannot interleave. It also keeps
/// the debug log, since every line the engine says passes through here.
pub struct SharedWriter<W: Write>(Arc<Mutex<Sink<W>>>);

/// A line read, numbered in the order read, so the session loop can say
/// which one it has reached.
pub(crate) type Heard = (u64, String);

/// The interface's output and, while one is open, the debug log beside it.
///
/// The log has what was read with `>> ` in front and what was said with `<< `.
/// A line is written to the log when the reader reads it, if the log is open
/// then. One read while it is closed is held here until the session loop
/// reaches it, because the lines already queued when the option that opens
/// the log is dispatched belong in it too. Those go in when the log opens,
/// after anything said since they were read. A line read while one log is
/// open goes to that one, even if the loop later moves the log elsewhere.
pub(crate) struct Sink<W> {
    out: W,
    log: Option<LineWriter<File>>,
    /// Whether the output is at the start of a line, where the log's next
    /// `<< ` goes.
    at_line_start: bool,
    /// The lines read while the log was closed that the loop has not reached.
    held: VecDeque<Heard>,
}

impl<W: Write> SharedWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self(Arc::new(Mutex::new(Sink {
            out: inner,
            log: None,
            at_line_start: true,
            held: VecDeque::new(),
        })))
    }

    /// A poisoned lock is a panic already reported on the other thread, and
    /// the interface still wants its answer, so the sink is used as it is.
    fn lock(&self) -> MutexGuard<'_, Sink<W>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The reader has read a line it answers itself.
    fn heard_unqueued(&self, line: &str) {
        self.lock().log_input(line);
    }

    /// The reader has read a line for the session loop.
    fn heard(&self, (number, line): &Heard) {
        let mut sink = self.lock();
        if sink.log.is_some() {
            sink.log_input(line);
        } else {
            sink.held.push_back((*number, line.clone()));
        }
    }

    /// The session loop has reached a line, so it is no longer held.
    fn reached(&self, number: u64) {
        let mut sink = self.lock();
        if sink.held.front().is_some_and(|(held, _)| *held == number) {
            sink.held.pop_front();
        }
    }

    /// Opens the debug log at `path`, appending to what is there, and writes
    /// the lines read that have not been reached. A log already open is
    /// closed first, unless the new one cannot be opened. The line that
    /// opened it goes first, unless a log was open when it was read and has
    /// it already.
    pub(crate) fn open_log(&self, path: &Path, opened_by: &str) -> std::io::Result<()> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut sink = self.lock();
        let written = sink.log.is_some();
        sink.log = Some(LineWriter::new(file));
        if !written {
            sink.log_input(opened_by);
        }
        while let Some((_, line)) = sink.held.pop_front() {
            sink.log_input(&line);
        }
        Ok(())
    }

    pub(crate) fn close_log(&self) {
        self.lock().log = None;
    }
}

impl<W: Write> Sink<W> {
    /// A failed write to the log is let go, since the interface's answers
    /// matter more than the record of them.
    fn log_input(&mut self, line: &str) {
        let Some(log) = &mut self.log else {
            return;
        };
        // every line is said whole under the lock, so this is a guard only
        if !self.at_line_start {
            let _ = log.write_all(b"\n");
            self.at_line_start = true;
        }
        let _ = writeln!(log, ">> {}", line);
    }

    fn log_output(&mut self, said: &[u8]) {
        for piece in said.split_inclusive(|byte| *byte == b'\n') {
            if let Some(log) = &mut self.log {
                if self.at_line_start {
                    let _ = log.write_all(b"<< ");
                }
                let _ = log.write_all(piece);
            }
            self.at_line_start = piece.ends_with(b"\n");
        }
    }
}

impl<W: Write> Write for Sink<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.out.write(buf)?;
        self.log_output(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
impl SharedWriter<Vec<u8>> {
    /// What has been said so far, while the session still holds the writer.
    pub(crate) fn read_back(&self) -> String {
        String::from_utf8(self.lock().out.clone()).unwrap()
    }
}

// derived Clone would want W: Clone, which is not what is being cloned
impl<W: Write> Clone for SharedWriter<W> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<W: Write> Write for SharedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.lock().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.lock().flush()
    }

    /// One lock for a whole line, where the default takes one per piece.
    fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        self.lock().write_fmt(args)
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
    /// The `go`s the reader has read, and those the loop has answered. A
    /// go is searching from the moment it is read, since an `isready` read
    /// behind it would otherwise queue behind a search that holds its
    /// answer for a stop. Counted, where one flag would be lowered by the
    /// first of two queued gos while the second is still to run.
    gos_read: Arc<AtomicU64>,
    gos_answered: Arc<AtomicU64>,
    /// Up while a `stop` has been read that the loop has not dispatched. A
    /// stop reaches the loop after the `go` it follows, so a search sees the
    /// flag up exactly when a stop sent after its `go` has been read. One
    /// flag set and cleared by the two threads without these counts would
    /// let the loop's clearing of one stop take a later one with it.
    stop: Arc<AtomicBool>,
    /// Changed only with the flag, under this lock, so neither thread can
    /// set it from a count the other has since moved.
    stops: Arc<Mutex<Stops>>,
    /// Whether a reader thread attends the session. A held answer waits on a
    /// stop only a reader can send, so a session without one answers at once.
    attended: bool,
    /// The session's thread, woken from a held answer.
    session: thread::Thread,
}

/// The `stop`s read and dispatched so far, `quit` and a closed pipe counted
/// as read.
#[derive(Default)]
struct Stops {
    read: u64,
    dispatched: u64,
}

impl SessionControl {
    fn for_this_thread() -> Self {
        Self {
            gos_read: Arc::new(AtomicU64::new(0)),
            gos_answered: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            stops: Arc::new(Mutex::new(Stops::default())),
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

    /// Whether a go read has yet to be answered. A go counts as answered
    /// just before its bestmove is written, so an `isready` sent after the
    /// bestmove is passed on to the loop.
    fn searching(&self) -> bool {
        self.gos_answered.load(Ordering::Acquire) < self.gos_read.load(Ordering::Acquire)
    }

    fn go_read(&self) {
        self.gos_read.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn answered(&self) {
        self.gos_answered.fetch_add(1, Ordering::Release);
    }

    fn stops(&self) -> std::sync::MutexGuard<'_, Stops> {
        self.stops.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counted whether or not a search is running: a `stop` typed just after
    /// a `go` may be read before the session begins searching, and must
    /// still stop that search.
    fn ask_to_stop(&self) {
        let mut stops = self.stops();
        stops.read += 1;
        self.stop.store(true, Ordering::Release);
        drop(stops);
        self.session.unpark();
    }

    /// The loop has reached a `stop`, which is spent: the flag stays up only
    /// for one read after it.
    pub(crate) fn stop_dispatched(&self) {
        let mut stops = self.stops();
        stops.dispatched += 1;
        self.stop
            .store(stops.read > stops.dispatched, Ordering::Release);
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

/// The reader thread. It answers `isready` while a go it has read is
/// unanswered, whether or not the loop has begun it, and passes every
/// other line on in order. Nothing is dropped: a `position` thrown away would
/// leave the interface and the engine silently on different games.
fn read_ahead<I, W>(
    input: I,
    mut out: SharedWriter<W>,
    control: &SessionControl,
    lines: Sender<Heard>,
) where
    I: Iterator<Item = std::io::Result<String>>,
    W: Write,
{
    let mut sent = 0;
    for line in input {
        let line = match line {
            Ok(line) => line,
            // a line that is not utf-8 (a log path in a windows code page,
            // say) has been consumed whole, so it is dropped and the next
            // read. Any other error leaves as the pipe closing does
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                let _ = writeln!(out, "info string dropped a line that is not utf-8");
                continue;
            }
            Err(error) => {
                let _ = writeln!(out, "info string could not read input: {}", error);
                break;
            }
        };
        // answered here and never queued, so it is never held either
        if first_word(&line) == "isready" && control.searching() {
            out.heard_unqueued(&line);
            let _ = writeln!(out, "readyok");
            continue;
        }
        let heard = (sent, line);
        out.heard(&heard);
        match first_word(&heard.1) {
            // passed on as well, since the search still owes a bestmove
            "stop" | "quit" => control.ask_to_stop(),
            "go" => control.go_read(),
            _ => {}
        }
        if lines.send(heard).is_err() {
            return;
        }
        sent += 1;
    }
    // the pipe closing is the interface leaving, and reads as a quit: a
    // search or a held answer would otherwise outlive it
    control.ask_to_stop();
    let _ = lines.send((sent, "quit".to_string()));
}

/// Hands lines to the handler in order until the input ends or the handler
/// returns false.
pub(crate) fn session_loop<W, H>(
    lines: Receiver<Heard>,
    out: &SharedWriter<W>,
    control: &SessionControl,
    mut handle: H,
) where
    W: Write,
    H: FnMut(&str, &SessionControl) -> bool,
{
    for (number, line) in lines {
        out.reached(number);
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
    let heard = out.clone();
    thread::spawn(move || {
        read_ahead(input(), heard, &reader, sender);
    });
    session_loop(lines, &out, &control, handle);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A log file of the test's own, removed before and after.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn named(name: &str) -> Self {
            let file = format!("arche-{}-{}.log", name, std::process::id());
            let path = std::env::temp_dir().join(file);
            let _ = std::fs::remove_file(&path);
            Self(path)
        }

        pub(crate) fn read(&self) -> String {
            std::fs::read_to_string(&self.0).unwrap_or_default()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn heard(out: &SharedWriter<Vec<u8>>, number: u64, line: &str) {
        out.heard(&(number, line.to_string()));
    }

    #[test]
    fn lines_queued_when_the_log_opens_follow_the_line_that_opened_it() {
        let log = Scratch::named("queued");
        let out = SharedWriter::new(Vec::new());
        // the reader drains the pipe before the loop reaches the option
        heard(&out, 0, "setoption name Debug Log File value x");
        heard(&out, 1, "position startpos");
        heard(&out, 2, "go depth 1");
        out.reached(0);
        out.open_log(&log.0, "setoption name Debug Log File value x")
            .unwrap();
        out.reached(1);
        let _ = writeln!(out.clone(), "readyok");
        heard(&out, 3, "stop");
        assert_eq!(
            log.read(),
            ">> setoption name Debug Log File value x\n\
             >> position startpos\n\
             >> go depth 1\n\
             << readyok\n\
             >> stop\n"
        );
    }

    #[test]
    fn a_line_reached_that_was_never_held_releases_no_other() {
        let log = Scratch::named("reached");
        let out = SharedWriter::new(Vec::new());
        out.open_log(&log.0, "open").unwrap();
        heard(&out, 0, "close");
        heard(&out, 1, "written before the close was reached");
        out.reached(0);
        out.close_log();
        heard(&out, 2, "reopen");
        heard(&out, 3, "held behind the reopen");
        // line 1 was written, not held, so reaching it must leave 2 and 3
        out.reached(1);
        out.reached(2);
        out.open_log(&log.0, "reopen").unwrap();
        assert!(
            log.read()
                .ends_with(">> reopen\n>> held behind the reopen\n"),
            "the log reads: {}",
            log.read()
        );
    }

    #[test]
    fn setting_the_log_again_does_not_write_its_line_twice() {
        // an interface may resend its options before every game
        let log = Scratch::named("again");
        let out = SharedWriter::new(Vec::new());
        heard(&out, 0, "set");
        out.reached(0);
        out.open_log(&log.0, "set").unwrap();
        heard(&out, 1, "set");
        out.reached(1);
        out.open_log(&log.0, "set").unwrap();
        assert_eq!(log.read(), ">> set\n>> set\n");
    }

    #[test]
    fn output_is_marked_a_line_at_a_time_however_it_is_written() {
        let log = Scratch::named("output");
        let mut out = SharedWriter::new(Vec::new());
        out.open_log(&log.0, "go").unwrap();
        out.write_all(b"info depth 1").unwrap();
        out.write_all(b" score cp 20\nbestmove e2e4\n").unwrap();
        out.close_log();
        let _ = writeln!(out, "not logged");
        assert_eq!(
            log.read(),
            ">> go\n<< info depth 1 score cp 20\n<< bestmove e2e4\n"
        );
        assert_eq!(
            out.read_back(),
            "info depth 1 score cp 20\nbestmove e2e4\nnot logged\n"
        );
    }

    fn up(control: &SessionControl) -> bool {
        control.handle().load(Ordering::Acquire)
    }

    #[test]
    fn a_stop_is_spent_by_its_own_dispatch_and_no_other() {
        let control = SessionControl::for_this_thread();
        // two stops read before the loop reaches the first
        control.ask_to_stop();
        control.ask_to_stop();
        assert!(up(&control));
        control.stop_dispatched();
        assert!(up(&control), "the first stop's dispatch spent the second");
        control.stop_dispatched();
        assert!(!up(&control), "a spent stop would reach the next search");
    }

    /// No loop runs here, so the go is still queued when the isready is
    /// read. Passed on, the isready would wait behind a search that holds
    /// its answer for a stop the interface will not send until it is ready.
    #[test]
    fn an_isready_read_behind_a_queued_go_is_answered_by_the_reader() {
        let control = SessionControl::for_this_thread();
        let (sender, lines) = channel();
        let said = SharedWriter::new(Vec::new());
        let input = ["go infinite", "isready"].map(|line| Ok(line.to_string()));
        read_ahead(input.into_iter(), said.clone(), &control, sender);
        assert_eq!(said.read_back(), "readyok\n");
        let passed: Vec<String> = lines.into_iter().map(|(_, line)| line).collect();
        assert_eq!(passed, ["go infinite", "quit"]);
    }

    /// `BufRead::lines` has consumed the bad line by the time it reports it,
    /// so the next line reads normally. The reader used to leave as the pipe
    /// closing does, taking the session with it.
    #[test]
    fn a_line_that_is_not_utf8_is_dropped_and_the_rest_read() {
        let control = SessionControl::for_this_thread();
        let (sender, lines) = channel();
        let said = SharedWriter::new(Vec::new());
        let input = [
            Ok("isready".to_string()),
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )),
            Ok("isready".to_string()),
        ];
        read_ahead(input.into_iter(), said.clone(), &control, sender);
        assert_eq!(
            said.read_back(),
            "info string dropped a line that is not utf-8\n"
        );
        let passed: Vec<String> = lines.into_iter().map(|(_, line)| line).collect();
        assert_eq!(passed, ["isready", "isready", "quit"]);
    }

    #[test]
    fn a_stop_dispatched_with_none_read_raises_nothing() {
        // an unattended session dispatches stops no reader counted
        let control = SessionControl::unattended();
        control.stop_dispatched();
        assert!(!up(&control));
    }
}
