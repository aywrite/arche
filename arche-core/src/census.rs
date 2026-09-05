// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The cutoff census: which move cuts a full width node off, and what
//! company it cut ahead of.
//!
//! The move ordering is read for what it earns, and what it earns is decided
//! at cutoffs, but a cutoff also censors: every move ordered after the one
//! that cut is never searched, so a raw history count says how often a move
//! cut among the moves it was allowed to be tried on, not how good it is.
//! The census is the data that censoring is quantified from. One event per
//! sampled node of the default search, taken at the moment the node answers,
//! from the two places a node returns out of `alpha_beta`'s move loop: the
//! cutoff, and the loop's natural end. Nothing is recorded from inside the
//! loop, which is the same "return out of the node" placement the residual
//! sampler documents. Quiescence and the root are out of scope: quiescence
//! cuts on capture order and the history questions do not apply, and the
//! root searches every move.
//!
//! An event is a portrait and not a comparison. There is no claim, no
//! reference and no replay: a row says what the node knew and what it did,
//! and the reading is the contrast between the nodes that cut and the nodes
//! that did not. Recording the held nodes at the same rate is what keeps
//! that contrast honest; a cut-only stream reproduces the very censoring
//! the census exists to measure.
//!
//! The recorder hangs off an engine the way the residual sampler does, and
//! an engine without one searches exactly the tree it searched before there
//! was a census at all, which is what the pinned bench counts say.

use crate::bench::{self, Position};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchParameters};
use crate::play::Play;
use crate::residual::{DEPTH_SPREAD, Sampler, Window};
use std::fmt;

/// What the census contributes to a sampling key: an arbitrary constant far
/// in its bits from the three the residual sampler holds (`Shortcut::salt`),
/// under the same rule. The draws are not joint: the salts differ high in
/// the word (bit 62 against the null move's, bit 63 against the other two),
/// so at any rate coarser than one in four a node kept here is not one the
/// residual kinds keep.
const SALT: u64 = 0xc5b9_128e_66d0_3a47;

/// The key a node's answer is sampled by: the position, the depth, and
/// nothing about the run, exactly as `residual::sample_key` builds one, so
/// two runs of the same search record the same nodes and a change that
/// reorders the tree without changing what is in it samples the same nodes.
pub fn sample_key(position_key: u64, depth: u8) -> u64 {
    position_key ^ SALT ^ u64::from(depth).wrapping_mul(DEPTH_SPREAD)
}

/// About one record in every this many events, unless the command says
/// otherwise. Every full width node past its table probe and its shortcuts
/// offers an event, so the stream is denser than the residual sampler's;
/// the same rate still fills a comfortable share of the default cap at the
/// bench's depth.
pub const DEFAULT_EVERY: u32 = 1_000;

/// What kind of move cut a node off, in the ordering's own precedence:
/// material first, so a capture that is also a killer is a capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The table's move, searched before anything was generated.
    Table,
    /// A move with a victim, wherever the swap priced it.
    Capture,
    /// A promotion with no victim.
    Promotion,
    /// A quiet move standing in one of the node's killer slots.
    Killer,
    /// Any other quiet move.
    Quiet,
}

impl Class {
    /// The classes, in the order the summary reports them.
    pub const KINDS: [Class; 5] = [
        Class::Table,
        Class::Capture,
        Class::Promotion,
        Class::Killer,
        Class::Quiet,
    ];

    /// The word a row prints.
    pub fn word(self) -> &'static str {
        match self {
            Class::Table => "table",
            Class::Capture => "capture",
            Class::Promotion => "promotion",
            Class::Killer => "killer",
            Class::Quiet => "quiet",
        }
    }

    /// The class of a cutting move, from the move and the killer slots as
    /// they stood at the cutoff. `table` is the caller's to say, since it
    /// is a fact about where the move came from and not about the move.
    pub fn of(m: &Play, table: bool, killers: [Option<Play>; 2]) -> Class {
        if table {
            return Class::Table;
        }
        if m.capture.is_some() {
            return Class::Capture;
        }
        if m.promote.is_some() {
            return Class::Promotion;
        }
        if killers.contains(&Some(*m)) {
            return Class::Killer;
        }
        Class::Quiet
    }
}

