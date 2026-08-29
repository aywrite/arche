// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::params::{Param, Params};
pub use crate::session::report_panics_to;
use crate::session::{self, SessionControl, SharedWriter, first_word};
use crate::time_control::TimeControl;
use arche_core::Color;
use arche_core::Engine;
use arche_core::Limits;
use arche_core::SearchConfig;
use arche_core::SearchOutcome;
use arche_core::SearchParameters;
use arche_core::bench;
use arche_core::residual;
use arche_core::{PvLine, SearchResult};
use std::io::{BufRead, Stdout, Write};

/// The time part of a `go` command, from the point of view of the side to
/// move.
///
/// A clock or a move time that was sent but cannot be read stands in as spent
/// rather than being discarded, because discarding it would read as the
/// keyword having been absent, and a `go` with no time at all searches without
/// a limit. That trades one bad outcome for a smaller one: an unreadable move
/// time beside a good clock spends the clock rather than reading it, and moves
/// almost at once. Playing a weak move is recoverable and thinking for ever is
/// not. A count of moves is left alone instead: it only divides the clock, and
/// the time control already treats a missing one as a number to assume.
fn time_control_from(params: &Params, color: Color) -> TimeControl {
    let (clock, increment) = match color {
        Color::White => ("wtime", "winc"),
        Color::Black => ("btime", "binc"),
    };
    TimeControl {
        time: params.count(clock).read_or(0),
        increment: params.count(increment).read_or(0),
        moves_to_go: params.count("movestogo").read(),
        move_time: params.count("movetime").read_or(0),
        infinite: params.flag("infinite"),
    }
}

/// The `Hash` option's range, in megabytes, as the handshake advertises it.
///
/// The top is sixteen gibibytes, more than any machine here has, and is also
/// what a `bench hash` may ask for, since a bench is a search like any other.
/// It is held to what this machine can address besides, since the megabytes
/// become bytes in a usize: on the sixty four bit targets the engine is built
/// for that costs nothing, and on a narrower one it advertises a range that
/// can actually be asked for rather than one that would overflow.
const HASH_MIN_MB: u64 = 1;
const HASH_MAX_MB: u64 = {
    let addressable = (usize::MAX / (1024 * 1024)) as u64;
    if addressable < 16 * 1024 {
        addressable
    } else {
        16 * 1024
    }
};

/// The size the handshake advertises as the default, taken from the engine's
/// own so that an interface which never sends a `setoption` is told the table
/// it is actually going to get.
const HASH_DEFAULT_MB: u64 = (arche_core::DEFAULT_TABLE_BYTES / (1024 * 1024)) as u64;

// a default outside the range advertised beside it would be a handshake no
// interface could honour, so moving the engine's default out of range fails
// the build rather than the game
const _: () = assert!(HASH_DEFAULT_MB >= HASH_MIN_MB && HASH_DEFAULT_MB <= HASH_MAX_MB);

/// A `Hash` value held to the range the handshake advertises. An interface is
/// not supposed to send one outside it, and one that does is asking for more
/// table than we offer rather than making a mistake worth refusing, so the
/// nearest size we do offer is what it gets.
fn clamp_hash(megabytes: u64) -> u64 {
    megabytes.clamp(HASH_MIN_MB, HASH_MAX_MB)
}

pub struct UCI<T: Engine, W: Write> {
    author: String,
    name: String,
    version: String,

    engine: T,
    out: W,
}

impl<T: Engine> UCI<T, SharedWriter<Stdout>> {
    pub fn new_with_engine(engine: T) -> Self {
        Self::with_output(engine, SharedWriter::new(std::io::stdout()))
    }

    /// Read stdin on a thread of its own and run the session on this one.
    pub fn read_loop(&mut self) {
        self.wire(|| std::io::stdin().lock().lines());
    }
}

impl<T: Engine, W: Write + Send + 'static> UCI<T, SharedWriter<W>> {
    /// Run this session over the input given: the session module owns the
    /// threads and the loop, and every line comes back to `dispatch` here.
    /// The engine never crosses the boundary: it is searched on this
    /// thread, the one it was built on.
    fn wire<I, F>(&mut self, input: F)
    where
        I: Iterator<Item = std::io::Result<String>>,
        F: FnOnce() -> I + Send + 'static,
    {
        let out = self.out.clone();
        session::wire(out, input, |line, control| self.dispatch(line, control));
    }
}

impl<T: Engine, W: Write> UCI<T, W> {
    /// Separate from new_with_engine so that what is said can be captured.
    fn with_output(engine: T, out: W) -> Self {
        Self {
            author: env!("CARGO_PKG_AUTHORS").to_string(),
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            engine,
            out,
        }
    }

    /// One line, one arm: a command is its first word, and the word says
    /// everything about where the line goes. A `go` is bracketed so the
    /// reader thread knows a search is running and can stop it.
    ///
    /// Returns false once the engine has been asked to quit.
    fn dispatch(&mut self, line: &str, control: &SessionControl) -> bool {
        match first_word(line) {
            "quit" => return false,
            "go" => {
                control.began_searching();
                self.parse_go(line, control);
                control.answered();
            }
            "stop" => {
                // the reader thread stops the search, so by the time one
                // reaches here whatever it was meant for has answered, or
                // there was nothing to answer, and the flag it set is
                // spent. Taken in silence, because the protocol allows a
                // stop at any moment and an engine that answered one with
                // a complaint would be wrong
                control.clear();
            }
            "isready" => self.say(format_args!("readyok")),
            "ucinewgame" => {
                self.engine.new_game();
                let result = self.parse_position("position startpos");
                self.report(result);
            }
            "uci" => {
                // written to the field directly: say borrows all of self,
                // and these lines also read from it
                let _ = writeln!(self.out, "id name {} {}", self.name, self.version);
                let _ = writeln!(self.out, "id author {}", self.author);
                self.say(format_args!(
                    "option name Hash type spin default {} min {} max {}",
                    HASH_DEFAULT_MB, HASH_MIN_MB, HASH_MAX_MB
                ));
                // advertised with a range of one so that an interface
                // configuring a match reads the engine as single threaded
                // rather than being left to find out by playing one
                self.say(format_args!(
                    "option name Threads type spin default 1 min 1 max 1"
                ));
                self.say(format_args!("uciok"));
            }
            "setoption" => {
                let result = self.set_option(line);
                self.report(result);
            }
            "position" => {
                let result = self.parse_position(line);
                self.report(result);
            }
            "display" => {
                // one info string a row, so the dump goes through the
                // writer's lock like every other line and an interface
                // reads it as the commentary it is rather than as protocol
                // it has to parse. The blank separator rows say nothing
                // once every row carries the prefix, so they stay behind
                let board = self.engine.board_display();
                for row in board.lines().filter(|row| !row.is_empty()) {
                    self.say(format_args!("info string {}", row));
                }
            }
            "bench" => self.bench(line),
            "perft" => {
                let depth = perft_depth(&Params::of(line));
                let nodes = self.engine.perft(depth);
                self.say(format_args!(
                    "info string perft depth {} nodes {}",
                    depth, nodes
                ));
            }
            _ => self.say(format_args!("info string unrecognised command: {}", line)),
        }
        true
    }

    /// Writes one line to the interface. A failed write means the interface
    /// itself is gone, which leaves no one to tell, so the error is dropped.
    fn say(&mut self, line: std::fmt::Arguments) {
        let _ = writeln!(self.out, "{}", line);
    }

