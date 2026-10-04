// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Sessions against the real binary: argument handling, the stdin loop, the
//! reader thread and exit codes, which the in process tests cannot see.
//!
//! Every wait has a deadline and the child is killed on drop, so a binary
//! that stops answering fails the suite rather than hanging it.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

/// Generous, because it bounds a real search on any machine.
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

    /// Reads until a line satisfies the test and returns it, keeping every
    /// line read in `said`.
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

    /// Nothing arrives for this long. The claim is "held", not "never".
    fn stays_quiet_for(&mut self, span: Duration) {
        if let Ok(line) = self.lines.recv_timeout(span) {
            panic!("expected silence, got {:?} (after {:#?})", line, self.said);
        }
    }

    /// Close the engine's stdin, which is what an interface dying does.
    fn hang_up(&mut self) {
        drop(self.child.stdin.take());
    }

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

    /// Says quit and checks the engine exits cleanly on it.
    fn quit(mut self) {
        self.say("quit");
        assert!(self.finished().success());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The move a bestmove line names.
fn move_of(answer: &str) -> &str {
    answer
        .strip_prefix("bestmove ")
        .unwrap_or_else(|| panic!("not a bestmove: {}", answer))
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
    s.quit();
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
    s.quit();
}

#[test]
fn an_infinite_search_holds_its_answer_for_the_stop() {
    // black is mated, so the search is over at once and only the hold can
    // be what the silence is
    let mut s = Session::start(&[]);
    s.say("position fen 7k/6Q1/6K1/8/8/8/8/8 b - - 0 1");
    s.say("go infinite");
    s.stays_quiet_for(Duration::from_millis(400));
    s.say("stop");
    s.wait_for(|l| l == "bestmove 0000");
    s.quit();
}

#[test]
fn the_interface_hanging_up_ends_the_engine() {
    // no quit: the pipe closing has to be enough
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
    s.quit();
}

#[test]
fn the_debug_log_holds_every_line_read_and_said_in_order() {
    let log = std::env::temp_dir().join(format!("arche-session-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let typed = [
        format!("setoption name Debug Log File value {}", log.display()),
        "uci".to_string(),
        "position startpos".to_string(),
        "go infinite".to_string(),
        "isready".to_string(),
        "stop".to_string(),
        "quit".to_string(),
    ];
    let mut s = Session::start(&[]);
    // the first four go at once, so the reader usually has them before the
    // option that opens the log is handled (the unit tests in session.rs
    // hold lines deterministically)
    for line in &typed[..4] {
        s.say(line);
    }
    s.wait_for(|l| l.starts_with("info depth"));
    // answered by the reader mid-search, not by the session loop
    s.say(&typed[4]);
    s.wait_for(|l| l == "readyok");
    s.say(&typed[5]);
    s.wait_for(|l| l.starts_with("bestmove"));
    s.say(&typed[6]);
    assert!(s.finished().success());
    // stdout is closed, so this takes what is left and ends
    while let Ok(line) = s.lines.recv_timeout(DEADLINE) {
        s.said.push(line);
    }

    let written = std::fs::read_to_string(&log).expect("the log was written");
    let _ = std::fs::remove_file(&log);
    let read: Vec<&str> = written
        .lines()
        .filter_map(|l| l.strip_prefix(">> "))
        .collect();
    let said: Vec<&str> = written
        .lines()
        .filter_map(|l| l.strip_prefix("<< "))
        .collect();
    assert_eq!(read, typed, "the log reads: {}", written);
    assert_eq!(said, s.said, "the log reads: {}", written);
    // each line in when it was read, not when the loop reached it
    let at = |start: &str| {
        written
            .lines()
            .position(|l| l.starts_with(start))
            .unwrap_or_else(|| panic!("no {:?} in the log: {}", start, written))
    };
    assert!(
        at(">> isready") < at("<< readyok"),
        "the log reads: {}",
        written
    );
    assert!(
        at(">> stop") < at("<< bestmove"),
        "the log reads: {}",
        written
    );
    assert!(
        written
            .lines()
            .all(|l| l.starts_with(">> ") || l.starts_with("<< ")),
        "the log reads: {}",
        written
    );
}

fn nodes_of(info: &str) -> u64 {
    info.split_whitespace()
        .skip_while(|word| *word != "nodes")
        .nth(1)
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("no node count in {}", info))
}

/// The nodes the last info line of one search reports.
fn nodes_of_a_search(s: &mut Session, depth: u8) -> u64 {
    let from = s.said.len();
    s.say("position startpos");
    s.say(&format!("go depth {}", depth));
    s.wait_for(|l| l.starts_with("bestmove"));
    let info = s.said[from..]
        .iter()
        .rfind(|l| l.starts_with("info depth "))
        .unwrap_or_else(|| panic!("a search reported no depth: {:#?}", s.said));
    nodes_of(info)
}

#[test]
fn the_clear_hash_button_empties_the_table() {
    // cold, warm, then after the button, which costs what the cold one did
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

    s.quit();
}

/// A middlegame with enough going on at the root for a cut-short iteration
/// to change its mind.
const SHARP_MIDDLEGAME: &str = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";

/// The bench's Italian opening, whose depth seven fails high.
const ITALIAN: &str = "r1bqk2r/pppp1ppp/2n2n2/2b1p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 0 1";

/// Kiwipete, the bench's tactical middlegame, whose depth five fails low.
const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";

/// WAC.021 of the tactical suite, which changes its root move at depth nine
/// and reports the new move as a floor first.
const WAC_021: &str = "5rk1/1b3p1p/pp3p2/3n1N2/1P6/P1qB1PP1/3Q3P/4R1K1 w - - 0 1";

#[test]
#[cfg_attr(
    feature = "machine-test",
    ignore = "pins a search made with the shipped factor table, which the test rank replaces"
)]
fn the_move_a_swap_answers_with_opens_the_last_line_said() {
    // a node budget, so the cut falls on the same node on every machine.
    // It has to land after an iteration finds its better move and before
    // that iteration ends: depth six answers d3e2 and finishes at 22,149
    // nodes, and depth seven reports d3b1 from 39,917 nodes on and finishes
    // at 42,904. The budget moves with the tree, in the commit that moved it
    let mut s = Session::start(&[]);
    s.say(&format!("position fen {}", SHARP_MIDDLEGAME));
    s.say("go nodes 41000");
    let answer = s.wait_for(|l| l.starts_with("bestmove"));
    let best = move_of(&answer);
    let info = s
        .said
        .iter()
        .rfind(|l| l.starts_with("info depth "))
        .unwrap_or_else(|| panic!("the search reported no depth: {:#?}", s.said));
    assert_eq!(line_opens_with(info), best, "the last line said: {}", info);
    assert!(
        info.contains(" lowerbound "),
        "a partial depth was reported as an exact score: {}",
        info
    );
    s.quit();
}