/// What the node's table probe had given it by the time it answered.
///
/// A probe that cut answered the node before the move loop, so no event
/// carries it; what is left is a miss, a move the node could put first, or
/// an entry whose move failed the pseudo-legality check and ordered
/// nothing. Read from what the node already learned, never probed again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    Miss,
    /// The probe returned a move, and the node put it first. A signature
    /// collision can hand over a move the position cannot play; such a row
    /// is `ScoreOnly`, so this word means the move was really used.
    Move,
    /// The probe hit, and its move was not one this position could play.
    ScoreOnly,
}

impl Table {
    /// The word a row prints.
    pub fn word(self) -> &'static str {
        match self {
            Table::Miss => "miss",
            Table::Move => "move",
            Table::ScoreOnly => "score_only",
        }
    }

    /// What the node learned: whether the probe hit, and whether the move
    /// it handed back was usable here.
    pub fn of(hit: bool, usable: bool) -> Table {
        match (hit, usable) {
            (false, _) => Table::Miss,
            (true, true) => Table::Move,
            (true, false) => Table::ScoreOnly,
        }
    }
}

/// What the engine hands the recorder about a cutting move: the move,
/// whether its answer came through the reduced scout, and whether it was
/// the table's move searched before generation, which is where the class
/// starts and a fact about where the move came from rather than the move.
#[derive(Clone, Copy, Debug)]
pub struct Cutting<'a> {
    pub play: &'a Play,
    pub reduced: bool,
    pub table: bool,
}

/// The cutting move's half of an event: everything that is only there when
/// the node cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    /// The cutting move's place among the searched moves, so 0 is the
    /// table's move when it was searched first. Always the last of them,
    /// which is what a cutoff is; recorded beside `searched` all the same,
    /// so a row is read without re-deriving it.
    pub index: usize,
    pub class: Class,
    /// The history table's score for the cutting move at the moment of the
    /// cutoff, quiets only and 0 otherwise. Read as a fraction of
    /// `history_max` rather than raw, since the raw number ages.
    pub history: u32,
    /// Whether the cutting move's answer came through the reduced scout.
    pub reduced: bool,
}

/// One node of the move loop answering.
///
/// Everything is owned, as a residual sample is: an event outlives the
/// search that took it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The position, as the board prints one, last on the row.
    pub fen: String,
    /// The depth the node searched with, the check extension included.
    pub depth: u8,
    /// The window the node was entered with, read from its bounds the way
    /// the residual sampler reads them.
    pub window: Window,
    pub in_check: bool,
    /// The moves the node generated. Legal-move counts are unknowable
    /// without making them, so the row holds what the list holds; a node
    /// its table move cut before generating holds none, and the row says 0.
    pub generated: usize,
    /// The moves actually made and searched, the loop's own counter, the
    /// cutting move among them. What the cutoff censored is
    /// `generated - searched` less the illegal ones, which is the
    /// analysis's subtraction to make and not the row's.
    pub searched: usize,
    /// The cutting move's half, or none when the loop ran out.
    pub cut: Option<Cut>,
    /// The largest history score among the node's generated quiets, the
    /// denominator `Cut::history` is read against. 0 when nothing was
    /// generated.
    pub history_max: u32,
    /// Whether the staged ordering ever scored the quiet band here, or the
    /// front answered before `order_quiets` ran: the class-staged scoring
    /// win made visible. A list too long for the stack is sorted whole,
    /// memories included, and still reads unscored; such a node is rare
    /// and shows itself by its generated count.
    pub quiets_scored: bool,
    pub tt: Table,
    /// The static evaluation less beta. Computed for kept events alone and
    /// exact, not a cache read: the census may call the eval cold for a
    /// sampled node, since it is off the measured path, where forcing an
    /// eval at every node to fill a column would change the engine.
    pub eval_beta: i32,
    /// Nodes spent under this node: the node counter at its answer less
    /// the counter at its entry. The eventual cost of however it answered.
    pub cost: u64,
}

/// A whole run: what it was asked for and what it recorded, a row an event.
#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    /// The most events the run would keep. Stated in the header only when
    /// it is not the default, the way the residuals header states its own.
    pub cap: usize,
    /// Positions of the suite the run searched.
    pub positions: usize,
    /// Every node offered, kept or not: the denominator the rows are read
    /// against.
    pub events: u64,
    /// Events the buffer had no room for.
    pub overflowed: u64,
    pub rows: Vec<Event>,
}