    /// Tells the interface about anything we could not act on. A bad line is
    /// the interface's problem to fix, so say so and carry on reading.
    fn report(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.say(format_args!("info string {}", error));
        }
    }

    /// One line through the real dispatcher, unattended: what a test drives
    /// when the question is what a line does rather than what two threads
    /// do with each other.
    #[cfg(test)]
    fn handle(&mut self, line: &str) -> bool {
        self.dispatch(line, &SessionControl::unattended())
    }

    /// Dispatches input until it is exhausted or `quit` arrives, on one
    /// thread and with nothing able to interrupt a search.
    #[cfg(test)]
    fn run<R: BufRead>(&mut self, input: R) {
        let control = SessionControl::unattended();
        for line in input.lines() {
            match line {
                Ok(line) => {
                    if !self.dispatch(&line, &control) {
                        return;
                    }
                }
                // there is nothing left to read and no one to tell, so stop
                // rather than sitting in the loop asking again
                Err(error) => {
                    self.say(format_args!("info string could not read input: {}", error));
                    return;
                }
            }
        }
    }

    /// The same bench as the command line argument: the depth is for trying
    /// the command cheaply, the number that means anything is the one at
    /// the default, and the table and policy are for measuring rather than
    /// pinning.
    fn bench(&mut self, line: &str) {
        match bench_settings(&Params::of(line)) {
            Ok(settings) => {
                let report = bench::run_suite(
                    &bench::positions(),
                    settings.depth,
                    settings.table_bytes,
                    settings.config,
                );
                self.say(format_args!("{}", report));
            }
            Err(what) => self.say(format_args!("info string unrecognised bench {}", what)),
        }
    }

    /// `setoption name <option> value <value>`. The two options the handshake
    /// advertises are the only ones answered to; anything else is said back
    /// rather than acted on, so that an interface sending an option meant for
    /// another engine is told it did.
    ///
    /// Both names here are a single word, which is what lets the name be read
    /// as the word after `name`. An option named with two would need the words
    /// between `name` and `value` gathered instead.
    fn set_option(&mut self, line: &str) -> Result<(), String> {
        let params = Params::of(line);
        let Some(name) = params.value("name") else {
            return Err(format!("setoption without an option name: {}", line));
        };
        match name {
            "Hash" => self.set_hash(&params),
            "Threads" => self.set_threads(&params),
            other => Err(format!("unrecognised option: {}", other)),
        }
    }

    /// Give the engine a table of the megabytes asked for. The table is
    /// emptied by being rebuilt, which is what the protocol expects of a size
    /// change and why an interface sends one between games rather than during
    /// one.
    fn set_hash(&mut self, params: &Params) -> Result<(), String> {
        // the word as well as the count, so that what is said back is what was
        // sent: the count is read the way a clock is, and a negative size
        // reaching us as a zero would otherwise be reported as a zero
        let (word, megabytes) = match (params.value("value"), params.count("value")) {
            (Some(word), Param::Read(megabytes)) => (word, megabytes),
            (_, Param::Unreadable(word)) => {
                return Err(format!("unrecognised Hash value: {}", word));
            }
            _ => return Err("Hash was sent without a value".to_string()),
        };
        let held = clamp_hash(megabytes);
        if held != megabytes {
            self.say(format_args!(
                "info string Hash {} is outside {} to {}, using {}",
                word, HASH_MIN_MB, HASH_MAX_MB, held
            ));
        }
        if !self.engine.set_table_bytes(held as usize * 1024 * 1024) {
            return Err(format!(
                "no memory for a {}MB table, keeping the one we have",
                held
            ));
        }
        Ok(())
    }

    /// There is no parallel search, so the only count that can be honoured is
    /// one. Any other is said back and then ignored: refusing to play because
    /// a match was configured for four threads would be worse than playing on
    /// one, and the interface has already been told the maximum is one.
    fn set_threads(&mut self, params: &Params) -> Result<(), String> {
        // the word rather than the count, for the reason set_hash reads one
        match (params.value("value"), params.count("value")) {
            (_, Param::Read(1)) => Ok(()),
            (Some(word), Param::Read(_)) => Err(format!(
                "Threads {} was asked for; the engine searches on one",
                word
            )),
            (_, Param::Unreadable(word)) => Err(format!("unrecognised Threads value: {}", word)),
            _ => Err("Threads was sent without a value".to_string()),
        }
    }

    /// A move that cannot be played leaves the position at the last one that
    /// could be, since the interface is expected to send the whole line again
    /// rather than to carry on from a position we rejected.
    fn parse_position(&mut self, line: &str) -> Result<(), String> {
        // trimmed before the strip: the dispatcher read the first word past
        // any leading space, and this must read the same line it did
        let position_string = line
            .trim_start()
            .strip_prefix("position")
            .unwrap_or(line)
            .trim();
        let (start, move_list) = match position_string.split_once("moves") {
            Some((s, m)) => (s.trim(), Some(m)),
            None => (position_string, None),
        };
        if start.starts_with("startpos") {
            self.engine.parse_fen(arche_core::STARTING_FEN)?;
        } else if let Some(fen) = start.strip_prefix("fen") {
            self.engine.parse_fen(fen.trim())?;
        } else {
            return Err(format!("unrecognised position: {}", start));
        }

        if let Some(moves) = move_list {
            for m in moves.split_whitespace() {
                if !self.engine.make_move_str(m.trim()) {
                    return Err(format!("could not play {}", m));
                }
            }
        }
        Ok(())
    }

    /// A `go`, under the session's control: the reader thread's stop flag
    /// rides into the search, and a go that holds its answer waits here
    /// for the stop that releases it.
    fn parse_go(&mut self, line: &str, control: &SessionControl) {
        let params = Params::of(line);
        // the clock starts here, when the command arrived, and the search
        // reports its elapsed time against the same start
        let limits = Limits::starting_now(
            time_control_from(&params, self.engine.active_color()).budget(),
            // an unreadable node limit is ignored rather than obeyed as zero,
            // which would stop the search before it had a move to report
            params.count("nodes").read(),
        );
        let depth = go_depth(&params);
        let holds = holds_its_answer(&params, depth, &limits);
        let sp = SearchParameters::stoppable(depth, limits, control.handle());

        // the closure writes while the engine is borrowed for the search, so
        // it goes to the writer directly rather than through say
        let out = &mut self.out;
        let outcome = self
            .engine
            .iterative_deepening_search(sp, |depth, result, pv| {
                let _ = writeln!(out, "{}", format_info(depth, result, &pv));
            });
        // an infinite search does not answer until it is told to, even when
        // it ran out of depths to search first
        if holds {
            control.wait_for_stop();
        }
        match outcome {
            SearchOutcome::Complete(result) | SearchOutcome::Aborted(Some(result)) => {
                self.say(format_args!("bestmove {}", result.best_move));
            }
            SearchOutcome::GameOver => {
                self.say(format_args!("info string no legal moves identified"));
                // 0000 is the null move, used to report that there is no move
                // to make
                self.say(format_args!("bestmove 0000"));
            }
            SearchOutcome::Aborted(None) => self.say(format_args!("bestmove 0000")),
        }
    }
}

/// Whether this `go` must sit on its answer until a `stop` arrives.
///
/// `go infinite` says so outright. A `go` with nothing to bound it at all
/// says the same thing by saying nothing: it used to mean a search to the
/// old depth cap on no clock, which nothing sends deliberately, and one
/// behaviour is worth more here than a distinction between the two.
fn holds_its_answer(params: &Params, depth: Option<u8>, limits: &Limits) -> bool {
    params.flag("infinite")
        || (depth.is_none() && limits.clock().is_none() && limits.node_budget() == u64::MAX)
}

/// The depth asked of a go command, if one was. A depth past what the engine
/// will search is a request to go deep, not a reason to refuse the command,
/// so it is held to the ply rail rather than rejected.
///
/// The rail is also what keeps the root's check extension inside a byte: a
/// depth of two hundred and fifty five from a position in check used to be
/// deepened to two hundred and fifty six and overflow.
fn go_depth(params: &Params) -> Option<u8> {
    params
        .count("depth")
        .read()
        .map(|depth| depth.try_into().unwrap_or(u8::MAX).min(arche_core::MAX_PLY))
}

/// What a bench command or argument asked for: `bench [depth] [hash <MB>]
/// [taint refuse|trust|skip|rule50]`, each setting standing in for the
/// bench's own when absent. The depth is for trying the command cheaply; the
/// table and the policy are what the graph history measurements vary, and a
/// report states all three in its header so it can be rerun from it.
pub struct BenchSettings {
    pub depth: u8,
    pub table_bytes: usize,
    pub config: SearchConfig,
}

/// The words a bench takes after its depth. One of them standing where
/// the depth would be means the depth was left out, not mistyped.
const BENCH_KEYWORDS: [&str; 2] = ["hash", "taint"];

