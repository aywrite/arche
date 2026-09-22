// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Sessions against the real binary, the way an interface runs it: argument
//! handling, the stdin loop, the reader thread and exit codes, which the in
//! process tests cannot see.
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
/// suite.
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
    /// on the way is kept in `said`.
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
    // black is mated, so the search is over at once and only the hold can
    // be what the silence is
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
    s.say("quit");
    assert!(s.finished().success());
}

/// The node count an info line reports.
fn nodes_of(info: &str) -> u64 {
    info.split_whitespace()
        .skip_while(|word| *word != "nodes")
        .nth(1)
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("no node count in {}", info))
}

/// The nodes the deepest info line of one search reports, read from the
/// lines that search said.
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
    // the same search three times: cold, warm on what the first left
    // behind, then after the button, which costs what the cold one did
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

/// A middlegame with enough going on at the root for a cut-short iteration
/// to change its mind.
const SHARP_MIDDLEGAME: &str = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";

/// The standard perft position, which changes its root move at depth eight
/// and reports the new move as a floor first.
const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";

#[test]
fn the_move_a_swap_answers_with_opens_the_last_line_said() {
    // a node budget rather than a clock, so the iteration is cut short on
    // the same node on every machine. The budget has to land after an
    // iteration finds its better move and before that iteration ends:
    // depth eleven answers d3b5 and finishes at 638,703 nodes, depth
    // twelve reports d3b1 from between 1,744,000 and 1,745,000 and
    // finishes at 2,041,148. The budget moves whenever the tree does, in
    // the commit that moved it
    let mut s = Session::start(&[]);
    s.say(&format!("position fen {}", SHARP_MIDDLEGAME));
    s.say("go nodes 1900000");
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

/// The centipawn score an info line reported.
fn score_of(info: &str) -> i32 {
    info.split_whitespace()
        .skip_while(|word| *word != "cp")
        .nth(1)
        .and_then(|score| score.parse().ok())
        .unwrap_or_else(|| panic!("no centipawn score in {}", info))
}

/// The first move of the line an info line reported.
fn line_opens_with(info: &str) -> &str {
    info.split(" pv ")
        .nth(1)
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("no line in {}", info))
}

#[test]
fn an_iteration_no_root_move_reached_answers_with_the_depth_before_it() {
    // the opening is worth 54 to white at depth five and 0 at depth six, so
    // depth six opens above what the position turns out to be and nothing
    // inside the window answers it: the depth is reported as the ceiling it
    // is and searched again wider. The budget lands inside that second
    // search, which reaches nothing above its own alpha either, so what
    // answers is still depth five's and not the ceiling just reported. The
    // first search reports at 5,769 nodes and the second finishes at
    // 11,258
    let mut s = Session::start(&[]);
    s.say("position startpos");
    s.say("go nodes 8000");
    let answer = s.wait_for(|l| l.starts_with("bestmove"));
    let best = answer
        .strip_prefix("bestmove ")
        .unwrap_or_else(|| panic!("not a bestmove: {}", answer));

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
    // the ceiling's line opens with the move that came closest, which is
    // not the move answered with: a move nothing was shown to beat is not
    // an answer
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
    // and the ceiling really was one: the position is worth less than the
    // depth answering said, which is what a search finds when it fails low
    assert!(
        score_of(ceiling) < score_of(completed),
        "the ceiling did not fall short of the answer: {} against {}",
        ceiling,
        completed
    );
    // the search then says the answering depth again with every node it
    // spent on it. That line is the one a match harness copies into the
    // game record, and the line it used to copy is the ceiling above.
    // Neither the completed depth's count nor the ceiling's covers the
    // iteration the budget stopped
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
    assert_eq!(nodes_of(total), 8000, "the last line is not the budget");
    assert!(
        nodes_of(ceiling) < 8000,
        "the ceiling already covered the whole search: {}",
        ceiling
    );
    s.say("quit");
    assert!(s.finished().success());
}

#[test]
fn a_root_move_that_reaches_beta_is_reported_as_a_floor_and_then_answered_with() {
    // the opening is worth 0 to white at depth six and 49 at depth seven, so
    // depth seven's window is left behind on the other side: a move reaches
    // beta, the depth reports the floor that move is, and the search runs
    // again with beta raised. Both of depth seven's lines name the same
    // move, which is the point of storing it: the wider search tries it
    // first. A fixed depth rather than a budget, so nothing here is aborted
    // and the only bound a line can carry is the root's own
    let mut s = Session::start(&[]);
    s.say("position startpos");
    s.say("go depth 7");
    let answer = s.wait_for(|l| l.starts_with("bestmove"));
    let best = answer
        .strip_prefix("bestmove ")
        .unwrap_or_else(|| panic!("not a bestmove: {}", answer));

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
    s.say("quit");
    assert!(s.finished().success());
}

#[test]
fn a_floor_answers_until_the_wider_search_replaces_it() {
    // the other half of the floor: what the engine plays when the wider
    // search never finishes. Kiwipete is worth -50 to white at depth
    // seven, answered with e2a6; depth eight opens below that, d5e6
    // reaches beta and the floor is reported at 138,001 nodes and again at
    // 153,663 once the window has been widened, and the search finishes at
    // 237,702. A budget inside it is interrupted before anything beats its
    // alpha, so the root hands back no move at all and the floor is what is
    // left to answer with. Any budget from 138,002 to 237,701 does it; with
    // the floor not held the same budget answers e2a6, which is the move
    // the search has just shown worse.
    //
    // The endgame 8/k1b5/P4p2/1Pp2p1p/K1P2P1P/8/3B4/8 was this fixture
    // until the late move count landed. It still reports a floor, at depth
    // fifteen, but the floor now names the move depth fourteen answered
    // with, so the position can no longer say which of the two was held
    let mut s = Session::start(&[]);
    s.say(&format!("position fen {}", KIWIPETE));
    s.say("go nodes 180000");
    let answer = s.wait_for(|l| l.starts_with("bestmove"));
    let best = answer
        .strip_prefix("bestmove ")
        .unwrap_or_else(|| panic!("not a bestmove: {}", answer));

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

/// Runs the binary with the arguments given and waits for it, keeping stdout
/// and stderr apart. No deadline: stdin is closed, so a binary that fell
/// through to the uci loop reads nothing and exits.
fn run_to_end(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs")
}

#[test]
fn a_setting_that_cannot_be_read_is_refused_on_stderr() {
    // the reason goes to stderr with exit code 2 and stdout stays empty, so
    // no measuring tool mistakes the refusal for a report
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