/// Search the suite with the census armed. Recording is the whole of it:
/// there is no replay, because an event is a portrait of a decision and not
/// a claim a reference could check.
///
/// One sampler for the whole suite, carried from each position's engine to
/// the next, so the cap describes the run and not each position of it; the
/// configuration is the default, since the ordering under census is the
/// ordering the engine plays with.
pub fn run(positions: &[Position], depth: u8, every: u32, cap: usize) -> Report {
    let depth = depth.max(1);
    // the rate the sampler will really keep to, so the header states the
    // run that happened
    let every = every.max(1);
    let mut sampler = Sampler::with_cap(every, cap);
    for position in positions {
        let board = Board::from_fen(&position.fen)
            .unwrap_or_else(|e| panic!("census position {} does not parse: {}", position.id, e));
        let mut engine = AlphaBeta::with_config(board, bench::TABLE_BYTES, SearchConfig::default());
        engine.sample_cutoffs(sampler);
        engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
        sampler = engine
            .take_census()
            .expect("the sampler just handed to the engine comes back");
    }
    let sampled = sampler.drain();
    Report {
        depth,
        every,
        cap,
        positions: positions.len(),
        events: sampled.events,
        overflowed: sampled.overflowed,
        rows: sampled.taken,
    }
}

/// The events of one depth, counted the way the summary line prints them.
///
/// By depth and not pooled over the depths, for the residual summary's
/// reason: the depths are reached in wildly different numbers, and a pooled
/// rate is the shallowest depth's rate wearing every depth's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub depth: u8,
    /// Every row at this depth, which the cut rate is a share of.
    pub records: usize,
    pub cuts: usize,
    /// Cuts at index 0, at indices 1 to 3, and at 4 and past: how deep
    /// into the searched moves the ordering had to go.
    pub at_first: usize,
    pub early: usize,
    pub late: usize,
    /// Moves searched over the cut rows and over the held rows, summed;
    /// the line prints each as a mean over its own rows.
    pub searched_at_cuts: usize,
    pub searched_at_held: usize,
    /// Cuts by the cutting move's class, in `Class::KINDS` order.
    pub classes: [usize; 5],
    /// Cut rows whose quiet band was never scored.
    pub unscored_cuts: usize,
}

impl Report {
    /// One depth's summary, or none when the run kept no row of it.
    pub fn summary(&self, depth: u8) -> Option<Summary> {
        let rows: Vec<&Event> = self.rows.iter().filter(|row| row.depth == depth).collect();
        if rows.is_empty() {
            return None;
        }
        let mut counted = Summary {
            depth,
            records: rows.len(),
            cuts: 0,
            at_first: 0,
            early: 0,
            late: 0,
            searched_at_cuts: 0,
            searched_at_held: 0,
            classes: [0; 5],
            unscored_cuts: 0,
        };
        for row in rows {
            let Some(cut) = &row.cut else {
                counted.searched_at_held += row.searched;
                continue;
            };
            counted.cuts += 1;
            counted.searched_at_cuts += row.searched;
            match cut.index {
                0 => counted.at_first += 1,
                1..=3 => counted.early += 1,
                _ => counted.late += 1,
            }
            counted.classes[Class::KINDS
                .iter()
                .position(|class| *class == cut.class)
                .expect("every class is in KINDS")] += 1;
            counted.unscored_cuts += usize::from(!row.quiets_scored);
        }
        Some(counted)
    }

    /// Every summary the run has, shallowest depth first.
    pub fn summaries(&self) -> Vec<Summary> {
        let mut depths: Vec<u8> = self.rows.iter().map(|row| row.depth).collect();
        depths.sort_unstable();
        depths.dedup();
        depths
            .into_iter()
            .filter_map(|depth| self.summary(depth))
            .collect()
    }
}

/// A share as the summary prints one, or a `-` when nothing stands under
/// it: a figure with no denominator is not a zero.
fn share(part: usize, of: usize) -> String {
    if of == 0 {
        "-".to_string()
    } else {
        format!("{:.2}%", 100.0 * part as f64 / of as f64)
    }
}

/// A mean over some rows, under the same rule as `share`.
fn mean(total: usize, over: usize) -> String {
    if over == 0 {
        "-".to_string()
    } else {
        format!("{:.1}", total as f64 / over as f64)
    }
}