/// Reads the bench settings, or says which word could not be read: the
/// setting's name and the word, for the caller to report. Running the
/// default in its place would take seconds and explain nothing.
pub fn bench_settings(params: &Params) -> Result<BenchSettings, String> {
    let depth = match params.parse::<u8>("bench") {
        Param::Absent => bench::DEPTH,
        Param::Read(depth) => depth,
        Param::Unreadable(word) if BENCH_KEYWORDS.contains(&word) => bench::DEPTH,
        Param::Unreadable(word) => return Err(format!("depth: {word}")),
    };
    let table_bytes = match params.parse::<u64>("hash") {
        Param::Absent => bench::TABLE_BYTES,
        // the range the uci Hash option advertises, so that a size the engine
        // would play with is a size the bench can be run at
        Param::Read(mb) if (HASH_MIN_MB..=HASH_MAX_MB).contains(&mb) => mb as usize * 1024 * 1024,
        Param::Read(mb) => return Err(format!("hash: {mb}")),
        Param::Unreadable(word) => return Err(format!("hash: {word}")),
    };
    let config = match params.value("taint") {
        None => SearchConfig::default(),
        Some(word) => SearchConfig::with_taint(word).ok_or_else(|| format!("taint: {word}"))?,
    };
    Ok(BenchSettings {
        depth,
        table_bytes,
        config,
    })
}

/// What a residuals argument asked for: `residuals [depth] [every <n>]
/// [taint refuse|trust|skip|rule50]`. The depth and the policy are the
/// bench's own when absent, so a residual distribution is measured over the
/// tree the bench describes; the rate is how much of that tree is sampled.
pub struct ResidualSettings {
    pub depth: u8,
    pub every: u32,
    pub config: SearchConfig,
}

/// The words a residuals run takes after its depth, read the way the bench
/// reads its own.
const RESIDUAL_KEYWORDS: [&str; 2] = ["every", "taint"];

/// Reads the residual settings, or says which word could not be read. The
/// same shape as `bench_settings`, and for the same reason: running the
/// default in place of a word nobody typed would take minutes and explain
/// nothing.
pub fn residual_settings(params: &Params) -> Result<ResidualSettings, String> {
    let depth = match params.parse::<u8>("residuals") {
        Param::Absent => bench::DEPTH,
        Param::Read(depth) => depth,
        Param::Unreadable(word) if RESIDUAL_KEYWORDS.contains(&word) => bench::DEPTH,
        Param::Unreadable(word) => return Err(format!("depth: {word}")),
    };
    let every = match params.parse::<u32>("every") {
        Param::Absent => residual::DEFAULT_EVERY,
        // a rate of zero records every node a shortcut answers, which is a
        // thing to ask for rather than a mistake
        Param::Read(every) => every,
        Param::Unreadable(word) => return Err(format!("every: {word}")),
    };
    let config = match params.value("taint") {
        None => SearchConfig::default(),
        Some(word) => SearchConfig::with_taint(word).ok_or_else(|| format!("taint: {word}"))?,
    };
    Ok(ResidualSettings {
        depth,
        every,
        config,
    })
}

/// The depth asked of a perft command. A bare `perft` counts to depth one,
/// which is what the command did before it took a depth at all. Unlike the
/// bench, a depth too big for a byte is clamped rather than refused: perft is
/// asked for by hand and the answer to too deep is to wait or interrupt.
fn perft_depth(params: &Params) -> u8 {
    params
        .count("perft")
        .read()
        .map(|depth| depth.try_into().unwrap_or(u8::MAX))
        .unwrap_or(1)
}

