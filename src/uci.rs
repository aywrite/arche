// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::command::{Command, Keyword};
use crate::params::{Param, Params};
use crate::session::{self, SessionControl, SharedWriter, first_word, report_panics_to};
use crate::time_control::{DEFAULT_MOVE_OVERHEAD_MS, TimeControl};
use arche_core::Color;
use arche_core::Engine;
use arche_core::Limits;
use arche_core::ScoreBound;
use arche_core::SearchConfig;
use arche_core::SearchOutcome;
use arche_core::SearchParameters;
use arche_core::bench;
use arche_core::{PvLine, SearchResult};
use std::io::{BufRead, Stdout, Write};
use std::ops::RangeInclusive;

/// The `Hash` option's range, in megabytes, as the handshake advertises it.
/// A `bench hash` takes the same range.
///
/// The top is sixteen gibibytes, held to what a usize can address so that a
/// narrower target advertises a range that can be asked for rather than one
/// that would overflow.
const HASH_MIN_MB: u64 = 1;
const HASH_MAX_MB: u64 = {
    let addressable = (usize::MAX / (1024 * 1024)) as u64;
    if addressable < 16 * 1024 {
        addressable
    } else {
        16 * 1024
    }
};

/// The handshake's default is the engine's own, so an interface that never
/// sends a `setoption` is told the table it is going to get.
const HASH_DEFAULT_MB: u64 = (arche_core::DEFAULT_TABLE_BYTES / (1024 * 1024)) as u64;

// a default outside the advertised range is caught here rather than in a game
const _: () = assert!(HASH_DEFAULT_MB >= HASH_MIN_MB && HASH_DEFAULT_MB <= HASH_MAX_MB);

/// The `Move Overhead` option's range, in milliseconds. Zero, because an
/// interface on the same machine may cost nothing worth holding back. Five
/// seconds is more than a network needs, and an interface that asks for it at
/// 1+0 gets the floor a spent clock gets.
const OVERHEAD_MIN_MS: u64 = 0;
const OVERHEAD_MAX_MS: u64 = 5_000;

// only the top: a u64 cannot fall below a minimum of zero
const _: () = assert!(DEFAULT_MOVE_OVERHEAD_MS <= OVERHEAD_MAX_MS);

/// Where a spin option's value goes. A row names its own, and `set_option`
/// matches over this, so a variant with no arm fails the build.
#[derive(Clone, Copy)]
enum Setting {
    Hash,
    Threads,
    MoveOverhead,
}

/// What kind of thing an option is, as the handshake says it.
enum OptionKind {
    /// A number in a range, and what that number is read into.
    Spin {
        default: u64,
        min: u64,
        max: u64,
        setting: Setting,
    },
    /// Pressed rather than set: no value and no state to advertise.
    Button,
}

/// One option the engine answers to.
struct UciOption {
    name: &'static str,
    kind: OptionKind,
}

/// The options, in the order the handshake says them. The one statement of
/// what exists: a row carries its name, its kind, and for a spin the range
/// it advertises and the setting that range is read into.
const OPTIONS: &[UciOption] = &[
    UciOption {
        name: "Hash",
        kind: OptionKind::Spin {
            default: HASH_DEFAULT_MB,
            min: HASH_MIN_MB,
            max: HASH_MAX_MB,
            setting: Setting::Hash,
        },
    },
    UciOption {
        name: "Clear Hash",
        kind: OptionKind::Button,
    },
    // a range of one, so an interface configuring a match reads the engine
    // as single threaded rather than finding out by playing
    UciOption {
        name: "Threads",
        kind: OptionKind::Spin {
            default: 1,
            min: 1,
            max: 1,
            setting: Setting::Threads,
        },
    },
    UciOption {
        name: "Move Overhead",
        kind: OptionKind::Spin {
            default: DEFAULT_MOVE_OVERHEAD_MS,
            min: OVERHEAD_MIN_MS,
            max: OVERHEAD_MAX_MS,
            setting: Setting::MoveOverhead,
        },
    },
];

impl UciOption {
    /// The handshake line for this option.
    fn advert(&self) -> String {
        match self.kind {
            OptionKind::Spin {
                default, min, max, ..
            } => format!(
                "option name {} type spin default {} min {} max {}",
                self.name, default, min, max
            ),
            OptionKind::Button => format!("option name {} type button", self.name),
        }
    }
}

/// A spin value read off a `setoption` line and held to the range its row
/// advertises: what to apply, and what to say when it had to be held.
#[derive(Debug, PartialEq, Eq)]
struct Held {
    value: u64,
    said: Option<String>,
}

/// Reads one spin value for the row named, holding it to that row's range.
/// Parsed rather than counted: a count reads a negative as a spent clock,
/// which is right for a clock and wrong here, since a negative is outside
/// every range the handshake advertises. A value outside the range is asking
/// for more than we offer rather than making a mistake worth refusing, so it
/// gets the nearest end and is told which, in the word it was sent as rather
/// than the number that was read. The range travels as one argument rather
/// than as two ends of the same type, which would swap unnoticed.
fn read_spin(name: &str, range: RangeInclusive<u64>, params: &Params) -> Result<Held, String> {
    let (word, value) = match (params.value("value"), params.parse::<u64>("value")) {
        (Some(word), Param::Read(value)) => (word, value),
        (_, Param::Unreadable(word)) => {
            return Err(format!("unrecognised {} value: {}", name, word));
        }
        _ => return Err(format!("{} was sent without a value", name)),
    };
    let held = value.clamp(*range.start(), *range.end());
    let said = (held != value).then(|| {
        format!(
            "info string {} {} is outside {} to {}, using {}",
            name,
            word,
            range.start(),
            range.end(),
            held
        )
    });
    Ok(Held { value: held, said })
}

pub struct UCI<T: Engine, W: Write> {
    author: String,
    name: String,
    version: String,