/// The report as the command prints it: a header naming what the run was
/// asked for and what it collected, a row an event, and a summary line a
/// depth.
///
/// A row is `depth window check outcome generated searched index class
/// history history_max scored tt eval_beta cost reduced fen`, whitespace
/// separated with the fen last, so it parses left to right and the field
/// that can hold spaces holds the rest of the line. The five fields a held
/// row has no value for print `-` rather than moving the columns.
///
/// The header states the events beside the records, always, for the
/// residuals header's reason: a distribution says nothing until the reader
/// knows how many chances there were to be in it.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cutoffs depth {} every {}", self.depth, self.every)?;
        if self.cap != crate::residual::DEFAULT_CAP {
            write!(f, " cap {}", self.cap)?;
        }
        write!(
            f,
            " positions {} events {} records {}",
            self.positions,
            self.events,
            self.rows.len(),
        )?;
        if self.overflowed > 0 {
            write!(f, " overflow {}", self.overflowed)?;
        }
        writeln!(f)?;
        for row in &self.rows {
            let held = "-".to_string();
            let (outcome, index, class, history, reduced) = match &row.cut {
                Some(cut) => (
                    "cut",
                    cut.index.to_string(),
                    cut.class.word().to_string(),
                    cut.history.to_string(),
                    if cut.reduced { "reduced" } else { "full" }.to_string(),
                ),
                None => ("held", held.clone(), held.clone(), held.clone(), held),
            };
            writeln!(
                f,
                "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
                row.depth,
                row.window.word(),
                if row.in_check { "check" } else { "calm" },
                outcome,
                row.generated,
                row.searched,
                index,
                class,
                history,
                row.history_max,
                if row.quiets_scored {
                    "scored"
                } else {
                    "unscored"
                },
                row.tt.word(),
                row.eval_beta,
                row.cost,
                reduced,
                row.fen,
            )?;
        }
        writeln!(f)?;
        writeln!(f, "summary")?;
        let summaries = self.summaries();
        // said rather than left out: a run that kept nothing is a fact
        if summaries.is_empty() {
            writeln!(f, "records 0")?;
        }
        for s in summaries {
            let held = s.records - s.cuts;
            write!(
                f,
                "depth {} records {} cuts {} rate {} index0 {} index1-3 {} index4+ {}",
                s.depth,
                s.records,
                s.cuts,
                share(s.cuts, s.records),
                share(s.at_first, s.cuts),
                share(s.early, s.cuts),
                share(s.late, s.cuts),
            )?;
            write!(
                f,
                " searched cut {} held {}",
                mean(s.searched_at_cuts, s.cuts),
                mean(s.searched_at_held, held),
            )?;
            for (class, count) in Class::KINDS.iter().zip(s.classes) {
                write!(f, " {} {}", class.word(), share(count, s.cuts))?;
            }
            writeln!(f, " unscored {}", share(s.unscored_cuts, s.cuts))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::Piece;
    use crate::residual::DEFAULT_CAP;

    /// The same two positions the residual tests record over: enough for a
    /// run to have something to keep, little enough for a test.
    fn suite() -> Vec<Position> {
        bench::parse_epd(
            "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - id \"sharp\";\n\
             r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - id \"kiwipete\";",
        )
    }

    fn quiet(from: u8, to: u8) -> Play {
        Play::new(from, to, None, None, false, false)
    }

    /// The key decides on the node alone, as the residual key does, and the
    /// census draws apart from the residual kinds: the same node keys
    /// differently under every one of them.
    #[test]
    fn a_key_is_the_node_and_nothing_about_the_run() {
        let position = 0x0123_4567_89ab_cdef;
        let key = sample_key(position, 4);
        assert_eq!(key, sample_key(position, 4));
        assert_ne!(key, sample_key(position, 5));
        assert_ne!(key, sample_key(position ^ 1, 4));
        for kind in crate::residual::Shortcut::KINDS {
            assert_ne!(key, crate::residual::sample_key(position, kind, 4));
        }
    }

    /// The ordering's own precedence, material first: the table's move
    /// outranks everything, a capture that is also a killer is a capture,
    /// a promotion that is also a killer is a promotion, and a quiet move
    /// no killer names is a quiet move.
    #[test]
    fn a_cutting_move_is_classed_material_first() {
        let m = quiet(8, 16);
        let killers = [Some(m), None];
        let take = Play::new(8, 16, Some(Piece::Pawn), None, false, false);
        let promote = Play::new(
            48,
            56,
            None,
            Some(crate::misc::PromotePiece::Queen),
            false,
            false,
        );
        assert_eq!(Class::of(&take, true, killers), Class::Table);
        assert_eq!(Class::of(&take, false, killers), Class::Capture);
        assert_eq!(
            Class::of(&promote, false, [Some(promote), None]),
            Class::Promotion
        );
        assert_eq!(Class::of(&m, false, killers), Class::Killer);
        assert_eq!(Class::of(&m, false, [None, Some(m)]), Class::Killer);
        assert_eq!(Class::of(&m, false, [None, None]), Class::Quiet);
        assert_eq!(
            Class::of(&m, false, [Some(quiet(9, 17)), None]),
            Class::Quiet
        );
    }

    /// The probe's three outcomes an event can carry, from what the node
    /// learned: no entry, a move it could use, and a hit whose move it
    /// could not.
    #[test]
    fn the_table_words_say_what_the_probe_gave_the_node() {
        assert_eq!(Table::of(false, false), Table::Miss);
        assert_eq!(Table::of(true, true), Table::Move);
        assert_eq!(Table::of(true, false), Table::ScoreOnly);
        assert_eq!(Table::Miss.word(), "miss");
        assert_eq!(Table::Move.word(), "move");
        assert_eq!(Table::ScoreOnly.word(), "score_only");
    }

    /// An event made up, for the tests that pin what the report prints.
    fn made_up(depth: u8, cut: Option<Cut>) -> Event {
        Event {
            fen: "4k3/8/8/8/8/8/8/4K3 w - - 0 1".to_string(),
            depth,
            window: Window::Zero,
            in_check: false,
            generated: 31,
            searched: match &cut {
                Some(cut) => cut.index + 1,
                None => 31,
            },
            cut,
            history_max: 25,
            quiets_scored: true,
            tt: Table::Miss,
            eval_beta: -12,
            cost: 40,
        }
    }

    fn cut_at(index: usize, class: Class) -> Option<Cut> {
        Some(Cut {
            index,
            class,
            history: 16,
            reduced: false,
        })
    }

    fn report_of(rows: Vec<Event>) -> Report {
        Report {
            depth: 4,
            every: 10,
            cap: DEFAULT_CAP,
            positions: 1,
            events: 400,
            overflowed: 0,
            rows,
        }
    }

    /// The row's fields in the order the module comment names them, with
    /// the fen last so a row parses left to right, and a held row printing
    /// `-` where a cut row has values rather than moving the columns.
    #[test]
    fn a_row_reads_left_to_right_with_the_fen_last() {
        let report = report_of(vec![
            Event {
                fen: "4k3/8/8/8/8/8/8/4K3 w - - 0 1".to_string(),
                depth: 3,
                window: Window::Zero,
                in_check: false,
                generated: 31,
                searched: 2,
                cut: Some(Cut {
                    index: 1,
                    class: Class::Killer,
                    history: 16,
                    reduced: true,
                }),
                history_max: 25,
                quiets_scored: true,
                tt: Table::Move,
                eval_beta: -37,
                cost: 214,
            },
            Event {
                fen: "4k3/8/8/8/8/8/8/4K3 b - - 0 1".to_string(),
                depth: 2,
                window: Window::Open,
                in_check: true,
                generated: 5,
                searched: 5,
                cut: None,
                history_max: 0,
                quiets_scored: false,
                tt: Table::Miss,
                eval_beta: 80,
                cost: 12,
            },
        ]);
        let text = report.to_string();
        let row = text.lines().nth(1).expect("a cut row");
        let words: Vec<&str> = row.splitn(16, ' ').collect();
        assert_eq!(
            words,
            vec![
                "3",
                "zw",
                "calm",
                "cut",
                "31",
                "2",
                "1",
                "killer",
                "16",
                "25",
                "scored",
                "move",
                "-37",
                "214",
                "reduced",
                "4k3/8/8/8/8/8/8/4K3 w - - 0 1",
            ]
        );
        let held = text.lines().nth(2).expect("a held row");
        let words: Vec<&str> = held.splitn(16, ' ').collect();
        assert_eq!(
            words,
            vec![
                "2",
                "open",
                "check",
                "held",
                "5",
                "5",
                "-",
                "-",
                "-",
                "0",
                "unscored",
                "miss",
                "80",
                "12",
                "-",
                "4k3/8/8/8/8/8/8/4K3 b - - 0 1",
            ]
        );
        assert!(
            text.starts_with("cutoffs depth 4 every 10 positions 1 events 400 records 2\n"),
            "{}",
            text
        );
    }

    /// The summary's counts, pinned against rows made up to land one in
    /// each bucket: the cut rate, the index bands, the two means, the class
    /// shares and the unscored share.
    #[test]
    fn the_summary_counts_the_cuts_and_where_they_fell() {
        let mut unscored = made_up(3, cut_at(0, Class::Table));
        unscored.quiets_scored = false;
        let report = report_of(vec![
            unscored,
            made_up(3, cut_at(2, Class::Capture)),
            made_up(3, cut_at(5, Class::Killer)),
            made_up(3, None),
        ]);
        let summary = report.summary(3).expect("four rows at depth three");
        assert_eq!(summary.records, 4);
        assert_eq!(summary.cuts, 3);
        assert_eq!((summary.at_first, summary.early, summary.late), (1, 1, 1));
        // one search each at indices 0, 2 and 5, so 1 + 3 + 6 searched
        // moves over the cuts, and the held row searched all 31
        assert_eq!(summary.searched_at_cuts, 10);
        assert_eq!(summary.searched_at_held, 31);
        assert_eq!(summary.classes, [1, 1, 0, 1, 0]);
        assert_eq!(summary.unscored_cuts, 1);
        assert!(
            report.to_string().contains(
                "depth 3 records 4 cuts 3 rate 75.00% index0 33.33% index1-3 33.33% \
                 index4+ 33.33% searched cut 3.3 held 31.0 table 33.33% capture 33.33% \
                 promotion 0.00% killer 33.33% quiet 0.00% unscored 33.33%"
            ),
            "{}",
            report
        );
    }

    /// A line a depth, and the depth with no rows is not invented.
    #[test]
    fn each_depth_is_summarised_on_its_own() {
        let report = report_of(vec![
            made_up(1, cut_at(0, Class::Capture)),
            made_up(1, None),
            made_up(3, cut_at(1, Class::Quiet)),
        ]);
        let depths: Vec<u8> = report.summaries().iter().map(|s| s.depth).collect();
        assert_eq!(depths, vec![1, 3]);
        assert!(report.summary(2).is_none());
        let text = report.to_string();
        assert!(
            text.contains("depth 1 records 2 cuts 1 rate 50.00% "),
            "{}",
            text
        );
        assert!(
            text.contains("depth 3 records 1 cuts 1 rate 100.00% "),
            "{}",
            text
        );
    }

    /// A depth of nothing but held rows has no cut to take a share of, so
    /// the cut figures print `-` rather than a rate that means nothing.
    #[test]
    fn a_depth_with_no_cuts_prints_no_cut_shares() {
        let report = report_of(vec![made_up(2, None)]);
        let text = report.to_string();
        assert!(
            text.contains(
                "depth 2 records 1 cuts 0 rate 0.00% index0 - index1-3 - index4+ - \
                 searched cut - held 31.0 table - capture - promotion - killer - quiet - \
                 unscored -"
            ),
            "{}",
            text
        );
    }

    /// The header states a cap off the default and an overflow, the way the
    /// residuals header does, and neither on an ordinary run.
    #[test]
    fn the_header_says_when_the_run_was_capped_or_dropped_something() {
        let mut report = report_of(Vec::new());
        let quiet_run = report.to_string();
        assert!(
            quiet_run.starts_with("cutoffs depth 4 every 10 positions 1 events 400 records 0\n"),
            "{}",
            quiet_run
        );
        assert!(!quiet_run.contains("overflow"), "{}", quiet_run);
        assert!(quiet_run.contains("\nrecords 0\n"), "{}", quiet_run);
        report.cap = 25;
        report.overflowed = 12;
        assert!(
            report.to_string().starts_with(
                "cutoffs depth 4 every 10 cap 25 positions 1 events 400 records 0 overflow 12\n"
            ),
            "{}",
            report
        );
    }

    /// A run over the suite: every row holds together with the position it
    /// names. The generated count is the list the fen regenerates, the
    /// searched count fits inside it, a cut's index is the last searched
    /// move, and the cost covers at least one node per move searched.
    #[test]
    fn a_run_records_rows_that_match_their_positions() {
        let report = run(&suite(), 4, 1, DEFAULT_CAP);
        assert_eq!(report.positions, 2);
        assert!(!report.rows.is_empty(), "nothing was recorded");
        assert!(report.events >= report.rows.len() as u64);
        for row in &report.rows {
            let board = Board::from_fen(&row.fen).unwrap_or_else(|e| panic!("{}: {}", row.fen, e));
            assert_eq!(board.in_check(), row.in_check, "{:?}", row);
            if row.generated == 0 {
                // the one way to answer without generating is the table's
                // move cutting first
                let cut = row.cut.as_ref().expect("an ungenerated node cut");
                assert_eq!(cut.class, Class::Table, "{:?}", row);
                assert_eq!(row.searched, 1, "{:?}", row);
                assert!(!row.quiets_scored, "{:?}", row);
            } else {
                let moves = if row.in_check {
                    board.evasions()
                } else {
                    board.generate_moves()
                };
                assert_eq!(row.generated, moves.len(), "{:?}", row);
                assert!(row.searched <= row.generated, "{:?}", row);
            }
            if let Some(cut) = &row.cut {
                assert_eq!(cut.index, row.searched - 1, "{:?}", row);
                if cut.class == Class::Quiet || cut.class == Class::Killer {
                    assert!(cut.history <= row.history_max, "{:?}", row);
                }
            }
            assert!(row.cost >= row.searched as u64, "{:?}", row);
        }
        // both outcomes are in the stream: the contrast is the census
        assert!(report.rows.iter().any(|row| row.cut.is_some()));
        assert!(report.rows.iter().any(|row| row.cut.is_none()));
        // and the table's move cutting before generation is common enough
        // that a full record of two positions holds one
        assert!(
            report.rows.iter().any(|row| row
                .cut
                .as_ref()
                .is_some_and(|cut| cut.class == Class::Table && cut.index == 0)),
            "no table move cutoff was recorded"
        );
    }

    /// A killer cutting past the front is what the census is for, and a
    /// full record of the suite holds one: index past 0, class killer, and
    /// its history within the quiets' largest.
    #[test]
    fn a_full_record_holds_a_killer_cutting_late() {
        let report = run(&suite(), 4, 1, DEFAULT_CAP);
        let killer = report
            .rows
            .iter()
            .filter_map(|row| row.cut.as_ref().map(|cut| (row, cut)))
            .find(|(_, cut)| cut.class == Class::Killer && cut.index >= 1)
            .expect("no killer cut past the front");
        assert_eq!(killer.1.index, killer.0.searched - 1);
        assert!(killer.1.history <= killer.0.history_max);
    }

    /// The census's contract: an engine with one searches the tree an
    /// engine without one searches. Asked of the armed engines themselves,
    /// position by position, rather than of two disarmed runs around them.
    #[test]
    fn recording_leaves_the_measured_search_where_it_was() {
        let searched_nodes = |engine: &mut AlphaBeta, id: &str| {
            let outcome =
                engine.iterative_deepening_search(SearchParameters::to_depth(4), |_, _, _, _| {});
            let crate::engine::SearchOutcome::Complete(result) = outcome else {
                panic!("{id}: an unlimited search did not complete");
            };
            result.nodes
        };
        let mut kept = 0;
        for position in &suite() {
            let board = Board::from_fen(&position.fen).unwrap();
            let mut plain =
                AlphaBeta::with_config(board.clone(), bench::TABLE_BYTES, SearchConfig::default());
            let plain_nodes = searched_nodes(&mut plain, &position.id);
            let mut armed =
                AlphaBeta::with_config(board, bench::TABLE_BYTES, SearchConfig::default());
            armed.sample_cutoffs(Sampler::with_cap(1, DEFAULT_CAP));
            let armed_nodes = searched_nodes(&mut armed, &position.id);
            assert_eq!(armed_nodes, plain_nodes, "{}", position.id);
            kept += armed
                .take_census()
                .expect("the sampler comes back")
                .drain()
                .taken
                .len();
        }
        assert!(kept > 0, "the armed runs recorded nothing");
    }

    /// The rate of zero and the depth of zero are held to one, so the
    /// header states the run that happened.
    #[test]
    fn a_rate_of_zero_is_reported_as_the_rate_that_ran() {
        let report = run(&suite(), 0, 0, 50);
        assert_eq!(report.depth, 1);
        assert_eq!(report.every, 1);
        assert!(
            report
                .to_string()
                .starts_with("cutoffs depth 1 every 1 cap 50 "),
            "{}",
            report
        );
    }
}