/// One completed depth as a UCI info line. The elapsed time comes from the
/// result rather than from a clock read here, so the rate reported divides a
/// node count by the time that same search took; a test pins the whole line
/// by building the result it formats.
fn format_info(depth: u8, result: &SearchResult, pv: &PvLine) -> String {
    let millis = result.elapsed.as_millis();
    // measure a search faster than a millisecond as one, so the rate stays
    // finite and the arithmetic stays whole
    let nps = (result.nodes as u128 * 1000 / millis.max(1)) as u64;
    match result.checkmate_in() {
        Some(mate_in) => format!(
            "info depth {} seldepth {} nodes {} time {} nps {} score mate {} pv {}",
            depth, result.selective_depth, result.nodes, millis, nps, mate_in, pv
        ),
        None => format!(
            "info depth {} seldepth {} nodes {} time {} nps {} score cp {} pv {}",
            depth, result.selective_depth, result.nodes, millis, nps, result.score, pv
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arche_core::{AlphaBeta, Board, Clock};
    use proptest::prelude::*;
    use std::io::Cursor;
    use std::sync::mpsc::{Sender, channel};
    use std::thread;
    use std::time::{Duration, Instant};

    /// A table small enough that a test can afford one per case, speaking
    /// into a buffer so that what the interface would see can be asserted.
    fn uci() -> UCI<AlphaBeta, Vec<u8>> {
        UCI::with_output(
            AlphaBeta::with_table_bytes(Board::new(), 8 * 1024),
            Vec::new(),
        )
    }

    /// An engine that searches nothing and keeps what it was asked for.
    ///
    /// The clock a `go` names is worked out here and enforced in the engine,
    /// and between the two it crosses one call. This is what stands on the
    /// other side of that call, so a test can say what reached it rather than
    /// timing a real search and inferring.
    struct Recorder {
        asked: Option<SearchParameters>,
        color: Color,
    }

    impl Recorder {
        fn to_move(color: Color) -> Self {
            Self { asked: None, color }
        }
    }

    impl Engine for Recorder {
        fn iterative_deepening_search(
            &mut self,
            search_options: SearchParameters,
            _on_depth: impl FnMut(u8, &SearchResult, PvLine),
        ) -> SearchOutcome {
            self.asked = Some(search_options);
            // nothing was searched, so there is no move to report
            SearchOutcome::GameOver
        }

        fn active_color(&self) -> Color {
            self.color
        }

        fn parse_fen(&mut self, _fen: &str) -> Result<(), String> {
            Ok(())
        }
        fn new_game(&mut self) {}
        fn make_move_str(&mut self, _play: &str) -> bool {
            true
        }
        fn set_table_bytes(&mut self, _bytes: usize) -> bool {
            true
        }
        fn board_display(&self) -> String {
            String::new()
        }
        fn perft(&mut self, _depth: u8) -> u64 {
            0
        }
    }

    /// What a `go` line asks of the engine behind it.
    fn asked_of_engine(line: &str) -> SearchParameters {
        asked_of_engine_as(line, Color::White)
    }

    /// The same, for a side to move.
    fn asked_of_engine_as(line: &str, color: Color) -> SearchParameters {
        let mut uci = UCI::with_output(Recorder::to_move(color), Vec::new());
        uci.run(Cursor::new(format!("{}\n", line)));
        uci.engine
            .asked
            .expect("the go command never reached the engine")
    }

    /// Everything said so far, as one string.
    fn said(uci: &UCI<AlphaBeta, Vec<u8>>) -> String {
        String::from_utf8(uci.out.clone()).unwrap()
    }

    #[test]
    fn a_position_can_be_set_from_the_start_or_from_a_fen() {
        let mut uci = uci();
        assert_eq!(uci.parse_position("position startpos"), Ok(()));
        assert_eq!(uci.engine.active_color(), Color::White);

        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R b KQ - 9 12";
        assert_eq!(uci.parse_position(&format!("position fen {}", fen)), Ok(()));
        assert_eq!(uci.engine.active_color(), Color::Black);
    }

    #[test]
    fn moves_after_a_position_are_played() {
        let mut uci = uci();
        assert_eq!(
            uci.parse_position("position startpos moves e2e4 e7e5 g1f3"),
            Ok(())
        );
        assert_eq!(uci.engine.active_color(), Color::Black);
    }

    #[test]
    fn malformed_positions_are_reported_rather_than_fatal() {
        // each of these used to panic, taking the engine down mid game
        for line in [
            "position",
            "position wibble",
            "position fen",
            "position fen not a fen at all",
            "position fen 8/8/8/8/8/8/8/8 w - - 0 1 moves e2e4",
            // both of these parsed, and then took the engine down on the search
            "position fen 8/8/8/8/8/8/8/8 w - - 0 1",
            "position fen 4k3/8/8/8/8/8/8/4R1K1 w - - 0 1",
            "position startpos moves e2e4 zzzz",
            "position startpos moves e2e4 e2e4",
        ] {
            let mut uci = uci();
            assert!(
                uci.parse_position(line).is_err(),
                "expected an error: {}",
                line
            );
        }
    }

    #[test]
    fn unrecognised_and_empty_commands_are_survivable() {
        let mut uci = uci();
        for line in ["", "   ", "wibble", "positional", "isready"] {
            assert!(uci.handle(line), "{} should not have quit", line);
        }
    }

    #[test]
    fn a_command_is_its_first_word_wherever_the_line_starts() {
        let mut uci = uci();
        uci.handle("  position startpos moves e2e4");
        assert_eq!(uci.engine.active_color(), Color::Black);
    }

    #[test]
    fn display_speaks_through_the_writer_as_info_strings() {
        let mut uci = uci();
        uci.handle("display");
        let spoken = said(&uci);
        assert!(
            spoken.contains("a b c d e f g h"),
            "not the board: {}",
            spoken
        );
        // an interface reads lines, and every one of these must be one it
        // can pass over rather than protocol it has to parse
        for line in spoken.lines() {
            assert!(
                line.starts_with("info string "),
                "a bare line an interface cannot read: {}",
                line
            );
        }
    }

    #[test]
    fn quit_stops_the_loop_and_leaves_the_rest_unread() {
        let mut uci = uci();
        uci.run(Cursor::new(
            "position startpos moves e2e4\nquit\nposition startpos\n",
        ));
        // the reset after quit must not have been acted on
        assert_eq!(uci.engine.active_color(), Color::Black);
    }

    #[test]
    fn the_loop_ends_when_the_input_does() {
        // without a quit the old loop asked a closed stdin for another line for
        // ever, so reaching the end of this call is the assertion
        let mut uci = uci();
        uci.run(Cursor::new("uci\nisready\nposition startpos\ngo depth 1\n"));
    }

    #[test]
    fn the_handshake_identifies_the_engine_and_ends_with_uciok() {
        let mut uci = uci();
        uci.handle("uci");
        let said = said(&uci);
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("id name arche "));
        assert!(lines[1].starts_with("id author "));
        // the default is the engine's own, so an interface that sends no
        // setoption is told the size it is going to get
        assert_eq!(
            lines[2],
            "option name Hash type spin default 256 min 1 max 16384"
        );
        assert_eq!(
            lines[3],
            "option name Threads type spin default 1 min 1 max 1"
        );
        assert_eq!(lines[4], "uciok");
    }

    #[test]
    fn isready_is_answered_with_readyok_alone() {
        let mut uci = uci();
        uci.handle("isready");
        assert_eq!(said(&uci), "readyok\n");
    }

    #[test]
    fn a_search_reports_each_depth_and_then_its_move() {
        let mut uci = uci();
        uci.run(Cursor::new("position startpos\ngo depth 3\n"));
        let said = said(&uci);
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines.len(), 4, "a depth three search speaks four lines");
        for line in &lines[..3] {
            assert!(
                line.starts_with("info depth "),
                "not an info line: {}",
                line
            );
        }
        assert!(lines[3].starts_with("bestmove "));
    }

    #[test]
    fn a_search_on_a_negative_clock_still_answers_with_a_move() {
        let mut uci = uci();
        uci.run(Cursor::new("position startpos\ngo wtime -5 btime -5\n"));
        let said = said(&uci);
        let last = said.lines().last().unwrap_or("");
        assert!(last.starts_with("bestmove "), "{}", said);
        assert_ne!(last, "bestmove 0000", "{}", said);
    }

    #[test]
    fn a_position_with_no_legal_moves_reports_the_null_move() {
        for fen in [
            "7k/6Q1/6K1/8/8/8/8/8 b - - 0 1", // checkmate
            "7k/5Q2/6K1/8/8/8/8/8 b - - 0 1", // stalemate
        ] {
            let mut uci = uci();
            uci.run(Cursor::new(format!("position fen {}\ngo depth 1\n", fen)));
            let said = said(&uci);
            assert!(
                said.contains("info string no legal moves identified"),
                "{}",
                fen
            );
            assert!(said.ends_with("bestmove 0000\n"), "{}: {}", fen, said);
        }
    }

    #[test]
    fn what_could_not_be_acted_on_is_reported_as_an_info_string() {
        let mut uci = uci();
        uci.run(Cursor::new("position wibble\nwobble\n"));
        let said = said(&uci);
        assert!(said.contains("info string unrecognised position: wibble"));
        assert!(said.contains("info string unrecognised command: wobble"));
    }

    #[test]
    fn a_perft_command_reports_its_count() {
        let mut uci = uci();
        uci.run(Cursor::new("position startpos\nperft 2\n"));
        assert_eq!(said(&uci), "info string perft depth 2 nodes 400\n");
    }

    #[test]
    fn a_new_game_resets_the_position() {
        let mut uci = uci();
        uci.parse_position("position startpos moves e2e4").unwrap();
        assert_eq!(uci.engine.active_color(), Color::Black);
        assert!(uci.handle("ucinewgame"));
        assert_eq!(uci.engine.active_color(), Color::White);
    }

    /// The table a request for `megabytes` builds, which is the whole entries
    /// that fit in them rather than the megabytes themselves.
    fn megabytes(megabytes: usize) -> usize {
        AlphaBeta::with_table_bytes(Board::new(), megabytes * 1024 * 1024).table_bytes()
    }

    #[test]
    fn the_hash_option_takes_the_table_size_it_is_given() {
        // no uci first: an option may be set before the handshake, and every
        // case below relies on that
        let mut uci = uci();
        assert!(uci.handle("setoption name Hash value 1"));
        assert_eq!(uci.engine.table_bytes(), megabytes(1));
        assert_eq!(
            said(&uci),
            "",
            "a size we can honour is acted on in silence"
        );
    }

    #[test]
    fn a_hash_size_below_the_smallest_offered_is_clamped_up_to_it() {
        let mut uci = uci();
        assert!(uci.handle("setoption name Hash value 0"));
        assert_eq!(uci.engine.table_bytes(), megabytes(1));
        assert!(said(&uci).contains("info string Hash 0 is outside 1 to 16384, using 1"));
    }

    #[test]
    fn a_hash_size_outside_the_range_offered_is_clamped_to_its_nearest_end() {
        // asked of the clamp rather than of the command, since honouring a
        // size at the top of the range would mean allocating sixteen
        // gigabytes to assert it
        assert_eq!(clamp_hash(99999), 16384);
        assert_eq!(clamp_hash(0), 1);
        assert_eq!(clamp_hash(u64::MAX), 16384);
        assert_eq!(clamp_hash(256), 256);
    }

    #[test]
    fn a_hash_value_that_cannot_be_read_leaves_the_table_alone() {
        for line in [
            "setoption name Hash value",
            "setoption name Hash value many",
        ] {
            let mut uci = uci();
            // resized first, so that what is kept is the size in force rather
            // than the one the engine happened to be built with
            uci.handle("setoption name Hash value 1");
            assert!(uci.handle(line));
            assert_eq!(uci.engine.table_bytes(), megabytes(1), "{}", line);
            assert!(
                said(&uci).starts_with("info string "),
                "{}: {}",
                line,
                said(&uci)
            );
        }
    }

    #[test]
    fn a_size_is_said_back_as_the_word_that_was_sent() {
        // a count is read the way a clock is, so a negative one reaches us as
        // a zero. What is said back has to be what the interface typed, or the
        // line describes a size nobody asked for
        let mut hash = uci();
        assert!(hash.handle("setoption name Hash value -5"));
        assert!(
            said(&hash).contains("info string Hash -5 is outside 1 to 16384, using 1"),
            "{}",
            said(&hash)
        );

        let mut threads = uci();
        assert!(threads.handle("setoption name Threads value -1"));
        assert!(
            said(&threads).contains("info string Threads -1 was asked for"),
            "{}",
            said(&threads)
        );
    }

    #[test]
    fn the_table_can_be_resized_between_two_searches_and_after_a_new_game() {
        // the three moments the protocol allows one, the third being before
        // any uci at all, which every case here already relies on
        let mut uci = uci();
        uci.run(Cursor::new(
            "position startpos
go depth 3
setoption name Hash value 1
             position startpos
go depth 3
ucinewgame
setoption name Hash value 2
             position startpos
go depth 3
",
        ));
        assert_eq!(uci.engine.table_bytes(), megabytes(2));
        let said = said(&uci);
        assert_eq!(
            said.lines()
                .filter(|line| line.starts_with("bestmove "))
                .count(),
            3,
            "{}",
            said
        );
    }

    #[test]
    fn the_threads_option_takes_one_in_silence() {
        let mut uci = uci();
        assert!(uci.handle("setoption name Threads value 1"));
        assert_eq!(said(&uci), "", "the one count we can honour is silent");
    }

    #[test]
    fn any_other_thread_count_is_said_back_and_then_played_on_anyway() {
        let mut uci = uci();
        uci.run(Cursor::new(
            "setoption name Threads value 4\nposition startpos\ngo depth 2\n",
        ));
        let said = said(&uci);
        assert!(
            said.contains("info string Threads 4 was asked for"),
            "{}",
            said
        );
        assert!(
            said.lines().last().unwrap_or("").starts_with("bestmove "),
            "{}",
            said
        );
    }

    #[test]
    fn an_option_we_do_not_have_is_reported_by_name() {
        let mut uci = uci();
        assert!(uci.handle("setoption name Nonsense value 1"));
        assert_eq!(said(&uci), "info string unrecognised option: Nonsense\n");
    }

    #[test]
    fn a_setoption_without_a_name_is_reported_rather_than_fatal() {
        let mut uci = uci();
        assert!(uci.handle("setoption"));
        assert!(said(&uci).starts_with("info string setoption without an option name"));
    }

    #[test]
    fn a_new_game_keeps_the_table_size_it_was_given() {
        let mut uci = uci();
        uci.handle("setoption name Hash value 1");
        assert!(uci.handle("ucinewgame"));
        assert_eq!(uci.engine.table_bytes(), megabytes(1));
    }

    #[test]
    fn each_colour_reads_its_own_clock() {
        let line = "go wtime 111 btime 222 winc 333 binc 444 movestogo 5";
        assert_eq!(
            time_control_from(&Params::of(line), Color::White),
            TimeControl {
                time: Some(111),
                increment: Some(333),
                moves_to_go: Some(5),
                move_time: None,
                infinite: false,
            }
        );
        assert_eq!(
            time_control_from(&Params::of(line), Color::Black),
            TimeControl {
                time: Some(222),
                increment: Some(444),
                moves_to_go: Some(5),
                move_time: None,
                infinite: false,
            }
        );
    }

    #[test]
    fn a_missing_clock_for_our_colour_is_not_taken_from_the_other() {
        let control = time_control_from(&Params::of("go btime 222 binc 444"), Color::White);
        assert_eq!(control.time, None);
        assert_eq!(control.increment, None);
    }

    #[test]
    fn move_time_and_infinite_are_read() {
        assert_eq!(
            time_control_from(&Params::of("go movetime 500"), Color::White).move_time,
            Some(500)
        );
        assert!(time_control_from(&Params::of("go infinite"), Color::White).infinite);
        assert!(!time_control_from(&Params::of("go wtime 1000"), Color::White).infinite);
    }

    #[test]
    fn a_clock_too_large_to_hold_is_not_read_as_absent() {
        let line = "go wtime 99999999999999999999999";
        assert_eq!(
            time_control_from(&Params::of(line), Color::White).time,
            Some(u64::MAX),
            "an unreadable clock must not turn into an unlimited search"
        );
    }

    #[test]
    fn a_negative_clock_is_read_as_empty() {
        // cutechess and fastchess send a clock below zero once their time
        // margin has been eaten into. Not reading it would leave the budget
        // unset and search without a limit, at the moment there is the least
        // time to spare
        let control = time_control_from(&Params::of("go wtime -5 btime -5"), Color::White);
        assert_eq!(control.time, Some(0));
        let control = time_control_from(
            &Params::of("go wtime -5 btime -5 winc -1 binc -1"),
            Color::Black,
        );
        assert_eq!(control.time, Some(0));
        assert_eq!(control.increment, Some(0));
    }

    #[test]
    fn a_clock_that_cannot_be_read_is_a_spent_one_rather_than_no_clock() {
        // discarding it would read as the keyword having been absent, and a
        // go with no time at all searches without a limit
        let control = time_control_from(&Params::of("go wtime abc winc x"), Color::White);
        assert_eq!(control.time, Some(0));
        assert_eq!(control.increment, Some(0));
        assert!(
            control.budget().is_some(),
            "an unreadable clock must still bound the search"
        );
    }

    #[test]
    fn an_unreadable_depth_is_ignored_rather_than_obeyed_as_zero() {
        // zero would be a depth of nothing, and the search would come back
        // without a move rather than without a limit
        assert_eq!(go_depth(&Params::of("go depth abc")), None);
    }

    #[test]
    fn a_keyword_inside_a_longer_word_is_not_one() {
        // the regexes this replaced had no word boundary, so a clock could be
        // read out of the middle of another token
        let control = time_control_from(&Params::of("go xwtime 300000"), Color::White);
        assert_eq!(control.time, None);
    }

    #[test]
    fn a_node_limit_is_honoured_end_to_end() {
        let mut uci = uci();
        uci.run(Cursor::new("position startpos\ngo nodes 5000\n"));
        let said = said(&uci);
        let lines: Vec<&str> = said.lines().collect();
        assert!(lines.last().unwrap().starts_with("bestmove "), "{}", said);
        let info = lines[lines.len() - 2];
        let nodes: u64 = info
            .split_whitespace()
            .skip_while(|word| *word != "nodes")
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no node count in {}", info));
        assert!(nodes <= 5000, "{}", said);
    }

    #[test]
    fn the_clock_a_go_names_reaches_the_search() {
        // 500 less the move overhead, which is what time_control works out
        // and what has to arrive on the other side of the call. A named move
        // time arrives named, so the deepening loop spends it rather than
        // answering early and keeping the rest
        assert_eq!(
            asked_of_engine("go movetime 500").limits.clock(),
            Some(Clock::Fixed(Duration::from_millis(450)))
        );
    }

    #[test]
    fn a_search_is_given_the_clock_of_the_side_to_move() {
        // both clocks are on the line and they are far apart, so a search
        // handed the wrong one is handed thirty times the time it has. A
        // share of a game clock arrives as one, since what is not spent on
        // this move is still there for the next
        let line = "go wtime 60000 btime 4000";
        assert_eq!(
            asked_of_engine_as(line, Color::White).limits.clock(),
            Some(Clock::Share(Duration::from_millis(1450))),
            "white was not given its own clock"
        );
        assert_eq!(
            asked_of_engine_as(line, Color::Black).limits.clock(),
            Some(Clock::Share(Duration::from_millis(50))),
            "black was not given its own clock"
        );
    }

    #[test]
    fn a_go_infinite_reaches_the_search_with_no_clock_to_cut_it_short() {
        let asked = asked_of_engine("go infinite wtime 60000");
        assert_eq!(asked.limits.clock(), None);
        assert_eq!(asked.limits.node_budget(), u64::MAX);
    }

    #[test]
    fn a_node_limit_reaches_the_search() {
        let asked = asked_of_engine("go nodes 5000");
        assert_eq!(asked.limits.node_budget(), 5000);
        assert_eq!(asked.limits.clock(), None, "a node limit is not a clock");
    }

    #[test]
    fn a_depth_reaches_the_search_without_a_limit_beside_it() {
        let asked = asked_of_engine("go depth 3");
        assert_eq!(asked.depth, Some(3));
        assert_eq!(asked.limits.clock(), None);
        assert_eq!(asked.limits.node_budget(), u64::MAX);
    }

    #[test]
    fn a_go_saying_nothing_limits_the_search_by_nothing() {
        let asked = asked_of_engine("go");
        assert_eq!(asked.depth, None);
        assert_eq!(asked.limits.clock(), None);
        assert_eq!(asked.limits.node_budget(), u64::MAX);
    }

    #[test]
    fn a_clock_and_a_node_limit_both_reach_the_search() {
        let asked = asked_of_engine("go movetime 500 nodes 5000");
        assert_eq!(
            asked.limits.clock(),
            Some(Clock::Fixed(Duration::from_millis(450)))
        );
        assert_eq!(asked.limits.node_budget(), 5000);
    }

    #[test]
    fn an_unreadable_limit_reaches_the_search_as_no_limit() {
        // read as zero it would stop the search before it had a move
        let asked = asked_of_engine("go nodes abc");
        assert_eq!(asked.limits.node_budget(), u64::MAX);
    }

    #[test]
    fn a_depth_past_the_ply_rail_is_clamped_rather_than_refused() {
        assert_eq!(go_depth(&Params::of("go depth 5")), Some(5));
        assert_eq!(
            go_depth(&Params::of("go depth 999")),
            Some(arche_core::MAX_PLY)
        );
        assert_eq!(go_depth(&Params::of("go infinite")), None);
    }

    #[test]
    fn a_depth_of_two_hundred_and_fifty_five_reaches_the_search_clamped() {
        // the root deepens by one more when it is in check, so a request of
        // the largest depth a byte holds used to overflow it and panic in a
        // debug build. Nothing that arrives now can, because nothing past
        // the rail arrives, and the rail itself leaves room for the
        // extension by a build time assertion beside it
        let asked = asked_of_engine("go depth 255");
        assert_eq!(asked.depth, Some(arche_core::MAX_PLY));
    }

    #[test]
    fn a_bench_command_ends_with_the_line_the_match_tools_read() {
        let mut uci = uci();
        uci.run(Cursor::new("bench 1\n"));
        let said = said(&uci);
        assert!(
            said.starts_with("bench depth 1 hash 16MB positions "),
            "{}",
            said
        );
        let last = said.lines().last().unwrap();
        let words: Vec<&str> = last.split(' ').collect();
        assert_eq!(words.len(), 4, "{}", last);
        assert_eq!((words[1], words[3]), ("nodes", "nps"), "{}", last);
        assert!(words[0].parse::<u64>().unwrap() > 0, "{}", last);
    }

    #[test]
    fn an_unreadable_bench_depth_is_reported_rather_than_searched() {
        // running the default in its place would take seconds and say
        // nothing about why, which is the wrong answer to a typo
        let mut uci = uci();
        uci.run(Cursor::new("bench abc\nbench 300\n"));
        assert_eq!(
            said(&uci),
            "info string unrecognised bench depth: abc\n\
             info string unrecognised bench depth: 300\n"
        );
    }

    #[test]
    fn a_bench_command_takes_a_table_size_and_a_taint_policy() {
        // the two settings the graph history measurements vary, stated
        // back in the header so a report says what it ran with. Each word
        // is optional, the words may come in either order, the depth may
        // be left out ahead of them, and a keyword with nothing after it
        // is the setting left out, as a bare `go depth` is
        let mut uci = uci();
        uci.run(Cursor::new(
            "bench 1 hash 1 taint trust\nbench 1 taint refuse hash 2\nbench hash 1 taint trust\nbench 1 taint\n",
        ));
        let said = said(&uci);
        let headers: Vec<&str> = said
            .lines()
            .filter(|line| line.starts_with("bench depth"))
            .map(|line| {
                let (settings, rest) = line.split_once(" positions ").unwrap();
                let (_, policy) = rest.split_once(' ').unwrap();
                (settings, policy)
            })
            .flat_map(|(settings, policy)| [settings, policy])
            .collect();
        assert_eq!(
            headers,
            [
                "bench depth 1 hash 1MB",
                "taint trust",
                "bench depth 1 hash 2MB",
                "taint refuse",
                "bench depth 7 hash 1MB",
                "taint trust",
                "bench depth 1 hash 16MB",
                "taint rule50",
            ],
            "{}",
            said
        );
    }

    #[test]
    fn an_unreadable_bench_setting_is_reported_rather_than_searched() {
        // a table of no size at all, one the machine cannot hold, and a
        // policy that does not exist are each refused by name
        let mut uci = uci();
        uci.run(Cursor::new(
            "bench 1 hash 0\nbench 1 hash 99999\nbench 1 hash big\nbench 1 taint maybe\n",
        ));
        assert_eq!(
            said(&uci),
            "info string unrecognised bench hash: 0\n\
             info string unrecognised bench hash: 99999\n\
             info string unrecognised bench hash: big\n\
             info string unrecognised bench taint: maybe\n"
        );
    }

    /// The settings alone, not a run: the argument searches the suite twice
    /// over and reading what it was asked for is the part worth pinning.
    #[test]
    fn a_residuals_argument_reads_its_depth_rate_and_policy() {
        let read = |line: &str| {
            let settings = residual_settings(&Params::of(line)).expect(line);
            (
                settings.depth,
                settings.every,
                settings.config.taint_word().to_string(),
            )
        };
        assert_eq!(
            read("residuals"),
            (bench::DEPTH, 1000, "rule50".to_string())
        );
        assert_eq!(read("residuals 4"), (4, 1000, "rule50".to_string()));
        assert_eq!(read("residuals 4 every 50"), (4, 50, "rule50".to_string()));
        // a word standing where the depth would be means the depth was left
        // out rather than mistyped, the same rule the bench reads by
        assert_eq!(
            read("residuals every 50 taint trust"),
            (bench::DEPTH, 50, "trust".to_string())
        );
        // and zero is a rate to ask for: it records every node a shortcut
        // answers, up to the cap
        assert_eq!(read("residuals 2 every 0"), (2, 0, "rule50".to_string()));
    }

    #[test]
    fn an_unreadable_residuals_setting_is_named_rather_than_run() {
        for (line, what) in [
            ("residuals abc", "depth: abc"),
            ("residuals 300", "depth: 300"),
            ("residuals 4 every lots", "every: lots"),
            ("residuals 4 taint maybe", "taint: maybe"),
        ] {
            assert_eq!(
                residual_settings(&Params::of(line)).err(),
                Some(what.to_string()),
                "{line}"
            );
        }
    }

    #[test]
    fn a_perft_command_reads_its_depth() {
        assert_eq!(perft_depth(&Params::of("perft 3")), 3);
        assert_eq!(
            perft_depth(&Params::of("perft")),
            1,
            "a bare perft counts to depth one"
        );
        assert_eq!(perft_depth(&Params::of("perft 999")), u8::MAX);
    }

    #[test]
    fn a_perft_command_counts_without_disturbing_the_position() {
        let mut uci = uci();
        uci.parse_position("position startpos").unwrap();
        assert_eq!(uci.engine.perft(2), 400);
        // counting is make/undo all the way down, the position must survive it
        assert_eq!(uci.engine.active_color(), Color::White);
    }

    use arche_core::Play;

    /// The move of this name in the starting position, for building the
    /// synthetic results the format tests pin.
    fn play_named(name: &str) -> Play {
        *Board::new()
            .generate_moves()
            .iter()
            .find(|m| format!("{}", m) == name)
            .unwrap_or_else(|| panic!("{} is not a move here", name))
    }

    #[test]
    fn an_info_line_reports_a_centipawn_score() {
        let result = SearchResult {
            nodes: 2000,
            elapsed: Duration::from_millis(500),
            selective_depth: 7,
            best_move: play_named("e2e4"),
            score: 25,
        };
        let pv = PvLine::new(vec![play_named("e2e4"), play_named("g1f3")]);
        assert_eq!(
            format_info(5, &result, &pv),
            "info depth 5 seldepth 7 nodes 2000 time 500 nps 4000 score cp 25 pv e2e4 g1f3"
        );
    }

    #[test]
    fn an_info_line_reports_a_mate_score_in_moves() {
        // three plies from checkmate reads as mate in two moves
        let result = SearchResult {
            nodes: 1500,
            elapsed: Duration::from_millis(20),
            selective_depth: 4,
            best_move: play_named("e2e4"),
            score: 30_000 - 3,
        };
        let pv = PvLine::new(vec![play_named("e2e4")]);
        assert_eq!(
            format_info(4, &result, &pv),
            "info depth 4 seldepth 4 nodes 1500 time 20 nps 75000 score mate 2 pv e2e4"
        );
    }

    #[test]
    fn a_search_faster_than_a_millisecond_still_reports_a_rate() {
        let result = SearchResult {
            nodes: 300,
            elapsed: Duration::ZERO,
            selective_depth: 1,
            best_move: play_named("e2e4"),
            score: 0,
        };
        let pv = PvLine::new(vec![]);
        assert_eq!(
            format_info(1, &result, &pv),
            "info depth 1 seldepth 1 nodes 300 time 0 nps 300000 score cp 0 pv "
        );
    }

    // ---- generated sessions -------------------------------------------

    /// Every line the protocol lets an engine say. Anything else is the
    /// engine muttering where an interface can hear it, which is how a
    /// handshake ends up mis-parsed by something that was only ever going to
    /// read the words it knows.
    const SPOKEN: [&str; 6] = ["info", "bestmove", "id", "option", "uciok", "readyok"];

    /// The keywords a generated line opens with: the ones the loop dispatches
    /// on, and near misses that fall through to the unrecognised branch.
    ///
    /// `bench` is deliberately absent. It runs a real bench of seventeen
    /// million nodes whoever is behind the loop, so a generated one would
    /// take a couple of seconds a case, and it prints its report as a table
    /// rather than as protocol. That table is fine — nothing but a person
    /// types `bench` at an engine — but it is not a line an interface could
    /// read, so a session containing one cannot be asked the question below.
    const KEYWORDS: [&str; 15] = [
        "uci",
        "isready",
        "ucinewgame",
        "setoption",
        "position",
        "go",
        "perft",
        "display",
        "stop",
        "ponderhit",
        "debug",
        "goodbye",
        "positional",
        "",
        "  go",
    ];

    /// A word a command line might carry: a keyword the protocol defines, a
    /// number of the shapes an interface really sends, something shaped like
    /// a square or a move, and junk.
    fn word() -> impl Strategy<Value = String> {
        prop_oneof![
            prop::sample::select(vec![
                "name",
                "value",
                "Hash",
                "Threads",
                "startpos",
                "fen",
                "moves",
                "wtime",
                "btime",
                "winc",
                "binc",
                "movestogo",
                "movetime",
                "infinite",
                "depth",
                "nodes",
                "hash",
                "taint",
            ])
            .prop_map(String::from),
            prop::sample::select(vec![
                "0",
                "1",
                "-1",
                "16",
                "300000",
                "99999999999999999999",
                "3.5",
            ])
            .prop_map(String::from),
            prop::sample::select(vec![
                "e2e4", "e7e8q", "0000", "e9e9", "a1", "-", "refuse", "trust",
            ])
            .prop_map(String::from),
            "[a-zA-Z0-9]{1,6}",
        ]
    }

    fn line() -> impl Strategy<Value = String> {
        (
            prop::sample::select(&KEYWORDS[..]),
            prop::collection::vec(word(), 0..7usize),
        )
            .prop_map(|(keyword, words)| {
                let mut line = keyword.to_string();
                for word in words {
                    line.push(' ');
                    line.push_str(&word);
                }
                line
            })
    }

    fn session() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(line(), 0..12usize)
    }

    /// How many of these lines the loop will dispatch as a `go`, counted
    /// with the dispatcher's own reading of a line, so the count cannot
    /// disagree with the loop about what one is.
    fn gos(lines: &[String]) -> usize {
        lines.iter().filter(|line| first_word(line) == "go").count()
    }

    fn bestmoves(said: &str) -> usize {
        said.lines()
            .filter(|line| line.starts_with("bestmove"))
            .count()
    }

    fn run_session(lines: &[String]) -> String {
        let mut uci = UCI::with_output(Recorder::to_move(Color::White), Vec::new());
        uci.run(Cursor::new(lines.join("\n") + "\n"));
        String::from_utf8(uci.out.clone()).unwrap()
    }

    proptest! {
        /// The loop answers whatever arrives and says only things the
        /// protocol defines.
        #[test]
        fn a_session_is_answered_in_the_protocol(lines in session()) {
            for line in run_session(&lines).lines() {
                prop_assert!(
                    SPOKEN.iter().any(|keyword| line.starts_with(keyword)),
                    "said something the protocol does not define: {}",
                    line
                );
            }
        }

        /// Exactly one bestmove for every go. This is the promise an
        /// interface waits on: none and the game hangs on our clock, two and
        /// the second is read as the answer to whatever go comes next, which
        /// is a move played in a position it was not chosen for.
        #[test]
        fn every_go_is_answered_exactly_once(lines in session()) {
            let said = run_session(&lines);
            prop_assert_eq!(gos(&lines), bestmoves(&said), "said: {}", said);
        }

        /// Nothing after a quit is read, whatever it is.
        #[test]
        fn a_quit_ends_the_session(before in session(), after in session()) {
            let mut lines = before.clone();
            lines.push("quit".to_string());
            lines.extend(after);
            let said = run_session(&lines);
            prop_assert_eq!(gos(&before), bestmoves(&said), "the lines after a quit were read");
        }
    }

    proptest! {
        // A real search behind the loop, so the recording engine is not the
        // only thing these promises have been checked against. Few cases:
        // every one of them searches, and the point is the promises rather
        // than the coverage the sessions above already give.
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn a_real_engine_keeps_the_same_promises(lines in prop::collection::vec(line(), 0..5usize)) {
            // Two things a real search will not survive being asked at
            // random. A generated perft depth is a number like 300000,
            // which would not finish this century; the recording engine
            // answers perft with a zero, so the sessions above are where its
            // parsing is covered. And a go with no clock in it searches to
            // the depth cap, which is seconds a case, so every go here is
            // given a move time. It goes straight after the keyword because
            // the reader takes the first of a repeated word, so this one
            // wins over whatever the generator put further along; infinite
            // would beat it whatever it said, so it comes out.
            let lines: Vec<String> = lines
                .into_iter()
                .filter(|line| first_word(line) != "perft")
                .map(|line| match first_word(&line) {
                    "go" => {
                        let rest = line.split_once("go").map_or("", |(_, rest)| rest);
                        format!("go movetime 5 {}", rest.replace("infinite", ""))
                    }
                    _ => line,
                })
                .collect();
            let mut uci = UCI::with_output(
                AlphaBeta::with_table_bytes(Board::new(), 8 * 1024),
                Vec::new(),
            );
            uci.run(Cursor::new(lines.join("\n") + "\n"));
            let spoken = said(&uci);

            for line in spoken.lines() {
                prop_assert!(
                    SPOKEN.iter().any(|keyword| line.starts_with(keyword)),
                    "said something the protocol does not define: {}",
                    line
                );
            }
            prop_assert_eq!(gos(&lines), bestmoves(&spoken), "said: {}", spoken);
        }
    }

    // ---- the session loop ---------------------------------------------

    /// A session on threads of its own, driven the way an interface drives
    /// one: lines typed in, and what was said read back while it is being
    /// said rather than after the loop has ended.
    struct Driven {
        typed: Sender<String>,
        said: SharedWriter<Vec<u8>>,
        session: thread::JoinHandle<()>,
    }

    impl Driven {
        fn of<T: Engine + Send + 'static>(engine: T) -> Self {
            let (typed, script) = channel::<String>();
            let said = SharedWriter::new(Vec::new());
            let out = said.clone();
            // the production wiring, entered through the production door:
            // the only substitution is the input
            let session = thread::spawn(move || {
                UCI::with_output(engine, out).wire(move || script.into_iter().map(Ok));
            });
            Self {
                typed,
                said,
                session,
            }
        }

        /// A real search behind the loop, on a table small enough to afford
        /// one per test.
        fn searching() -> Self {
            Self::of(AlphaBeta::with_table_bytes(Board::new(), 8 * 1024))
        }

        /// A session whose engine answers at once, which is how the holding
        /// of an answer is tested without waiting for a search to run out
        /// of depths.
        fn instant() -> Self {
            Self::of(Recorder::to_move(Color::White))
        }

        fn type_line(&self, line: &str) {
            let _ = self.typed.send(line.to_string());
        }

        fn said(&self) -> String {
            self.said.read_back()
        }

        /// Everything said once `what` has been, or a failure naming what
        /// was said instead. Generous, because it bounds a real search on
        /// whatever machine is running the suite rather than measuring one.
        fn wait_for(&self, what: &str) -> String {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let said = self.said();
                if said.contains(what) {
                    return said;
                }
                assert!(
                    Instant::now() < deadline,
                    "nothing said {:?} in thirty seconds, only: {}",
                    what,
                    said
                );
                thread::sleep(Duration::from_millis(1));
            }
        }

        /// Nothing more is said for this long. Used where the promise is
        /// that an answer is held back, which nothing but waiting can show.
        fn stays_quiet_for(&self, span: Duration) -> String {
            let said = self.said();
            thread::sleep(span);
            assert_eq!(said, self.said(), "something was said in the meantime");
            said
        }

        /// Close the interface and wait for the session to end.
        fn finish(self) -> String {
            drop(self.typed);
            self.session.join().expect("the session panicked");
            self.said.read_back()
        }
    }

    #[test]
    fn a_panic_is_said_where_the_interface_can_read_it() {
        // the hook is process wide, so the test reads back its own buffer
        // rather than asserting anything about the process's stdout
        let said = SharedWriter::new(Vec::new());
        report_panics_to(said.clone());
        let panicked = thread::spawn(|| panic!("the search fell over")).join();
        assert!(panicked.is_err());
        let line = said.read_back();
        assert!(
            line.starts_with("info string panicked at ") && line.contains("the search fell over"),
            "the panic was reported as: {}",
            line
        );
    }

    /// The last thing said, which is the bestmove in every session here.
    fn last_line(said: &str) -> &str {
        said.lines().last().unwrap_or("")
    }

    #[test]
    fn a_stop_mid_search_answers_with_a_real_move() {
        // twenty seconds of move time, stopped inside the first second of
        // it: the search comes back with the move it had rather than with
        // the null move, and it comes back at once
        let driven = Driven::searching();
        driven.type_line("position startpos");
        driven.type_line("go movetime 20000");
        driven.wait_for("info depth 1");
        let stopped = Instant::now();
        driven.type_line("stop");
        let said = driven.wait_for("bestmove");
        assert!(
            stopped.elapsed() < Duration::from_secs(10),
            "the stop was not acted on: {}",
            said
        );
        assert_ne!(
            last_line(&said),
            "bestmove 0000",
            "a stopped search answered with no move: {}",
            said
        );
        driven.finish();
    }

    #[test]
    fn a_stop_while_nothing_is_searching_is_taken_in_silence() {
        // it used to come back as an unrecognised command, which is what
        // an interface cancelling a game gets told today
        let driven = Driven::instant();
        driven.type_line("stop");
        driven.type_line("isready");
        driven.wait_for("readyok");
        assert_eq!(driven.finish(), "readyok\n");
    }

    #[test]
    fn an_isready_is_answered_while_a_search_runs() {
        // the protocol requires this one to be answered at once, whatever
        // the engine is in the middle of, and the reader thread answers it
        let driven = Driven::searching();
        driven.type_line("position startpos");
        driven.type_line("go movetime 20000");
        driven.wait_for("info depth 1");
        driven.type_line("isready");
        let said = driven.wait_for("readyok");
        assert!(
            !said.contains("bestmove"),
            "the search had already answered: {}",
            said
        );
        driven.type_line("stop");
        driven.wait_for("bestmove");
        driven.finish();
    }

    #[test]
    fn a_go_infinite_holds_its_move_until_a_stop_arrives() {
        // the engine behind this one answers at once, so the deepening is
        // over long before the stop. The bestmove still waits for it,
        // which is what infinite means
        let driven = Driven::instant();
        driven.type_line("go infinite");
        assert_eq!(
            driven.stays_quiet_for(Duration::from_millis(50)),
            "",
            "an infinite search answered without being stopped"
        );
        driven.type_line("stop");
        driven.wait_for("bestmove");
        driven.finish();
    }

    #[test]
    fn a_bare_go_holds_its_move_the_way_an_infinite_one_does() {
        // a go with nothing to bound it is an infinite one said differently
        let driven = Driven::instant();
        driven.type_line("go");
        assert_eq!(driven.stays_quiet_for(Duration::from_millis(50)), "");
        driven.type_line("stop");
        driven.wait_for("bestmove");
        driven.finish();
    }

    #[test]
    fn the_interface_leaving_ends_a_held_answer() {
        // a pipe that closes without a quit is an interface that has gone.
        // The hold used to park on a stop that could no longer come, and
        // the process outlived the only party that wanted its answer
        let driven = Driven::instant();
        driven.type_line("go infinite");
        let said = driven.finish();
        assert!(
            last_line(&said).starts_with("bestmove"),
            "the held answer was never said: {}",
            said
        );
    }

    #[test]
    fn a_bounded_go_answers_without_being_stopped() {
        // the other side of the rule: anything that says what it may spend
        // answers when it has spent it
        let driven = Driven::instant();
        driven.type_line("go depth 3");
        driven.wait_for("bestmove");
        driven.finish();
    }

    #[test]
    fn a_position_sent_during_a_search_is_applied_after_it() {
        // dropping it would leave the interface's idea of the game and the
        // engine's apart in silence, and the next go would search a
        // position nobody asked about. The second go proves which position
        // the engine ended up on: from that one there is no move at all
        let driven = Driven::searching();
        driven.type_line("position startpos");
        driven.type_line("go movetime 20000");
        driven.wait_for("info depth 1");
        driven.type_line("position fen 7k/6Q1/6K1/8/8/8/8/8 b - - 0 1");
        driven.type_line("go depth 1");
        driven.type_line("stop");
        driven.wait_for("bestmove 0000");
        let said = driven.finish();
        let bestmoves: Vec<&str> = said
            .lines()
            .filter(|line| line.starts_with("bestmove"))
            .collect();
        assert_eq!(bestmoves.len(), 2, "{}", said);
        assert_ne!(bestmoves[0], "bestmove 0000", "{}", said);
        assert_eq!(bestmoves[1], "bestmove 0000", "{}", said);
    }

    #[test]
    fn a_quit_during_a_search_answers_before_it_exits() {
        // every go gets a bestmove, unconditionally: there is one path out
        // of a search and it ends in an answer
        let driven = Driven::searching();
        driven.type_line("position startpos");
        driven.type_line("go movetime 20000");
        driven.wait_for("info depth 1");
        driven.type_line("quit");
        let said = driven.finish();
        assert!(last_line(&said).starts_with("bestmove"), "{}", said);
        assert_ne!(last_line(&said), "bestmove 0000", "{}", said);
    }

    #[test]
    fn what_holds_its_answer_is_what_nothing_bounds() {
        let unbounded = Limits::starting_now(None, None);
        assert!(holds_its_answer(
            &Params::of("go infinite"),
            None,
            &unbounded
        ));
        assert!(holds_its_answer(&Params::of("go"), None, &unbounded));
        // infinite outranks anything sent beside it, as the protocol says
        assert!(holds_its_answer(
            &Params::of("go infinite depth 2"),
            Some(2),
            &unbounded
        ));
        assert!(!holds_its_answer(
            &Params::of("go depth 2"),
            Some(2),
            &unbounded
        ));
        assert!(!holds_its_answer(
            &Params::of("go nodes 5000"),
            None,
            &Limits::starting_now(None, Some(5000))
        ));
        assert!(!holds_its_answer(
            &Params::of("go movetime 500"),
            None,
            &Limits::starting_now(Some(Clock::Fixed(Duration::from_millis(450))), None)
        ));
    }
}