    /// What the `Move Overhead` option is set to, held back from every budget
    /// a `go` works out. It outlives a game: it describes the connection, not
    /// the position.
    move_overhead: u64,

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
    /// Install the panic hook speaking through this session's writer, so the
    /// reason the engine died goes out under the same lock as every other
    /// line.
    pub fn report_panics(&self) {
        report_panics_to(self.out.clone());
    }

    /// Run this session over the input given. Every line comes back to
    /// `dispatch` on this thread, so the engine never crosses to the reader.
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
            move_overhead: DEFAULT_MOVE_OVERHEAD_MS,
            engine,
            out,
        }
    }

    /// A command is its first word. A `go` is bracketed so the reader thread
    /// knows a search is running and can stop it.
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
                // the reader thread already stopped the search; by the time
                // one reaches here the flag it set is spent. Taken in
                // silence, since the protocol allows a stop at any moment
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
                for option in OPTIONS {
                    self.say(format_args!("{}", option.advert()));
                }
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
                // one info string a row, so an interface reads the dump as
                // commentary rather than as protocol it has to parse; the
                // blank separator rows are dropped
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
    /// is gone, which leaves no one to tell, so the error is dropped.
    fn say(&mut self, line: std::fmt::Arguments) {
        let _ = writeln!(self.out, "{}", line);
    }

    /// Says what could not be acted on and carries on reading: a bad line is
    /// the interface's problem to fix.
    fn report(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.say(format_args!("info string {}", error));
        }
    }

    /// One line through the real dispatcher, with no reader thread.
    #[cfg(test)]
    fn handle(&mut self, line: &str) -> bool {
        self.dispatch(line, &SessionControl::unattended())
    }

    /// Dispatches input until it is exhausted or `quit` arrives, through the
    /// shipped session loop with the channel filled up front and no reader
    /// thread, so nothing can interrupt a search.
    #[cfg(test)]
    fn run<R: BufRead>(&mut self, input: R) {
        let (sender, lines) = std::sync::mpsc::channel();
        for line in input.lines() {
            sender
                .send(line.expect("a test script reads"))
                .expect("the lines are read after the channel is filled");
        }
        drop(sender);
        session::session_loop(lines, &SessionControl::unattended(), |line, control| {
            self.dispatch(line, control)
        });
    }

    /// The same bench as the command line argument.
    fn bench(&mut self, line: &str) {
        match bench_settings(&Params::of(line)) {
            Ok(settings) => match settings.run() {
                Some(report) => self.say(format_args!("{}", report)),
                None => self.say(format_args!("info string {}", NO_AUDIT_MEMORY)),
            },
            Err(what) => self.say(format_args!("info string unrecognised bench {}", what)),
        }
    }

    /// `setoption name <option> [value <value>]`. An option not in `OPTIONS`
    /// is said back rather than acted on, so an interface sending one meant
    /// for another engine is told. The name is every word between `name` and
    /// `value`, since `Clear Hash` has two.
    fn set_option(&mut self, line: &str) -> Result<(), String> {
        let params = Params::of(line);
        let Some(name) = params.phrase("name", "value") else {
            return Err(format!("setoption without an option name: {}", line));
        };
        let Some(option) = OPTIONS.iter().find(|option| option.name == name) else {
            return Err(format!("unrecognised option: {}", name));
        };
        match option.kind {
            // a button carries no value. Read off the kind rather than the
            // name, so every button empties the table; there is one today,
            // and a second would want an enum of its own beside `Setting`.
            // Only the table is emptied: the killers and the history start
            // empty at every `go` anyway
            OptionKind::Button => {
                self.engine.clear_table();
                Ok(())
            }
            OptionKind::Spin {
                min, max, setting, ..
            } => self.set_spin(option.name, min..=max, setting, &params),
        }
    }

    /// Reads one spin's value, says what the reader had to say about it, and
    /// puts it where the row asks.
    fn set_spin(
        &mut self,
        name: &str,
        range: RangeInclusive<u64>,
        setting: Setting,
        params: &Params,
    ) -> Result<(), String> {
        let Held { value, said } = read_spin(name, range, params)?;
        if let Some(said) = said {
            self.say(format_args!("{}", said));
        }
        match setting {
            // rebuilding the table empties it, which is what the protocol
            // expects of a size change
            Setting::Hash => {
                if !self.engine.set_table_bytes(value as usize * 1024 * 1024) {
                    return Err(format!(
                        "no memory for a {}MB table, keeping the one we have",
                        value
                    ));
                }
            }
            // there is no parallel search, so the count is held to one and
            // nothing is kept. Held and said rather than refused: declining
            // to play because a match was configured for four threads would
            // be worse than playing on one
            Setting::Threads => {}
            Setting::MoveOverhead => self.move_overhead = value,
        }
        Ok(())
    }

    /// A move that cannot be played leaves the position at the last one that
    /// could be, since the interface is expected to send the whole line again
    /// rather than to carry on from a position we rejected.
    fn parse_position(&mut self, line: &str) -> Result<(), String> {
        // trimmed before the strip, so this reads the line the dispatcher did
        let position_string = line
            .trim_start()
            .strip_prefix("position")
            .unwrap_or(line)
            .trim();
        // the move list begins at the first "moves" standing as a word of its
        // own: startposmoves is not startpos
        let moves_at = position_string.match_indices("moves").find(|(at, word)| {
            let before = position_string[..*at].chars().next_back();
            let after = position_string[at + word.len()..].chars().next();
            before.is_none_or(char::is_whitespace) && after.is_none_or(char::is_whitespace)
        });
        let (start, move_list) = match moves_at {
            Some((at, word)) => (
                position_string[..at].trim(),
                Some(&position_string[at + word.len()..]),
            ),
            None => (position_string, None),
        };
        // whole words, as the go line reads them: startposx is not startpos
        if start == "startpos" {
            self.engine.parse_fen(arche_core::STARTING_FEN)?;
        } else if let Some(fen) = start
            .strip_prefix("fen")
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
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

    /// A `go`: the reader thread's stop flag rides into the search, and a go
    /// that holds its answer waits here for the stop that releases it.
    fn parse_go(&mut self, line: &str, control: &SessionControl) {
        let go = Go::of(
            &Params::of(line),
            self.engine.active_color(),
            self.move_overhead,
        );
        let sp = SearchParameters::stoppable(go.depth, go.limits(), control.handle());

        // the closure writes while the engine is borrowed for the search, so
        // it goes to the writer directly rather than through say
        let out = &mut self.out;
        let outcome = self
            .engine
            .iterative_deepening_search(sp, |depth, result, pv, bound| {
                let _ = writeln!(out, "{}", format_info(depth, result, &pv, bound));
            });
        // an infinite search does not answer until it is told to, even when
        // it ran out of depths to search first
        if go.holds_its_answer() {
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

/// What a `go` asked for, read once from the line. Each part read from the
/// line is absent when the line did not name it.
struct Go {
    /// The depth asked for, held to the ply rail rather than refused: a depth
    /// past what the engine will search is a request to go deep. The rail is
    /// also what keeps the root's check extension inside a byte (a depth of
    /// 255 from a position in check used to overflow).
    depth: Option<u8>,
    /// The node budget. An unreadable one is ignored rather than obeyed as
    /// zero, which would stop the search before it had a move to report.
    nodes: Option<u64>,
    time: TimeControl,
    /// The session's `Move Overhead` when the `go` arrived. Not a word off the
    /// line, but the budget is worked out from it.
    overhead: u64,
}

impl Go {
    fn of(params: &Params, color: Color, overhead: u64) -> Self {
        Go {
            depth: params
                .count("depth")
                .read()
                .map(|depth| depth.try_into().unwrap_or(u8::MAX).min(arche_core::MAX_PLY)),
            nodes: params.count("nodes").read(),
            time: TimeControl::of(params, color),
            overhead,
        }
    }

    /// The bounds the search runs under. The clock starts here, as the command
    /// arrives, and the search reports its elapsed time against the same
    /// start.
    fn limits(&self) -> Limits {
        Limits::starting_now(self.time.budget(self.overhead), self.nodes)
    }

    /// Whether this `go` must sit on its answer until a `stop` arrives.
    ///
    /// `go infinite` says so outright. A `go` with nothing to bound it is read
    /// the same way: it used to mean a search to the depth cap on no clock,
    /// which nothing sends deliberately.
    fn holds_its_answer(&self) -> bool {
        // a node count too large to hold reads as u64::MAX, which is also
        // what no budget is, so it bounds nothing either
        self.time.infinite
            || (self.depth.is_none()
                && self.nodes.unwrap_or(u64::MAX) == u64::MAX
                && self.time.budget(self.overhead).is_none())
    }
}

/// What a bench command or argument asked for: `bench [depth] [hash <MB>]
/// [taint refuse|trust|skip|rule50] [audit]`, each setting the bench's own
/// when absent. The report states the depth, table and policy in its header
/// so it can be rerun from it.
pub struct BenchSettings {
    pub depth: u8,
    pub table_bytes: usize,
    pub config: SearchConfig,
    /// Whether each table keeps the full key of every entry, so the report
    /// can say how often an entry's signature accepted another position's.
    pub audit: bool,
}

/// What a bench takes: the usage's spelling and the words it refuses.
pub const BENCH: Command = Command {
    name: "bench",
    depth: true,
    keywords: &[
        Keyword {
            word: "hash",
            value: "<MB>",
        },
        Keyword {
            word: "taint",
            value: "refuse|trust|skip|rule50",
        },
    ],
    flags: &["audit"],
    summary: &[
        "search a fixed suite and print what each search counted,",
        "with audit adding what the table's key signature cost",
    ],
};

/// Reads the bench settings, or names the setting and the word that could not
/// be read. Running the default in its place would take seconds and explain
/// nothing.
pub fn bench_settings(params: &Params) -> Result<BenchSettings, String> {
    let depth = match params.parse::<u8>("bench") {
        Param::Absent => bench::DEPTH,
        Param::Read(depth) => depth,
        Param::Unreadable(word) if BENCH.takes(word) => bench::DEPTH,
        Param::Unreadable(word) => return Err(format!("depth: {word}")),
    };
    let table_bytes = match params.parse::<u64>("hash") {
        Param::Absent => bench::TABLE_BYTES,
        // the range the uci Hash option advertises
        Param::Read(mb) if (HASH_MIN_MB..=HASH_MAX_MB).contains(&mb) => mb as usize * 1024 * 1024,
        Param::Read(mb) => return Err(format!("hash: {mb}")),
        Param::Unreadable(word) => return Err(format!("hash: {word}")),
    };
    let config = match params.value("taint") {
        None => SearchConfig::default(),
        Some(word) => SearchConfig::with_taint(word).ok_or_else(|| format!("taint: {word}"))?,
    };
    // last, so a word that was going to be read as the depth has already
    // been refused under the better name
    BENCH.claim(params)?;
    Ok(BenchSettings {
        depth,
        table_bytes,
        config,
        audit: params.flag("audit"),
    })
}

/// What is said when the audit's keys cannot be had, shared by the command
/// and the argument. Refused rather than run without: a report without the
/// figures reads like a run that found nothing.
pub const NO_AUDIT_MEMORY: &str = "no memory for the audit's keys, which are half the table again";

impl BenchSettings {
    /// Runs the bench, or nothing when the audit's keys could not be
    /// allocated, which the caller reports with `NO_AUDIT_MEMORY`. The
    /// command and the argument both come through here.
    pub fn run(&self) -> Option<bench::Report> {
        let positions = bench::positions();
        if self.audit {
            bench::run_audited_suite(&positions, self.depth, self.table_bytes, self.config)
        } else {
            Some(bench::run_suite(
                &positions,
                self.depth,
                self.table_bytes,
                self.config,
            ))
        }
    }
}

/// The depth asked of a perft command. A bare `perft` counts to depth one. A
/// depth too big for a byte is clamped rather than refused: perft is asked
/// for by hand, and the answer to too deep is to wait or interrupt.
fn perft_depth(params: &Params) -> u8 {
    params
        .count("perft")
        .read()
        .map(|depth| depth.try_into().unwrap_or(u8::MAX))
        .unwrap_or(1)
}

/// One report from the search as a UCI info line. The elapsed time comes from
/// the result rather than a clock read here, so the rate divides a node count
/// by the time that same search took.
///
/// A score proved over some of the root moves rather than all of them is
/// qualified `lowerbound`, the protocol's word for it.
fn format_info(depth: u8, result: &SearchResult, pv: &PvLine, bound: ScoreBound) -> String {
    let millis = result.elapsed.as_millis();
    // a search faster than a millisecond is measured as one, so the rate
    // stays finite
    let nps = (result.nodes as u128 * 1000 / millis.max(1)) as u64;
    let qualifier = match bound {
        ScoreBound::Exact => "",
        ScoreBound::Lower => " lowerbound",
    };
    match result.checkmate_in() {
        Some(mate_in) => format!(
            "info depth {} seldepth {} nodes {} time {} nps {} score mate {}{} pv {}",
            depth, result.selective_depth, result.nodes, millis, nps, mate_in, qualifier, pv
        ),
        None => format!(
            "info depth {} seldepth {} nodes {} time {} nps {} score cp {}{} pv {}",
            depth, result.selective_depth, result.nodes, millis, nps, result.score, qualifier, pv
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

    /// A table small enough to afford one per case, speaking into a buffer.
    fn uci() -> UCI<AlphaBeta, Vec<u8>> {
        UCI::with_output(
            AlphaBeta::with_table_bytes(Board::new(), 8 * 1024),
            Vec::new(),
        )
    }

    /// An engine that searches nothing and keeps what it was asked for, so a
    /// test can say what reached the engine rather than timing a real search
    /// and inferring.
    struct Recorder {
        asked: Option<SearchParameters>,
        color: Color,
        /// How many times the table has been asked to empty.
        cleared: usize,
    }

    impl Recorder {
        fn to_move(color: Color) -> Self {
            Self {
                asked: None,
                color,
                cleared: 0,
            }
        }
    }

    impl Engine for Recorder {
        fn iterative_deepening_search(
            &mut self,
            search_options: SearchParameters,
            _on_depth: impl FnMut(u8, &SearchResult, PvLine, ScoreBound),
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
        fn clear_table(&mut self) {
            self.cleared += 1;
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
        // every line must be one an interface can pass over
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
        // the old loop asked a closed stdin for another line for ever, so
        // reaching the end of this call is the assertion
        let mut uci = uci();
        uci.run(Cursor::new("uci\nisready\nposition startpos\ngo depth 1\n"));
    }

    #[test]
    fn the_handshake_identifies_the_engine_and_ends_with_uciok() {
        let mut uci = uci();
        uci.handle("uci");
        let said = said(&uci);
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines.len(), 7);
        assert!(lines[0].starts_with("id name arche "));
        assert!(lines[1].starts_with("id author "));
        // the default is the engine's own
        assert_eq!(
            lines[2],
            "option name Hash type spin default 256 min 1 max 16384"
        );
        assert_eq!(lines[3], "option name Clear Hash type button");
        assert_eq!(
            lines[4],
            "option name Threads type spin default 1 min 1 max 1"
        );
        assert_eq!(
            lines[5],
            "option name Move Overhead type spin default 50 min 0 max 5000"
        );
        assert_eq!(lines[6], "uciok");
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

    /// A claimable fifty move draw is not "no legal moves". The engine used
    /// to report game over from a root whose counter had reached a hundred,
    /// so this printed `bestmove 0000` at a position with thirty moves, which
    /// a GUI that asks rather than adjudicating scores as a forfeit.
    #[test]
    fn a_claimable_fifty_move_draw_is_answered_with_a_move() {
        const FEN: &str = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 100 112";
        let mut uci = uci();
        uci.run(Cursor::new(format!("position fen {}\ngo depth 4\n", FEN)));
        let said = said(&uci);
        assert!(
            !said.contains("no legal moves identified"),
            "the position has legal moves: {}",
            said
        );
        assert!(!said.ends_with("bestmove 0000\n"), "{}", said);
        let last = said.lines().last().unwrap_or_default();
        assert!(last.starts_with("bestmove "), "{}", said);
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
    fn a_position_word_must_be_whole() {
        // startposx used to set the starting position, the x read as nothing
        for line in [
            "position startposx",
            "position startposition moves e2e4",
            "position startposmoves e2e4",
            "position fenx 8/8/8/8/8/8/8/8 w - - 0 1",
        ] {
            let mut uci = uci();
            let answer = uci.parse_position(line);
            assert!(answer.is_err(), "{} was accepted", line);
            assert!(
                answer.unwrap_err().starts_with("unrecognised position"),
                "{}",
                line
            );
        }
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

    /// The table a request for `megabytes` builds: the whole entries that fit,
    /// not the megabytes themselves.
    fn megabytes(megabytes: usize) -> usize {
        AlphaBeta::with_table_bytes(Board::new(), megabytes * 1024 * 1024).table_bytes()
    }

    #[test]
    fn a_hash_size_is_taken_as_sent_or_clamped_up_to_the_smallest_offered() {
        // no uci first: an option may be set before the handshake
        for (value, said_back) in [
            ("1", ""),
            ("0", "info string Hash 0 is outside 1 to 16384, using 1\n"),
        ] {
            let mut uci = uci();
            assert!(uci.handle(&format!("setoption name Hash value {}", value)));
            assert_eq!(uci.engine.table_bytes(), megabytes(1), "{}", value);
            assert_eq!(said(&uci), said_back, "{}", value);
        }
    }

    #[test]
    fn a_hash_value_that_cannot_be_read_leaves_the_table_alone() {
        for line in [
            "setoption name Hash value",
            "setoption name Hash value many",
            // below zero is outside the range advertised rather than a spent
            // clock, so it is refused instead of being lifted to the floor
            "setoption name Hash value -5",
        ] {
            let mut uci = uci();
            // resized first, so what is kept is the size in force rather than
            // the one the engine was built with
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
    fn the_table_can_be_resized_between_two_searches_and_after_a_new_game() {
        // the moments the protocol allows one
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
            said.contains("info string Threads 4 is outside 1 to 1, using 1"),
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
    fn the_move_overhead_the_option_sets_is_held_back_from_the_budget() {
        // through a whole session: what a setoption sets is what the go after
        // it holds back
        for (overhead, budget) in [(0, 500), (50, 450), (200, 300)] {
            let asked = asked_of_engine(&format!(
                "setoption name Move Overhead value {}\ngo movetime 500",
                overhead
            ));
            assert_eq!(
                asked.limits.clock(),
                Some(Clock::Fixed(Duration::from_millis(budget))),
                "overhead {}",
                overhead
            );
        }
    }

    #[test]
    fn a_move_overhead_inside_the_range_is_taken_in_silence() {
        let mut uci = uci();
        assert!(uci.handle("setoption name Move Overhead value 200"));
        assert_eq!(uci.move_overhead, 200);
        assert_eq!(said(&uci), "");
    }

    #[test]
    fn a_move_overhead_outside_the_range_offered_is_clamped_and_said_back() {
        let mut uci = uci();
        assert!(uci.handle("setoption name Move Overhead value 99999"));
        assert_eq!(
            said(&uci),
            "info string Move Overhead 99999 is outside 0 to 5000, using 5000\n"
        );
        assert_eq!(uci.move_overhead, 5000);
    }

    #[test]
    fn a_move_overhead_that_cannot_be_read_leaves_the_one_in_force() {
        for line in [
            "setoption name Move Overhead value",
            "setoption name Move Overhead value soon",
            // below zero is outside the range advertised rather than a spent
            // clock, so it is refused instead of being taken as no overhead
            "setoption name Move Overhead value -100",
        ] {
            let mut uci = uci();
            // set first, so what is kept is the overhead in force rather than
            // the default
            uci.handle("setoption name Move Overhead value 200");
            assert!(uci.handle(line));
            assert_eq!(uci.move_overhead, 200, "{}", line);
            assert!(
                said(&uci).starts_with("info string "),
                "{}: {}",
                line,
                said(&uci)
            );
        }
    }

    #[test]
    fn an_option_we_do_not_have_is_reported_by_name() {
        let mut uci = uci();
        assert!(uci.handle("setoption name Nonsense value 1"));
        assert_eq!(said(&uci), "info string unrecognised option: Nonsense\n");
    }

    #[test]
    fn every_spin_row_is_read_and_held_to_its_own_range() {
        // asked of the reader rather than of a session, which is what makes
        // the top of the Hash range assertable: a session would allocate the
        // sixteen gigabytes it names
        let spins = OPTIONS
            .iter()
            .filter(|option| matches!(option.kind, OptionKind::Spin { .. }))
            .count();
        let mut rows = 0;
        for option in OPTIONS {
            let OptionKind::Spin { min, max, .. } = option.kind else {
                continue;
            };
            rows += 1;
            let name = option.name;
            let read = |word: &str| {
                let line = format!("setoption name {} value {}", name, word);
                read_spin(name, min..=max, &Params::of(&line))
            };
            // what the reader returns for a word it had to hold to `end`
            let held = |word: &str, end: u64| {
                Ok(Held {
                    value: end,
                    said: Some(format!(
                        "info string {} {} is outside {} to {}, using {}",
                        name, word, min, max, end
                    )),
                })
            };

            for end in [min, max] {
                assert_eq!(
                    read(&end.to_string()),
                    Ok(Held {
                        value: end,
                        said: None
                    }),
                    "{} {}",
                    name,
                    end
                );
            }

            // one past each end, where there is a past: a u64 cannot fall
            // below a minimum of zero, and a row whose top is the largest
            // u64 has nothing above it
            let mut past = Vec::new();
            if let Some(below) = min.checked_sub(1) {
                past.push((below.to_string(), min));
            }
            if let Some(above) = max.checked_add(1) {
                past.push((above.to_string(), max));
                // the same value spelled with a leading zero, so the
                // sentence is held to saying the word back rather than the
                // number it read
                past.push((format!("0{}", above), max));
                // too large to hold is as unreadable as a word to the parse,
                // where a count read it as everything there is
                past.push((u64::MAX.to_string(), max));
            }
            for (word, end) in past {
                assert_eq!(read(&word), held(&word, end), "{} {}", name, word);
            }

            // a negative is no more readable than a word: it is outside every
            // range the handshake advertises
            for word in ["many", "-1", "18446744073709551616"] {
                assert_eq!(
                    read(word),
                    Err(format!("unrecognised {} value: {}", name, word)),
                    "{} {}",
                    name,
                    word
                );
            }
            assert_eq!(
                read_spin(name, min..=max, &Params::of("setoption name x value")),
                Err(format!("{} was sent without a value", name)),
                "{}",
                name
            );
        }
        // the loop passes over a row it does not recognise in silence, so
        // what it read is counted against what the table holds
        assert_eq!(rows, spins, "a spin row went unread");
        assert!(rows > 0, "no spin row was read at all");
    }

    #[test]
    fn the_clear_hash_button_reaches_the_engine_in_silence() {
        // several interfaces send a value with a button, and are not
        // complained at
        for line in [
            "setoption name Clear Hash",
            "setoption name Clear Hash value",
        ] {
            let mut uci = UCI::with_output(Recorder::to_move(Color::White), Vec::new());
            uci.run(Cursor::new(format!("{}\n", line)));
            assert_eq!(uci.engine.cleared, 1, "{}", line);
            assert_eq!(
                String::from_utf8(uci.out.clone()).unwrap(),
                "",
                "{} was answered",
                line
            );
        }
    }

    #[test]
    fn an_option_named_with_two_words_is_read_as_both_of_them() {
        // the name used to be the word after `name`, which would have read
        // `Clear Hash` as `Clear`
        let mut uci = uci();
        assert!(uci.handle("setoption name Some Other value 1"));
        assert_eq!(said(&uci), "info string unrecognised option: Some Other\n");
    }

    #[test]
    fn a_setoption_without_a_name_is_reported_rather_than_fatal() {
        let mut uci = uci();
        assert!(uci.handle("setoption"));
        assert!(said(&uci).starts_with("info string setoption without an option name"));
    }

    #[test]
    fn a_new_game_keeps_the_move_overhead_it_was_given() {
        // it describes the connection rather than the position
        let mut uci = uci();
        uci.handle("setoption name Move Overhead value 200");
        assert!(uci.handle("ucinewgame"));
        assert_eq!(uci.move_overhead, 200);
    }

    #[test]
    fn a_new_game_keeps_the_table_size_it_was_given() {
        let mut uci = uci();
        uci.handle("setoption name Hash value 1");
        assert!(uci.handle("ucinewgame"));
        assert_eq!(uci.engine.table_bytes(), megabytes(1));
    }

    /// The clock, the increment, the count of moves, the move time and the
    /// infinite flag, as a tuple so a case fits on one line.
    type Clocks = (Option<u64>, Option<u64>, Option<u64>, Option<u64>, bool);

    /// What a line's clock words are read as for a colour.
    fn clock_words(line: &str, color: Color) -> Clocks {
        let control = TimeControl::of(&Params::of(line), color);
        (
            control.time,
            control.increment,
            control.moves_to_go,
            control.move_time,
            control.infinite,
        )
    }

    #[test]
    fn what_the_clock_words_on_a_go_line_are_read_as() {
        // every field for each case, so what a line does not say is asserted
        // as well as what it does
        const BOTH: &str = "go wtime 111 btime 222 winc 333 binc 444 movestogo 5";
        const NOTHING: Clocks = (None, None, None, None, false);
        for (line, color, want) in [
            // each colour reads its own clock and increment; the count of
            // moves belongs to both
            (
                BOTH,
                Color::White,
                (Some(111), Some(333), Some(5), None, false),
            ),
            (
                BOTH,
                Color::Black,
                (Some(222), Some(444), Some(5), None, false),
            ),
            // and a clock missing for our colour is not taken from the other
            ("go btime 222 binc 444", Color::White, NOTHING),
            (
                "go movetime 500",
                Color::White,
                (None, None, None, Some(500), false),
            ),
            ("go infinite", Color::White, (None, None, None, None, true)),
            (
                "go wtime 1000",
                Color::White,
                (Some(1000), None, None, None, false),
            ),
            // a clock too large to hold must not turn into an unlimited search
            (
                "go wtime 99999999999999999999999",
                Color::White,
                (Some(u64::MAX), None, None, None, false),
            ),
            // cutechess and fastchess send a clock below zero once their time
            // margin has been eaten into. Not reading it would leave the
            // search unbounded at the moment there is least time to spare
            (
                "go wtime -5 btime -5",
                Color::White,
                (Some(0), None, None, None, false),
            ),
            (
                "go wtime -5 btime -5 winc -1 binc -1",
                Color::Black,
                (Some(0), Some(0), None, None, false),
            ),
            // the regexes this replaced had no word boundary
            ("go xwtime 300000", Color::White, NOTHING),
        ] {
            assert_eq!(clock_words(line, color), want, "{} as {:?}", line, color);
        }
    }

    #[test]
    fn a_clock_that_cannot_be_read_is_a_spent_one_rather_than_no_clock() {
        // discarding it would read as the keyword being absent, and a go with
        // no time at all searches without a limit
        let control = TimeControl::of(&Params::of("go wtime abc winc x"), Color::White);
        assert_eq!(control.time, Some(0));
        assert_eq!(control.increment, Some(0));
        assert!(
            control.budget(DEFAULT_MOVE_OVERHEAD_MS).is_some(),
            "an unreadable clock must still bound the search"
        );
    }

    #[test]
    fn a_depth_is_read_as_far_as_the_ply_rail_and_no_further() {
        // an unreadable depth is dropped rather than read as zero, which
        // would come back without a move
        for (line, depth) in [
            ("go depth 5", Some(5)),
            ("go depth 999", Some(arche_core::MAX_PLY)),
            ("go depth abc", None),
            ("go infinite", None),
        ] {
            assert_eq!(
                Go::of(&Params::of(line), Color::White, DEFAULT_MOVE_OVERHEAD_MS).depth,
                depth,
                "{}",
                line
            );
        }
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
    fn what_a_go_line_asks_of_the_search() {
        // a move time arrives less the overhead, and arrives fixed, so the
        // deepening loop spends it rather than answering early
        let fixed = |millis| Some(Clock::Fixed(Duration::from_millis(millis)));
        for (line, depth, clock, nodes) in [
            ("go movetime 500", None, fixed(450), u64::MAX),
            ("go infinite wtime 60000", None, None, u64::MAX),
            ("go nodes 5000", None, None, 5000),
            ("go depth 3", Some(3), None, u64::MAX),
            ("go", None, None, u64::MAX),
            ("go movetime 500 nodes 5000", None, fixed(450), 5000),
            // a limit read as zero would stop the search before it had a move
            ("go nodes abc", None, None, u64::MAX),
            // the root deepens by one more in check, so the largest depth a
            // byte holds used to overflow it; the rail leaves room
            ("go depth 255", Some(arche_core::MAX_PLY), None, u64::MAX),
        ] {
            let asked = asked_of_engine(line);
            assert_eq!(asked.depth, depth, "{}: depth", line);
            assert_eq!(asked.limits.clock(), clock, "{}: clock", line);
            assert_eq!(asked.limits.node_budget(), nodes, "{}: nodes", line);
        }
    }

    #[test]
    fn a_search_is_given_the_clock_of_the_side_to_move() {
        // the clocks are far apart, so a search handed the wrong one is
        // handed twenty times the time it has
        let line = "go wtime 60000 btime 4000";
        assert_eq!(
            asked_of_engine_as(line, Color::White).limits.clock(),
            Some(Clock::Share(Duration::from_millis(2950))),
            "white was not given its own clock"
        );
        assert_eq!(
            asked_of_engine_as(line, Color::Black).limits.clock(),
            Some(Clock::Share(Duration::from_millis(150))),
            "black was not given its own clock"
        );
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
    fn an_unreadable_bench_setting_is_reported_rather_than_searched() {
        // each is refused by name; running the default in its place would
        // take seconds and say nothing about why
        let mut uci = uci();
        uci.run(Cursor::new(
            "bench abc\nbench 300\nbench 1 hash 0\nbench 1 hash 99999\n\
             bench 1 hash big\nbench 1 taint maybe\n",
        ));
        assert_eq!(
            said(&uci),
            "info string unrecognised bench depth: abc\n\
             info string unrecognised bench depth: 300\n\
             info string unrecognised bench hash: 0\n\
             info string unrecognised bench hash: 99999\n\
             info string unrecognised bench hash: big\n\
             info string unrecognised bench taint: maybe\n"
        );
    }

    #[test]
    fn a_bench_command_takes_a_table_size_and_a_taint_policy() {
        // the words may come in either order, the depth may be left out, and
        // a keyword with nothing after it is the setting left out. The
        // left-out depth is checked on the settings alone, since running the
        // suite at the bench's own depth would cost seconds
        let mut uci = uci();
        uci.run(Cursor::new(
            "bench 1 hash 1 taint trust\nbench 1 taint refuse hash 2\nbench 1 taint\n",
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
                "bench depth 1 hash 16MB",
                "taint rule50",
            ],
            "{}",
            said
        );

        let left_out = bench_settings(&Params::of("bench hash 1 taint trust"))
            .expect("a left-out depth is the bench's own");
        assert_eq!(left_out.depth, bench::DEPTH);
        assert_eq!(left_out.table_bytes, 1024 * 1024);
        assert_eq!(left_out.config.taint_word(), "trust");
    }

    #[test]
    fn a_bench_command_takes_the_signature_audit() {
        // a word, so it may stand where the depth would be; a run without it
        // says nothing about signatures
        let mut uci = uci();
        uci.run(Cursor::new("bench 1 hash 1 audit\nbench 1 hash 1\n"));
        let said = said(&uci);
        assert_eq!(
            said.matches("signature audit: probes ").count(),
            1,
            "{}",
            said
        );
        let audited = bench_settings(&Params::of("bench audit")).expect("audit is a word");
        assert_eq!(audited.depth, bench::DEPTH);
        assert!(audited.audit);
        let plain = bench_settings(&Params::of("bench 1")).expect("a depth");
        assert!(!plain.audit);
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

    /// The move of this name in the starting position.
    fn play_named(name: &str) -> Play {
        *Board::new()
            .generate_moves()
            .iter()
            .find(|m| format!("{}", m) == name)
            .unwrap_or_else(|| panic!("{} is not a move here", name))
    }

    #[test]
    fn a_report_is_said_as_an_info_line() {
        // the best move is not part of the line, so it stands still
        let result = |nodes, millis, selective_depth, score| SearchResult {
            nodes,
            elapsed: Duration::from_millis(millis),
            selective_depth,
            best_move: play_named("e2e4"),
            score,
        };
        for (depth, result, line, bound, expected) in [
            (
                5,
                result(2000, 500, 7, 25),
                vec!["e2e4", "g1f3"],
                ScoreBound::Exact,
                "info depth 5 seldepth 7 nodes 2000 time 500 nps 4000 score cp 25 pv e2e4 g1f3",
            ),
            // three plies from checkmate reads as mate in two moves
            (
                4,
                result(1500, 20, 4, 30_000 - 3),
                vec!["e2e4"],
                ScoreBound::Exact,
                "info depth 4 seldepth 4 nodes 1500 time 20 nps 75000 score mate 2 pv e2e4",
            ),
            // a search faster than a millisecond is measured as one
            (
                1,
                result(300, 0, 1, 0),
                vec![],
                ScoreBound::Exact,
                "info depth 1 seldepth 1 nodes 300 time 0 nps 300000 score cp 0 pv ",
            ),
            // the qualifier goes after the score and before the line, where
            // the protocol has it
            (
                6,
                result(2000, 500, 7, 25),
                vec!["d2d4"],
                ScoreBound::Lower,
                "info depth 6 seldepth 7 nodes 2000 time 500 nps 4000 score cp 25 lowerbound pv d2d4",
            ),
        ] {
            let pv = PvLine::new(line.into_iter().map(play_named).collect());
            assert_eq!(
                format_info(depth, &result, &pv, bound),
                expected,
                "depth {}",
                depth
            );
        }
    }

    /// An engine whose search ends the way one the clock catches does: a
    /// depth completed and reported, then a better move from the aborted
    /// iteration, which the deepening loop swaps in. Scripted, because
    /// provoking a real swap means timing a search to the node.
    struct Swapper;

    impl Swapper {
        fn result(best_move: Play, score: arche_core::Score) -> SearchResult {
            SearchResult {
                nodes: 1000,
                elapsed: Duration::from_millis(100),
                selective_depth: 4,
                best_move,
                score,
            }
        }
    }

    impl Engine for Swapper {
        fn iterative_deepening_search(
            &mut self,
            _search_options: SearchParameters,
            mut on_depth: impl FnMut(u8, &SearchResult, PvLine, ScoreBound),
        ) -> SearchOutcome {
            let completed = Self::result(play_named("e2e4"), 20);
            on_depth(
                3,
                &completed,
                PvLine::new(vec![play_named("e2e4")]),
                ScoreBound::Exact,
            );
            let swapped = Self::result(play_named("d2d4"), 35);
            on_depth(
                4,
                &swapped,
                PvLine::new(vec![play_named("d2d4")]),
                ScoreBound::Lower,
            );
            SearchOutcome::Aborted(Some(swapped))
        }

        fn active_color(&self) -> Color {
            Color::White
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
        fn clear_table(&mut self) {}
        fn board_display(&self) -> String {
            String::new()
        }
        fn perft(&mut self, _depth: u8) -> u64 {
            0
        }
    }

    #[test]
    fn the_move_a_swap_answers_with_opens_the_last_line_said() {
        // fastchess warns on a bestmove the last pv does not open with
        let mut uci = UCI::with_output(Swapper, Vec::new());
        uci.run(Cursor::new("position startpos\ngo movetime 100\n"));
        let said = String::from_utf8(uci.out.clone()).unwrap();
        let lines: Vec<&str> = said.lines().collect();
        let best = lines
            .last()
            .and_then(|line| line.strip_prefix("bestmove "))
            .unwrap_or_else(|| panic!("no bestmove in {}", said));
        let last_info = lines
            .iter()
            .rfind(|line| line.starts_with("info depth "))
            .unwrap_or_else(|| panic!("no info line in {}", said));
        let first_of_the_line = last_info
            .split(" pv ")
            .nth(1)
            .and_then(|line| line.split_whitespace().next())
            .unwrap_or_else(|| panic!("no line in {}", last_info));
        assert_eq!(first_of_the_line, best, "{}", said);
        assert!(last_info.contains(" lowerbound "), "{}", last_info);
    }

    // ---- generated sessions -------------------------------------------

    /// Every line the protocol lets an engine say. Anything else is the
    /// engine muttering where an interface can hear it.
    const SPOKEN: [&str; 6] = ["info", "bestmove", "id", "option", "uciok", "readyok"];

    /// The keywords a generated line opens with: the ones the loop dispatches
    /// on, and near misses that fall through to the unrecognised branch.
    ///
    /// `bench` is absent. It runs a real bench of several million nodes
    /// whoever is behind the loop, and prints a table rather than protocol,
    /// so a session containing one cannot be asked the question below.
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

    /// A word a command line might carry: a protocol keyword, a number of the
    /// shapes an interface sends, something shaped like a move, and junk.
    fn word() -> impl Strategy<Value = String> {
        prop_oneof![
            prop::sample::select(vec![
                "name",
                "value",
                "Hash",
                // the words of the option names, so a generated setoption
                // can reach them
                "Clear",
                "Move",
                "Overhead",
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

    /// How many of these lines the loop will dispatch as a `go`, read the
    /// way the dispatcher reads a line.
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

        /// Exactly one bestmove for every go: none and the game hangs on our
        /// clock, two and the second is read as the answer to the next go, a
        /// move played in a position it was not chosen for.
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
        // a real search behind the loop, with few cases because every one
        // of them searches
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn a_real_engine_keeps_the_same_promises(lines in prop::collection::vec(line(), 0..5usize)) {
            // a generated perft depth like 300000 would not finish, and a go
            // with no clock searches to the depth cap, so perft is dropped
            // and every go is given a move time. It goes straight after the
            // keyword because the reader takes the first of a repeated word;
            // infinite would beat it whatever it said, so it comes out
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
    /// one: lines typed in, and what was said read back while the loop runs.
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
            // the production wiring; the only substitution is the input
            let session = thread::spawn(move || {
                UCI::with_output(engine, out).wire(move || script.into_iter().map(Ok));
            });
            Self {
                typed,
                said,
                session,
            }
        }

        /// A real search behind the loop.
        fn searching() -> Self {
            Self::of(AlphaBeta::with_table_bytes(Board::new(), 8 * 1024))
        }

        /// A session whose engine answers at once, so a held answer can be
        /// tested without waiting for a search to run out of depths.
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
        /// was said instead. The deadline is generous because it bounds a
        /// real search on whatever machine runs the suite.
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

        /// Nothing more is said for this long, which is the only way to show
        /// an answer is held back.
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
        // twenty seconds of move time, stopped inside the first second: the
        // search comes back at once with the move it had
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
        // it used to come back as an unrecognised command
        let driven = Driven::instant();
        driven.type_line("stop");
        driven.type_line("isready");
        driven.wait_for("readyok");
        assert_eq!(driven.finish(), "readyok\n");
    }

    #[test]
    fn an_isready_is_answered_while_a_search_runs() {
        // the protocol requires this one answered at once, whatever the
        // engine is in the middle of
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
    fn a_go_nothing_bounds_holds_its_move_until_a_stop_arrives() {
        // the engine answers at once, so the deepening is over long before
        // the stop; the bestmove still waits for it
        for line in ["go infinite", "go"] {
            let driven = Driven::instant();
            driven.type_line(line);
            assert_eq!(
                driven.stays_quiet_for(Duration::from_millis(50)),
                "",
                "{} answered without being stopped",
                line
            );
            driven.type_line("stop");
            driven.wait_for("bestmove");
            driven.finish();
        }
    }

    #[test]
    fn the_interface_leaving_ends_a_held_answer() {
        // a pipe that closes without a quit is an interface that has gone;
        // the hold used to park on a stop that could no longer come
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
        let driven = Driven::instant();
        driven.type_line("go depth 3");
        driven.wait_for("bestmove");
        driven.finish();
    }

    #[test]
    fn a_position_sent_during_a_search_is_applied_after_it() {
        // the second go proves which position the engine ended up on: from
        // that one there is no move at all
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
        // every go gets a bestmove, a quit included
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
        let holds = |line: &str| {
            Go::of(&Params::of(line), Color::White, DEFAULT_MOVE_OVERHEAD_MS).holds_its_answer()
        };
        assert!(holds("go infinite"));
        assert!(holds("go"));
        // infinite outranks anything sent beside it, as the protocol says
        assert!(holds("go infinite depth 2"));
        assert!(!holds("go depth 2"));
        assert!(!holds("go nodes 5000"));
        // too large to hold reads as no budget
        assert!(holds("go nodes 99999999999999999999999"));
        assert!(!holds("go movetime 500"));
        assert!(!holds("go wtime 1000"));
        // the overhead shrinks a budget and never takes one away
        let held_back = |line: &str| {
            Go::of(&Params::of(line), Color::White, OVERHEAD_MAX_MS).holds_its_answer()
        };
        assert!(!held_back("go movetime 500"));
        assert!(!held_back("go wtime 1000"));
        assert!(held_back("go infinite"));
    }
}