fn score_of(info: &str) -> i32 {
    info.split_whitespace()
        .skip_while(|word| *word != "cp")
        .nth(1)
        .and_then(|score| score.parse().ok())
        .unwrap_or_else(|| panic!("no centipawn score in {}", info))
}

fn line_opens_with(info: &str) -> &str {
    info.split(" pv ")
        .nth(1)
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("no line in {}", info))
}

#[test]
#[cfg_attr(
    feature = "machine-test",
    ignore = "pins a search made with the shipped factor table, which the test rank replaces"
)]
fn an_iteration_no_root_move_reached_answers_with_the_depth_before_it() {
    // Kiwipete is worth 31 at depth four, answered with e2a6, and depth five
    // fails low: it is reported as a ceiling (b2b3 at 1) at 8,566 nodes and
    // searched again wider, finishing at 10,820. The budget lands inside
    // that second search, which also reaches nothing above alpha, so depth
    // four's move still answers. The Italian opening was this fixture until
    // the pair term's refit at a ridge of 3e-7, under which it fails low at
    // no depth to ten
    let mut s = Session::start(&[]);
    s.say(&format!("position fen {}", KIWIPETE));
    s.say("go nodes 9700");
    let answer = s.wait_for(|l| l.starts_with("bestmove"));
    let best = move_of(&answer);

    let lines: Vec<&String> = s
        .said
        .iter()
        .filter(|l| l.starts_with("info depth "))
        .collect();
    let (total, iterations) = lines
        .split_last()
        .unwrap_or_else(|| panic!("the search reported no depth: {:#?}", s.said));
    let ceiling = iterations
        .last()
        .unwrap_or_else(|| panic!("only one depth was reported: {:#?}", s.said));
    assert!(
        ceiling.contains(" upperbound "),
        "the ceiling was not reported as one: {}",
        ceiling
    );
    // the ceiling names the move that came closest, which is not an answer
    assert_ne!(line_opens_with(ceiling), best, "the closest move answered");
    let completed = iterations
        .iter()
        .rfind(|l| !l.contains("bound "))
        .unwrap_or_else(|| panic!("no depth completed: {:#?}", s.said));
    assert_eq!(
        line_opens_with(completed),
        best,
        "the answer is not the last completed depth's: {}",
        completed
    );
    // and it really was a fail low
    assert!(
        score_of(ceiling) < score_of(completed),
        "the ceiling did not fall short of the answer: {} against {}",
        ceiling,
        completed
    );
    // the last line says the answering depth again with every node spent,
    // which is what a match harness copies into the game record. It used to
    // copy the ceiling, whose count leaves out the stopped iteration
    assert_eq!(
        line_opens_with(total),
        best,
        "the last line is not the answer's"
    );
    assert_eq!(
        score_of(total),
        score_of(completed),
        "the last line moved the score the answering depth said"
    );
    assert_eq!(nodes_of(total), 9700, "the last line is not the budget");
    assert!(
        nodes_of(ceiling) < 9700,
        "the ceiling already covered the whole search: {}",
        ceiling
    );
    s.quit();
}

