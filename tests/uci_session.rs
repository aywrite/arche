// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Sessions against the real binary, the way an interface runs it.
//!
//! Everything else in the suite drives the engine in process, which covers
//! the parsing and the search and says nothing about the program: argument
//! handling, the stdin loop, the reader thread, exit codes. These spawn the
//! executable cargo built, script a session against its pipes, and assert on
//! the transcript — the docker smoke test's job, on every push and on every
//! platform the release ships for.
//!
//! Every wait has a deadline, so a binary that stops answering fails the
//! suite rather than hanging it, and the child is killed on drop so a failed
//! test cannot leave an engine searching behind the runner.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

/// Generous, because it bounds a real search on whatever machine runs the
/// suite rather than measuring one.
const DEADLINE: Duration = Duration::from_secs(30);

struct Session {
    child: Child,
    lines: Receiver<String>,
    said: Vec<String>,
}

impl Session {
    fn start(args: &[&str]) -> Session {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arche"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the binary cargo built should start");
        let stdout = child.stdout.take().expect("stdout was piped");
        let (sender, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        Session {
            child,
            lines,
            said: Vec::new(),
        }
    }

    fn say(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("stdin is piped");
        writeln!(stdin, "{}", line).expect("the engine is still reading");
    }

    /// Reads until a line satisfies the test, and returns it. Everything read
    /// on the way is kept for the failure message and for later asserts.
    fn wait_for(&mut self, what: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.said.push(line.clone());
                    if what(&line) {
                        return line;
                    }
                }
                Err(_) => panic!(
                    "nothing matching arrived in thirty seconds, only: {:#?}",
                    self.said
                ),
            }
        }
    }

    /// Nothing arrives for this long. The one assertion only waiting can
    /// make, so the span is short and the claim is "held", not "never".
    fn stays_quiet_for(&mut self, span: Duration) {
        if let Ok(line) = self.lines.recv_timeout(span) {
            panic!("expected silence, got {:?} (after {:#?})", line, self.said);
        }
    }

    /// Close the engine's stdin, which is what an interface dying does.
    fn hang_up(&mut self) {
        drop(self.child.stdin.take());
    }

    /// The exit status, or a failure if the process outlives the deadline.
    fn finished(&mut self) -> ExitStatus {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().expect("the child can be waited on") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the engine did not exit inside thirty seconds; said: {:#?}",
                self.said
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn looks_like_a_move(line: &str) -> bool {
    let Some(m) = line.strip_prefix("bestmove ") else {
        return false;
    };
    let m = m.as_bytes();
    m.len() >= 4
        && m[0].is_ascii_lowercase()
        && m[1].is_ascii_digit()
        && m[2].is_ascii_lowercase()
        && m[3].is_ascii_digit()
}

#[test]
fn the_handshake_answers_the_way_the_smoke_test_expects() {
    let mut s = Session::start(&[]);
    s.say("uci");
    s.wait_for(|l| l.starts_with("id name arche"));
    s.wait_for(|l| l == "uciok");
    s.say("isready");
    s.wait_for(|l| l == "readyok");
    s.say("position startpos");
    s.say("go movetime 200");
    let best = s.wait_for(|l| l.starts_with("bestmove"));
    assert!(looks_like_a_move(&best), "not a move: {}", best);
    s.say("quit");
    assert!(s.finished().success());
}

#[test]
fn a_stop_ends_an_infinite_search_with_a_real_move() {
    let mut s = Session::start(&[]);
    s.say("position startpos");
    s.say("go infinite");
    s.wait_for(|l| l.starts_with("info depth"));
    s.say("stop");
    let best = s.wait_for(|l| l.starts_with("bestmove"));
    assert!(looks_like_a_move(&best), "a stopped search said: {}", best);
    s.say("quit");
    assert!(s.finished().success());
}

#[test]
fn an_infinite_search_holds_its_answer_for_the_stop() {
    // black is mated, so the search is over at once; only the hold can be
    // what the silence is
    let mut s = Session::start(&[]);
    s.say("position fen 7k/6Q1/6K1/8/8/8/8/8 b - - 0 1");
    s.say("go infinite");
    s.stays_quiet_for(Duration::from_millis(400));
    s.say("stop");
    s.wait_for(|l| l == "bestmove 0000");
    s.say("quit");
    assert!(s.finished().success());
}

#[test]
fn the_interface_hanging_up_ends_the_engine() {
    // no quit: the pipe closing has to be enough, or a dead interface leaves
    // an engine searching for a stop that cannot come
    let mut s = Session::start(&[]);
    s.say("position startpos");
    s.say("go infinite");
    s.wait_for(|l| l.starts_with("info depth"));
    s.hang_up();
    s.wait_for(|l| l.starts_with("bestmove"));
    assert!(s.finished().success());
}

#[test]
fn a_stop_with_nothing_running_is_taken_in_silence() {
    let mut s = Session::start(&[]);
    s.say("stop");
    s.say("isready");
    let ready = s.wait_for(|l| l == "readyok");
    assert_eq!(ready, "readyok");
    assert!(
        !s.said.iter().any(|l| l.contains("unrecognised")),
        "the stop was complained about: {:#?}",
        s.said
    );
    s.say("quit");
    assert!(s.finished().success());
}

/// The nodes the deepest info line of one search reports. The search is run
/// from here, so the lines read are that search's own rather than every
/// line the session has said.
fn nodes_of_a_search(s: &mut Session, depth: u8) -> u64 {
    let from = s.said.len();
    s.say("position startpos");
    s.say(&format!("go depth {}", depth));
    s.wait_for(|l| l.starts_with("bestmove"));
    let info = s.said[from..]
        .iter()
        .rfind(|l| l.starts_with("info depth "))
        .unwrap_or_else(|| panic!("a search reported no depth: {:#?}", s.said));
    info.split_whitespace()
        .skip_while(|word| *word != "nodes")
        .nth(1)
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("no node count in {}", info))
}

#[test]
fn the_clear_hash_button_empties_the_table() {
    // the same search three times over: cold, then on the table the first
    // one left behind, then after the button. The middle one is cheaper for
    // what the table holds, and the third costs what the cold one did,
    // which is what an emptied table looks like from outside the engine
    let mut s = Session::start(&[]);
    s.say("setoption name Hash value 1");
    let cold = nodes_of_a_search(&mut s, 6);
    let warm = nodes_of_a_search(&mut s, 6);
    assert!(
        warm < cold,
        "the second search read nothing from the table: {} against {}",
        warm,
        cold
    );

    s.say("setoption name Clear Hash");
    s.say("isready");
    s.wait_for(|l| l == "readyok");
    let cleared = nodes_of_a_search(&mut s, 6);
    assert_eq!(cleared, cold, "the table still held the first search");
    assert!(
        !s.said.iter().any(|l| l.contains("unrecognised")),
        "the button was complained about: {:#?}",
        s.said
    );

    s.say("quit");
    assert!(s.finished().success());
}

/// The sharp middlegame the engine's own tests search, which has enough
/// going on at the root for a cut-short iteration to change its mind.
const SHARP_MIDDLEGAME: &str = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";

#[test]
fn the_move_a_swap_answers_with_opens_the_last_line_said() {
    // a node budget rather than a clock, so the iteration is cut short on
    // the same node on every machine and this is a fixture rather than a
    // race: this budget leaves an iteration with a better move than the
    // depth before it answered, which is the swap. Without a report of its
    // own the last line an interface read would open with the move being
    // given up, and fastchess calls that out on every move it happens on
    let mut s = Session::start(&[]);
    s.say(&format!("position fen {}", SHARP_MIDDLEGAME));
    s.say("go nodes 39894");
    let answer = s.wait_for(|l| l.starts_with("bestmove"));
    let best = answer
        .strip_prefix("bestmove ")
        .unwrap_or_else(|| panic!("not a bestmove: {}", answer));
    let info = s
        .said
        .iter()
        .rfind(|l| l.starts_with("info depth "))
        .unwrap_or_else(|| panic!("the search reported no depth: {:#?}", s.said))
        .clone();
    let first = info
        .split(" pv ")
        .nth(1)
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("no line in {}", info));
    assert_eq!(first, best, "the last line said: {}", info);
    assert!(
        info.contains(" lowerbound "),
        "a partial depth was reported as an exact score: {}",
        info
    );
    s.say("quit");
    assert!(s.finished().success());
}

#[test]
fn the_version_and_the_help_answer_and_exit_cleanly() {
    let mut version = Session::start(&["--version"]);
    version.wait_for(|l| l.starts_with("arche "));
    assert!(version.finished().success());

    let mut help = Session::start(&["--help"]);
    help.wait_for(|l| l.contains("bench"));
    assert!(help.finished().success());
}

#[test]
fn an_unrecognised_argument_fails_with_the_code_the_scripts_check() {
    let mut s = Session::start(&["--frobnicate"]);
    assert_eq!(s.finished().code(), Some(2));
}

#[test]
fn the_bench_argument_prints_the_line_the_match_tools_read() {
    let mut s = Session::start(&["bench", "1"]);
    s.wait_for(|l| {
        let mut words = l.split_whitespace();
        matches!(
            (words.next(), words.next(), words.next(), words.next()),
            (Some(n), Some("nodes"), Some(_), Some("nps")) if n.parse::<u64>().is_ok()
        )
    });
    assert!(s.finished().success());
}
