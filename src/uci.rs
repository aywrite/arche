use crate::params::{Param, Params};
use crate::time_control::TimeControl;
use basic_engine::Color;
use basic_engine::Engine;
use basic_engine::Limits;
use basic_engine::SearchConfig;
use basic_engine::SearchOutcome;
use basic_engine::SearchParameters;
use basic_engine::bench;
use basic_engine::{PvLine, SearchResult};
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
const HASH_DEFAULT_MB: u64 = (basic_engine::DEFAULT_TABLE_BYTES / (1024 * 1024)) as u64;

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

impl<T: Engine> UCI<T, Stdout> {
    pub fn new_with_engine(engine: T) -> Self {
        Self::with_output(engine, std::io::stdout())
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

    pub fn read_loop(&mut self) {
        self.run(std::io::stdin().lock());
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

    /// Handles input until it is exhausted or `quit` arrives. Separate from
    /// read_loop so that it can be driven without stdin.
    fn run<R: BufRead>(&mut self, input: R) {
        for line in input.lines() {
            match line {
                Ok(line) => {
                    if !self.handle(&line) {
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

    /// Returns false once the engine has been asked to quit.
    fn handle(&mut self, line: &str) -> bool {
        if line.starts_with("quit") {
            return false;
        }
        if line.starts_with("isready") {
            self.say(format_args!("readyok"));
        } else if line.starts_with("ucinewgame") {
            self.engine.new_game();
            let result = self.parse_position("position startpos");
            self.report(result);
        } else if line.starts_with("uci") {
            // written to the field directly: say borrows all of self, and
            // these lines also read from it
            let _ = writeln!(self.out, "id name {} {}", self.name, self.version);
            let _ = writeln!(self.out, "id author {}", self.author);
            self.say(format_args!(
                "option name Hash type spin default {} min {} max {}",
                HASH_DEFAULT_MB, HASH_MIN_MB, HASH_MAX_MB
            ));
            // advertised with a range of one so that an interface configuring
            // a match reads the engine as single threaded rather than being
            // left to find out by playing one
            self.say(format_args!(
                "option name Threads type spin default 1 min 1 max 1"
            ));
            self.say(format_args!("uciok"));
        } else if line.starts_with("setoption") {
            let result = self.set_option(line);
            self.report(result);
        } else if line.starts_with("position") {
            let result = self.parse_position(line);
            self.report(result);
        } else if line.starts_with("display") {
            self.engine.display_board();
        } else if line.starts_with("go") {
            self.parse_go(line);
        } else if line.starts_with("bench") {
            // the same as the command line argument: the depth is for
            // trying the command cheaply, the number that means anything is
            // the one at the default, and the table and policy are for
            // measuring rather than pinning
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
        } else if line.starts_with("perft") {
            let depth = perft_depth(&Params::of(line));
            let nodes = self.engine.perft(depth);
            self.say(format_args!(
                "info string perft depth {} nodes {}",
                depth, nodes
            ));
        } else {
            self.say(format_args!("info string unrecognised command: {}", line));
        }
        true
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
        let position_string = line.strip_prefix("position").unwrap_or(line).trim();
        let (start, move_list) = match position_string.split_once("moves") {
            Some((s, m)) => (s.trim(), Some(m)),
            None => (position_string, None),
        };
        if start.starts_with("startpos") {
            self.engine.parse_fen(basic_engine::STARTING_FEN)?;
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

    fn parse_go(&mut self, line: &str) {
        let params = Params::of(line);
        // the clock starts here, when the command arrived, and the search
        // reports its elapsed time against the same start
        let limits = Limits::starting_now(
            time_control_from(&params, self.engine.active_color()).budget(),
            // an unreadable node limit is ignored rather than obeyed as zero,
            // which would stop the search before it had a move to report
            params.count("nodes").read(),
        );
        let sp = SearchParameters::new(go_depth(&params), limits);

        // the closure writes while the engine is borrowed for the search, so
        // it goes to the writer directly rather than through say
        let out = &mut self.out;
        let outcome = self
            .engine
            .iterative_deepening_search(sp, |depth, result, pv| {
                let _ = writeln!(out, "{}", format_info(depth, result, &pv));
            });
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

/// The depth asked of a go command, if one was. A depth too big for a byte
/// is a request to go deep, not a reason to refuse the command, so it is
/// clamped to the most a byte holds rather than rejected.
fn go_depth(params: &Params) -> Option<u8> {
    params
        .count("depth")
        .read()
        .map(|depth| depth.try_into().unwrap_or(u8::MAX))
}

/// What a bench command or argument asked for: `bench [depth] [hash <MB>]
/// [taint refuse|trust]`, each setting standing in for the bench's own when
/// absent. The depth is for trying the command cheaply; the table and the
/// policy are what the graph history measurements vary, and a report states
/// all three in its header so it can be rerun from it.
pub(crate) struct BenchSettings {
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
pub(crate) fn bench_settings(params: &Params) -> Result<BenchSettings, String> {
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

/// One completed depth as a UCI info line. The elapsed time arrives as a
/// parameter rather than being read from a clock here, so that tests can pin
/// the whole line.
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
    use basic_engine::{AlphaBeta, Board};
    use std::io::Cursor;
    use std::time::Duration;

    /// A table small enough that a test can afford one per case, speaking
    /// into a buffer so that what the interface would see can be asserted.
    fn uci() -> UCI<AlphaBeta, Vec<u8>> {
        UCI::with_output(
            AlphaBeta::with_table_bytes(Board::new(), 8 * 1024),
            Vec::new(),
        )
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
    fn a_search_runs_on_a_table_the_hash_option_resized() {
        let mut uci = uci();
        uci.run(Cursor::new(
            "setoption name Hash value 1\nposition startpos\ngo depth 3\n",
        ));
        let said = said(&uci);
        assert!(
            said.lines().last().unwrap_or("").starts_with("bestmove "),
            "{}",
            said
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
    fn an_unreadable_limit_is_ignored_rather_than_obeyed_as_zero() {
        // zero would be a limit of nothing, and the search would come back
        // without a move rather than without a limit
        assert_eq!(go_depth(&Params::of("go depth abc")), None);
        assert_eq!(Params::of("go nodes abc").count("nodes").read(), None);
    }

    #[test]
    fn a_keyword_inside_a_longer_word_is_not_one() {
        // the regexes this replaced had no word boundary, so a clock could be
        // read out of the middle of another token
        let control = time_control_from(&Params::of("go xwtime 300000"), Color::White);
        assert_eq!(control.time, None);
    }

    #[test]
    fn a_node_limit_is_read() {
        assert_eq!(
            Params::of("go nodes 1234").count("nodes").read(),
            Some(1234)
        );
        assert_eq!(Params::of("go depth 3").count("nodes").read(), None);
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
    fn a_depth_too_big_for_a_byte_is_clamped_rather_than_refused() {
        assert_eq!(go_depth(&Params::of("go depth 5")), Some(5));
        assert_eq!(go_depth(&Params::of("go depth 999")), Some(u8::MAX));
        assert_eq!(go_depth(&Params::of("go infinite")), None);
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
                "taint refuse",
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

    use basic_engine::Play;

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
}