#[test]
#[cfg_attr(
    feature = "machine-test",
    ignore = "pins a search made with the shipped factor table, which the test rank replaces"
)]
fn a_root_move_that_reaches_beta_is_reported_as_a_floor_and_then_answered_with() {
    // the Italian opening is worth -18 at depth six and 14 at depth seven,
    // so d1e2 reaches beta at depth seven, is reported as a floor and is
    // searched again with beta raised. Both lines name the same move,
    // because the wider search tries it first. A fixed depth, so nothing is
    // aborted and the only bound a line can carry is the root's own. The
    // Tarrasch rook ending was this fixture until the pair term's refit at a
    // ridge of 3e-7, under which its depth seven no longer fails high
    let mut s = Session::start(&[]);
    s.say(&format!("position fen {}", ITALIAN));
    s.say("go depth 7");
    let answer = s.wait_for(|l| l.starts_with("bestmove"));
    let best = move_of(&answer);

    let deepest: Vec<&String> = s
        .said
        .iter()
        .filter(|l| l.starts_with("info depth 7 "))
        .collect();
    let floor = deepest
        .iter()
        .find(|l| l.contains(" lowerbound "))
        .unwrap_or_else(|| panic!("the depth did not fail high: {:#?}", s.said));
    assert_eq!(line_opens_with(floor), best, "the floor named another move");
    let exact = deepest
        .iter()
        .find(|l| !l.contains("bound "))
        .unwrap_or_else(|| panic!("the depth never completed: {:#?}", s.said));
    assert_eq!(
        line_opens_with(exact),
        best,
        "the wider search answered with another move: {}",
        exact
    );
    s.quit();
}

#[test]
#[cfg_attr(
    feature = "machine-test",
    ignore = "pins a search made with the shipped factor table, which the test rank replaces"
)]
fn a_floor_answers_until_the_wider_search_replaces_it() {
    // what the engine plays when the wider search never finishes. WAC.021 is
    // answered with d2c3 at depth eight. At depth nine d2c3 reaches beta
    // first, at 71,058 nodes. Then d2h6 does and is reported as a floor at
    // 97,896, and again at 100,437 as the window widens, and the search
    // finishes at 125,615. A budget after d2h6's floor and inside that is
    // interrupted before anything beats alpha, so the floor is what is left
    // to answer with; with the floor not held it answers d2c3, the move the
    // search has just shown worse
    let mut s = Session::start(&[]);
    s.say(&format!("position fen {}", WAC_021));
    s.say("go nodes 110000");
    let answer = s.wait_for(|l| l.starts_with("bestmove"));
    let best = move_of(&answer);

    let lines: Vec<&String> = s
        .said
        .iter()
        .filter(|l| l.starts_with("info depth "))
        .collect();
    let floor = lines
        .last()
        .unwrap_or_else(|| panic!("the search reported no depth: {:#?}", s.said));
    assert!(
        floor.contains(" lowerbound "),
        "the last line said is not the floor: {}",
        floor
    );
    assert_eq!(line_opens_with(floor), best, "the floor did not answer");
    // and the floor is a move no completed depth named, so answering with
    // it is a claim about the floor and not about the depth before it
    let completed = lines
        .iter()
        .rfind(|l| !l.contains("bound "))
        .unwrap_or_else(|| panic!("no depth completed: {:#?}", s.said));
    assert_ne!(
        line_opens_with(completed),
        best,
        "the floor names what the last completed depth answered, so this \
         says nothing about which of the two was held"
    );
    s.quit();
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

/// Runs the binary to the end, keeping stdout and stderr apart. No deadline:
/// stdin is closed, so a binary that fell through to the uci loop exits.
fn run_to_end(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs")
}

/// Names the commands by hand on purpose, rather than walking the array the
/// binary dispatches from: a command dropped from that array fails here,
/// where a loop over the shorter array would pass.
#[test]
fn a_setting_that_cannot_be_read_is_refused_on_stderr() {
    // stdout stays empty, so no measuring tool mistakes a refusal for a
    // report
    for (arguments, reason) in [
        (["bench", "abc"].as_slice(), "unrecognised bench depth: abc"),
        (
            &["residuals", "every", "abc"],
            "unrecognised residuals every: abc",
        ),
        (
            &["cutoffs", "every", "abc"],
            "unrecognised cutoffs every: abc",
        ),
        (
            &["reductions", "every", "abc"],
            "unrecognised reductions every: abc",
        ),
        (
            &["effort", "every", "abc"],
            "unrecognised effort every: abc",
        ),
        (
            &["forced", "every", "abc"],
            "unrecognised forced every: abc",
        ),
        // terms has no rate; its suite is the setting that can fail to read
        (
            &["terms", "epd", "no/such/file.epd"],
            "unrecognised terms epd: no/such/file.epd",
        ),
    ] {
        let out = run_to_end(arguments);
        assert_eq!(out.status.code(), Some(2), "{:?}", arguments);
        assert!(
            out.stdout.is_empty(),
            "a refused run printed a report: {:?}",
            arguments
        );
        let said = String::from_utf8(out.stderr).unwrap();
        assert!(said.contains(reason), "stderr said: {}", said);
    }
}
