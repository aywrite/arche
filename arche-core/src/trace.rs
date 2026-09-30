// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The trace mode: what the search's hot functions are asked, recorded as
//! it runs. Built only with the `trace` feature, so a build that plays
//! carries none of it.
//!
//! Every node the search enters is numbered here, in visit order, since the
//! engine's own counter restarts at each iteration. A node is sampled by a
//! hash of that number, and the nodes below a sampled one are sampled too,
//! to `window` plies, so a child's calls can be matched with its parent's.
//! A sampled node's position goes to the `nodes` stream, and every call a
//! hook sees while it is the node being searched goes to that hook's
//! stream, attributed to the node and to the line that made the call.
//!
//! The streams are files of fixed width little endian records, each behind
//! a header naming the stream, the format's version and the record width.
//! `manifest.json` beside them says how the run was made, how many records
//! each stream holds, and which line of the source each site number is.
//! Recording changes nothing: an armed search visits the nodes a disarmed
//! one does.

use crate::bench::{self, Position};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchParameters};
use crate::misc::{Color, Piece};
use crate::play::Play;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::panic::Location;
use std::path::{Path, PathBuf};

/// The format's version. A column added or moved is a new version.
pub const VERSION: u32 = 1;

/// About one node in every this many starts a sampled window, unless the
/// command says otherwise.
pub const DEFAULT_EVERY: u64 = 64;
/// How many plies below a sampled node are sampled with it.
pub const DEFAULT_WINDOW: u8 = 1;
/// The most records a stream keeps. Recording stops there; the search does
/// not.
pub const DEFAULT_CAP: u64 = 20_000_000;

/// Where a node sits in the search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Root = 0,
    Full = 1,
    Quiescence = 2,
}

/// The aggregate attack queries the `attacks` stream records. Only one so
/// far: `square_attacked` returns from several places, and its slider
/// probes are in the `sliders` stream already.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Query {
    AttackersTo = 1,
}

/// The streams, in the order their files are written and counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Nodes = 0,
    Sliders = 1,
    Attacks = 2,
    Swaps = 3,
    Lists = 4,
    Evals = 5,
    Walks = 6,
    Bounds = 7,
    Makes = 8,
    Orders = 9,
    Calls = 10,
}

impl Stream {
    const ALL: [Stream; 11] = [
        Stream::Nodes,
        Stream::Sliders,
        Stream::Attacks,
        Stream::Swaps,
        Stream::Lists,
        Stream::Evals,
        Stream::Walks,
        Stream::Bounds,
        Stream::Makes,
        Stream::Orders,
        Stream::Calls,
    ];

    fn name(self) -> &'static str {
        match self {
            Stream::Nodes => "nodes",
            Stream::Sliders => "sliders",
            Stream::Attacks => "attacks",
            Stream::Swaps => "swaps",
            Stream::Lists => "lists",
            Stream::Evals => "evals",
            Stream::Walks => "walks",
            Stream::Bounds => "bounds",
            Stream::Makes => "makes",
            Stream::Orders => "orders",
            Stream::Calls => "calls",
        }
    }

    /// Bytes a record, header excluded.
    fn width(self) -> usize {
        match self {
            Stream::Nodes => 96,
            Stream::Sliders => 32,
            Stream::Attacks => 32,
            Stream::Swaps => 32,
            Stream::Lists => 32,
            Stream::Evals => 96,
            Stream::Walks => 24,
            Stream::Bounds => 32,
            Stream::Makes => MAKE_WIDTH,
            Stream::Orders => 48,
            Stream::Calls => 32,
        }
    }
}

/// Whether `name` is a stream's, for the settings reader to refuse the rest.
pub fn is_stream(name: &str) -> bool {
    Stream::ALL.iter().any(|s| s.name() == name)
}

/// The node being searched, as the hooks see it.
#[derive(Clone, Copy, Default)]
struct Context {
    node: u64,
    /// The move list the node is working through, numbered when `order`
    /// was asked for it; zero before.
    list: u64,
    /// The node's last evaluation, numbered when it was recorded; zero
    /// before. What the search then compares it with is recorded against it.
    eval: u64,
    kind: u8,
    sampled: bool,
    /// How many more plies below this node are sampled because it is.
    left: u8,
}

thread_local! {
    static CONTEXT: Cell<Context> = const { Cell::new(Context { node: 0, list: 0, eval: 0, kind: 0, sampled: false, left: 0 }) };
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
}

thread_local! {
    /// Set while the trace does work of its own on the board, which the
    /// recorders then leave out.
    static MUTED: Cell<u32> = const { Cell::new(0) };
}

/// `f` with the recorders shut: what the trace asks of the board for its
/// own records (the evaluation's recount of the walk), or what a debug
/// check makes on a copy, is not what the search asked.
pub(crate) fn muted<R>(f: impl FnOnce() -> R) -> R {
    MUTED.with(|m| m.set(m.get() + 1));
    let result = f();
    MUTED.with(|m| m.set(m.get() - 1));
    result
}

fn is_muted() -> bool {
    MUTED.with(Cell::get) != 0
}

/// One stream's file and its counts.
struct Writer {
    out: BufWriter<File>,
    records: u64,
    dropped: u64,
}

/// Where an armed run's records go.
struct Sink {
    every: u64,
    window: u8,
    cap: u64,
    /// The suite position being searched.
    position: u16,
    /// Nodes entered so far, sampled or not: the next node's number less one.
    entered: u64,
    /// Move lists begun in sampled nodes so far: the next list's number
    /// less one.
    lists: u64,
    /// Evaluations made in sampled nodes so far: the next one's number less
    /// one. The walk's records are written before the evaluation's own, so
    /// they carry the number it is about to take.
    evals: u64,
    /// Pieces walked for the evaluation being made.
    walked: u8,
    sampled: u64,
    /// Which streams are recorded; the others' files hold a header alone.
    enabled: [bool; 11],
    /// History entries written (one `gravitate` each) and quiet cutoffs
    /// taught, sampled or not: the ordering's memories as a clock.
    history_writes: u64,
    history_cutoffs: u64,
    /// Moves made (and passes) so far, sampled or not.
    makes: u64,
    /// One frame a move made and not yet taken back, innermost last.
    frames: Vec<Frame>,
    /// What `gives_check` answered at each sampled node still on the path,
    /// by the move asked about.
    asked: HashMap<u64, Vec<(u8, u8, u8, bool)>>,
    writers: Vec<Writer>,
    sites: HashMap<(&'static str, u32, u32), u16>,
    site_list: Vec<(&'static str, u32, u32)>,
    failed: Option<io::Error>,
}

impl Sink {
    #[cfg(test)]
    fn create(dir: &Path, every: u64, window: u8, cap: u64) -> io::Result<Self> {
        Self::create_only(dir, every, window, cap, None)
    }

    /// The same, recording only the streams named (the nodes always).
    fn create_only(
        dir: &Path,
        every: u64,
        window: u8,
        cap: u64,
        only: Option<&[String]>,
    ) -> io::Result<Self> {
        // the calls stream records every node's lists, sampled or not, so it
        // is written only when it is named
        let enabled = Stream::ALL.map(|stream| {
            stream == Stream::Nodes
                || only.map_or(stream != Stream::Calls, |names| {
                    names.iter().any(|n| n == stream.name())
                })
        });
        std::fs::create_dir_all(dir)?;
        let mut writers = Vec::new();
        for stream in Stream::ALL {
            let mut out = BufWriter::with_capacity(
                1 << 20,
                File::create(dir.join(format!("{}.bin", stream.name())))?,
            );
            // the header: a tag, the version, the stream and its width
            out.write_all(b"ARCHETRC")?;
            out.write_all(&VERSION.to_le_bytes())?;
            out.write_all(&(stream as u32).to_le_bytes())?;
            out.write_all(&(stream.width() as u32).to_le_bytes())?;
            out.write_all(&0u32.to_le_bytes())?;
            writers.push(Writer {
                out,
                records: 0,
                dropped: 0,
            });
        }
        Ok(Self {
            every: every.max(1),
            window,
            cap,
            position: 0,
            entered: 0,
            lists: 0,
            evals: 0,
            walked: 0,
            sampled: 0,
            enabled,
            history_writes: 0,
            history_cutoffs: 0,
            makes: 0,
            frames: Vec::new(),
            asked: HashMap::new(),
            writers,
            sites: HashMap::new(),
            site_list: Vec::new(),
            failed: None,
        })
    }

    fn site(&mut self, at: &'static Location<'static>) -> u16 {
        let key = (at.file(), at.line(), at.column());
        if let Some(&id) = self.sites.get(&key) {
            return id;
        }
        let id = u16::try_from(self.site_list.len()).expect("fewer than 65,536 call sites");
        self.sites.insert(key, id);
        self.site_list.push(key);
        id
    }

    fn write(&mut self, stream: Stream, record: &[u8]) {
        debug_assert_eq!(record.len(), stream.width());
        if !self.enabled[stream as usize] {
            return;
        }
        let writer = &mut self.writers[stream as usize];
        if writer.records >= self.cap {
            writer.dropped += 1;
            return;
        }
        if let Err(e) = writer.out.write_all(record) {
            self.failed.get_or_insert(e);
            return;
        }
        writer.records += 1;
    }
}

/// A node's context, put back when the node returns.
pub(crate) struct Entered {
    saved: Context,
}

impl Drop for Entered {
    fn drop(&mut self) {
        let leaving = CONTEXT.with(Cell::get);
        if leaving.sampled {
            SINK.with(|sink| {
                if let Some(sink) = sink.borrow_mut().as_mut() {
                    sink.asked.remove(&leaving.node);
                }
            });
        }
        CONTEXT.with(|c| c.set(self.saved));
    }
}

/// A node entered: numbered, sampled or not, and its position recorded if it
/// is. Nothing is done when no run is armed.
pub(crate) fn enter(kind: Kind, board: &Board, depth: u8) -> Option<Entered> {
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let sink = sink.as_mut()?;
        let saved = CONTEXT.with(Cell::get);
        sink.entered += 1;
        let node = sink.entered;
        let (sampled, left) = if saved.sampled && saved.left > 0 {
            (true, saved.left - 1)
        } else if mix(node) % sink.every == 0 {
            (true, sink.window)
        } else {
            (false, 0)
        };
        // a node whose position the capped stream cannot take is not
        // sampled, so no call is recorded against a node that is not there
        let sampled = sampled && sink.writers[Stream::Nodes as usize].records < sink.cap;
        if sampled {
            sink.sampled += 1;
            let mut record = [0u8; 96];
            record[0..8].copy_from_slice(&node.to_le_bytes());
            record[8..16].copy_from_slice(&saved.node.to_le_bytes());
            record[16..18].copy_from_slice(&sink.position.to_le_bytes());
            record[18] = kind as u8;
            record[19] = u8::try_from(board.line_ply).unwrap_or(u8::MAX);
            record[20] = depth;
            let snapshot = board.traced();
            record[21] = snapshot.side;
            record[22] = snapshot.castle;
            record[23] = snapshot.en_passant;
            for (i, board) in snapshot.boards.iter().enumerate() {
                record[24 + 8 * i..32 + 8 * i].copy_from_slice(&board.to_le_bytes());
            }
            // 24 + 8 * 8 = 88
            record[88..96].copy_from_slice(&snapshot.key.to_le_bytes());
            sink.write(Stream::Nodes, &record);
        }
        CONTEXT.with(|c| {
            c.set(Context {
                node,
                list: 0,
                eval: 0,
                kind: kind as u8,
                sampled,
                left,
            })
        });
        Some(Entered { saved })
    })
}

/// A magic probe. `relevant` is the occupancy under the square's blocker
/// mask, which is everything the result depends on.
pub(crate) fn slider(
    at: &'static Location<'static>,
    straight: bool,
    square: u8,
    relevant: u64,
    result: u64,
) {
    let context = CONTEXT.with(Cell::get);
    if !context.sampled || is_muted() {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let site = sink.site(at);
        let mut record = [0u8; 32];
        record[0..8].copy_from_slice(&context.node.to_le_bytes());
        record[8..10].copy_from_slice(&site.to_le_bytes());
        record[10] = u8::from(straight);
        record[11] = square;
        record[16..24].copy_from_slice(&relevant.to_le_bytes());
        record[24..32].copy_from_slice(&result.to_le_bytes());
        sink.write(Stream::Sliders, &record);
    });
}

/// An aggregate attack query. `color` is the attacking side's, or 2 where
/// the query takes both.
pub(crate) fn attack(
    at: &'static Location<'static>,
    query: Query,
    square: u8,
    color: u8,
    occupied: u64,
    result: u64,
) {
    let context = CONTEXT.with(Cell::get);
    if !context.sampled || is_muted() {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let site = sink.site(at);
        let mut record = [0u8; 32];
        record[0..8].copy_from_slice(&context.node.to_le_bytes());
        record[8..10].copy_from_slice(&site.to_le_bytes());
        record[10] = query as u8;
        record[11] = square;
        record[12] = color;
        record[16..24].copy_from_slice(&occupied.to_le_bytes());
        record[24..32].copy_from_slice(&result.to_le_bytes());
        sink.write(Stream::Attacks, &record);
    });
}

/// A sampled node is about to have its moves ordered: the list is numbered,
/// and the swaps the ordering runs and the moves the loop reaches are
/// recorded against it.
pub(crate) fn list_begin() {
    let mut context = CONTEXT.with(Cell::get);
    if !context.sampled {
        return;
    }
    SINK.with(|sink| {
        if let Some(sink) = sink.borrow_mut().as_mut() {
            sink.lists += 1;
            context.list = sink.lists;
        }
    });
    CONTEXT.with(|c| c.set(context));
}

/// One swap the ordering ran, with the piece that captures.
pub(crate) fn swap(m: &Play, attacker: Option<Piece>, see: i32) {
    let context = CONTEXT.with(Cell::get);
    if !context.sampled || context.list == 0 {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let mut record = [0u8; 32];
        record[0..8].copy_from_slice(&context.list.to_le_bytes());
        record[8..16].copy_from_slice(&context.node.to_le_bytes());
        record[16] = m.from;
        record[17] = m.to;
        record[18] = piece_code(m.capture);
        record[19] = piece_code(attacker);
        record[20] = promote_code(m);
        record[21] = u8::from(m.en_passant);
        record[24..28].copy_from_slice(&see.to_le_bytes());
        sink.write(Stream::Swaps, &record);
    });
}

/// What a `lists` record says about its move.
#[derive(Clone, Copy)]
enum Listed {
    /// Its place in the list as `order` left it.
    Ordered = 0,
    /// The loop came to it, at the index given.
    Reached = 1,
    /// Its score cut the node off.
    Cutoff = 2,
}

/// The list as `order` left it, one record a move. `table` is the move the
/// ordering was told the table holds.
pub(crate) fn ordered(moves: &[Play], table: Option<Play>) {
    for (i, m) in moves.iter().enumerate() {
        listed(Listed::Ordered, i, m, table == Some(*m));
    }
}

/// The loop has come to the move at `index`.
pub(crate) fn reached(index: usize, m: &Play) {
    listed(Listed::Reached, index, m, false);
}

/// The move at `index` cut the node off.
pub(crate) fn cutoff(index: usize, m: &Play) {
    listed(Listed::Cutoff, index, m, false);
}

fn listed(what: Listed, index: usize, m: &Play, table: bool) {
    let context = CONTEXT.with(Cell::get);
    if !context.sampled || context.list == 0 {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let mut record = [0u8; 32];
        record[0..8].copy_from_slice(&context.list.to_le_bytes());
        record[8..16].copy_from_slice(&context.node.to_le_bytes());
        record[16] = what as u8;
        record[17] = context.kind;
        record[18] = u8::try_from(index).unwrap_or(u8::MAX);
        record[19] = m.from;
        record[20] = m.to;
        record[21] = piece_code(m.capture);
        record[22] = promote_code(m);
        record[23] = u8::from(m.en_passant) | u8::from(m.castle) << 1 | u8::from(table) << 2;
        sink.write(Stream::Lists, &record);
    });
}

/// What an `orders` record says. Each record carries the list's number, the
/// node's, the event, the node's kind, an index, a move and three words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Order {
    /// One generated move, in generation order: `a` is the generator (0
    /// full, 1 captures, 2 evasions) and `b` the list's length.
    Generated = 1,
    /// Quiescence's delta filter before the order: `a` the moves kept,
    /// `c` a mask of the generated places kept.
    Filtered = 2,
    /// `order` has returned: `a` the front, `b` the losing captures (-1
    /// where the caller does not keep them), `c` the table's place (255 for
    /// none, and for a caller that does not keep it) with 256 set when the list spilled
    /// the buffer, and the index the memory ply (255 for none).
    StageOne = 3,
    /// The quiet run keyed, one record a quiet in the order `order` left
    /// it: the index is its place in the run, `a` its history entry, `b`
    /// which killer it is (0 none, 1 or 2), `c` the packed key the engine
    /// made. The run's first record also carries, in the move columns of
    /// a `Killers` record written before it, the two killers.
    Keyed = 4,
    /// The ply's two killers as the keying read them, one in the move
    /// columns and the other packed into `a`; `b` the run's length, `c`
    /// the history writes so far; the index is 1 when the ledger ordered
    /// the run whole.
    Killers = 5,
    /// `pick` at run place `t` (the index): the move it put there.
    Pick = 6,
    /// `sort_rest` from run place `t`: `a` how many it sorted.
    SortRest = 7,
    /// `keep_unskippable` from run place `t`: `a` where the survivors end
    /// (in run places), `b` how many it asked `gives_check` of.
    Keep = 8,
    /// What the loop did with the move at the index: `a` the outcome (see
    /// `Outcome`), `b` the moves searched before it, `c` the nodes entered
    /// below it.
    Decided = 9,
    /// The loop ended: `a` the index it stopped at, `b` the cutoff's index
    /// or -1, `c` the history writes so far.
    End = 10,
    /// The table's move before generation, recorded against list zero: `a`
    /// the outcome (0 not pseudo legal, 1 illegal, 2 searched, 3 cut), `c`
    /// the nodes entered below it.
    Table = 11,
}

/// What the loop did with a move it came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Searched at the node's depth, no scout.
    Searched = 0,
    /// Scouted shallower first (`Decided`'s index carries the move; the
    /// reduction is in the high byte of `a`).
    Scouted = 1,
    /// Dropped by the shallow rules (quiet futility or the late move count).
    Shallow = 2,
    /// Dropped by the late move gate.
    Pruned = 3,
    /// Made and found illegal.
    Illegal = 4,
    /// The table's move, searched before the list, passed over.
    TablePlace = 5,
    /// Behind the survivors `keep_unskippable` kept: never reached.
    Dropped = 6,
    /// Quiescence's delta test in the loop.
    Delta = 7,
    /// Quiescence's losing capture skip.
    Losing = 8,
}

fn order_record(what: Order, index: usize, m: Option<&Play>, a: i64, b: i64, c: u64) {
    let context = CONTEXT.with(Cell::get);
    if !context.sampled || (context.list == 0 && what != Order::Table) {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let mut record = [0u8; 48];
        let list = if what == Order::Table {
            0
        } else {
            context.list
        };
        record[0..8].copy_from_slice(&list.to_le_bytes());
        record[8..16].copy_from_slice(&context.node.to_le_bytes());
        record[16] = what as u8;
        record[17] = context.kind;
        record[18] = u8::try_from(index).unwrap_or(u8::MAX);
        if let Some(m) = m {
            record[19] = m.from;
            record[20] = m.to;
            record[21] = piece_code(m.capture);
            record[22] = promote_code(m);
            record[23] = u8::from(m.en_passant) | u8::from(m.castle) << 1;
        } else {
            record[21] = 7;
        }
        record[24..32].copy_from_slice(&a.to_le_bytes());
        record[32..40].copy_from_slice(&b.to_le_bytes());
        record[40..48].copy_from_slice(&c.to_le_bytes());
        sink.write(Stream::Orders, &record);
    });
}

/// A move packed in sixteen bits for a record's word: from, to, and the
/// flags above them. Zero for none.
fn packed_play(m: Option<Play>) -> i64 {
    m.map_or(0, |m| {
        1 << 40
            | i64::from(m.from)
            | i64::from(m.to) << 8
            | i64::from(piece_code(m.capture)) << 16
            | i64::from(promote_code(&m)) << 24
            | (i64::from(m.en_passant) | i64::from(m.castle) << 1) << 32
    })
}

/// The list as generated, before anything reorders or filters it.
pub(crate) fn generated(moves: &[Play], generator: u8) {
    for (i, m) in moves.iter().enumerate() {
        order_record(
            Order::Generated,
            i,
            Some(m),
            i64::from(generator),
            moves.len() as i64,
            0,
        );
    }
}

/// Quiescence's delta filter kept `kept` of `generated`.
pub(crate) fn filtered(generated: &[Play], kept: &[Play]) {
    let mut mask = 0u64;
    let mut j = 0;
    for (i, m) in generated.iter().enumerate() {
        if j < kept.len() && kept[j] == *m {
            mask |= 1 << i.min(63);
            j += 1;
        }
    }
    order_record(Order::Filtered, 0, None, kept.len() as i64, 0, mask);
}

/// What `order` reported.
pub(crate) fn stage_one(
    front: usize,
    losing: usize,
    table_at: Option<usize>,
    spilled: bool,
    ply: Option<usize>,
) {
    let place = table_at.map_or(255, |t| t as u64) | u64::from(spilled) << 8;
    order_record(
        Order::StageOne,
        ply.unwrap_or(255),
        None,
        front as i64,
        if losing == usize::MAX {
            -1
        } else {
            losing as i64
        },
        place,
    );
}

/// The quiet run keyed: the killers, then each quiet with its history
/// entry, which killer it is, and its packed key.
pub(crate) fn keyed(
    killers: [Option<Play>; 2],
    run: &[Play],
    history: impl Fn(&Play) -> i32,
    keys: &[i64],
    whole: bool,
) {
    let writes = SINK.with(|sink| sink.borrow().as_ref().map_or(0, |s| s.history_writes));
    order_record(
        Order::Killers,
        usize::from(whole),
        killers[0].as_ref(),
        packed_play(killers[1]),
        run.len() as i64,
        writes,
    );
    for (i, m) in run.iter().enumerate() {
        let killer = if killers[0] == Some(*m) {
            1
        } else if killers[1] == Some(*m) {
            2
        } else {
            0
        };
        order_record(
            Order::Keyed,
            i,
            Some(m),
            i64::from(history(m)),
            killer,
            keys.get(i).map_or(0, |&k| k as u64),
        );
    }
}

/// `pick` put `m` at run place `t`.
pub(crate) fn picked(t: usize, m: &Play) {
    order_record(Order::Pick, t, Some(m), 0, 0, 0);
}

/// `sort_rest` sorted `count` from run place `t`.
pub(crate) fn sorted_rest(t: usize, count: usize) {
    order_record(Order::SortRest, t, None, count as i64, 0, 0);
}

/// `keep_unskippable` from run place `t` kept up to `kept`, asking
/// `asked` moves whether they give check.
pub(crate) fn kept(t: usize, kept: usize, asked: usize) {
    order_record(Order::Keep, t, None, kept as i64, asked as i64, 0);
}

/// The nodes entered so far, for a caller that wants the nodes below a move.
pub(crate) fn entered() -> u64 {
    SINK.with(|sink| sink.borrow().as_ref().map_or(0, |s| s.entered))
}

/// What the loop did with the move at `index`.
pub(crate) fn decided(
    index: usize,
    m: &Play,
    outcome: Outcome,
    reduction: u8,
    searched: usize,
    below: u64,
) {
    order_record(
        Order::Decided,
        index,
        Some(m),
        outcome as i64 | i64::from(reduction) << 8,
        searched as i64,
        below,
    );
}

/// The loop stopped at `index`, cut off there or at its end.
pub(crate) fn ended(index: usize, cut: Option<usize>) {
    let writes = SINK.with(|sink| sink.borrow().as_ref().map_or(0, |s| s.history_writes));
    order_record(
        Order::End,
        index,
        None,
        index as i64,
        cut.map_or(-1, |c| c as i64),
        writes,
    );
}

/// The table's move, tried before the list: 0 not pseudo legal, 1
/// illegal, 2 searched, 3 cut.
pub(crate) fn table_tried(m: &Play, outcome: u8, below: u64) {
    order_record(Order::Table, 0, Some(m), i64::from(outcome), 0, below);
}

/// A cutoff taught the memories: `writes` history entries changed.
pub(crate) fn taught(writes: usize) {
    SINK.with(|sink| {
        if let Some(sink) = sink.borrow_mut().as_mut() {
            sink.history_writes += writes as u64;
            sink.history_cutoffs += 1;
        }
    });
}

/// One list of any node, sampled or not, as the `calls` stream records it:
/// the position's key, a hash of the moves in their order, what the list
/// is (0 full, 1 captures, 2 evasions as generated, 3 the order `order`
/// left, 4 the quiet run's keys), the node's kind and the length.
pub(crate) fn call(board: &Board, what: u8, kind: Kind, moves: &[Play], keys: &[i64]) {
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        if !sink.enabled[Stream::Calls as usize] {
            return;
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |x: u64| {
            h ^= x;
            h = h.wrapping_mul(0x0100_0000_01b3);
            h ^= h >> 29;
        };
        for m in moves {
            eat(packed_play(Some(*m)) as u64);
        }
        for &k in keys {
            eat(k as u64);
        }
        let mut record = [0u8; 32];
        record[0..8].copy_from_slice(&board.key.to_le_bytes());
        record[8..16].copy_from_slice(&h.to_le_bytes());
        record[16] = what;
        record[17] = kind as u8;
        record[18] = u8::try_from(moves.len()).unwrap_or(u8::MAX);
        record[20..22].copy_from_slice(&sink.position.to_le_bytes());
        record[24..32].copy_from_slice(&sink.entered.to_le_bytes());
        sink.write(Stream::Calls, &record);
    });
}

/// A piece as the streams number it, pawn 0 to king 5, and 6 for none.
fn piece_code(piece: Option<Piece>) -> u8 {
    piece.map_or(6, |p| p as u8)
}

/// A promotion as the streams number it: 0 for none, then knight 1 to
/// queen 4.
fn promote_code(m: &Play) -> u8 {
    m.promote.map_or(0, |p| p as u8 + 1)
}

/// What one evaluation was made of, as the `evals` stream records it. The
/// packed pairs are the tapered halves `psqt::pack` makes, before the one
/// divide.
pub(crate) struct Evaluation {
    pub(crate) score: i16,
    /// Material that cannot mate, so the sum answered zero and walked
    /// nothing.
    pub(crate) drawn: bool,
    /// Whether the shelter's and the pawn structure's tables held the
    /// position before it was asked, or none where the sum was handed no
    /// tables.
    pub(crate) hits: Option<(bool, bool)>,
    /// The accumulator's phase, before the cap.
    pub(crate) phase: i32,
    pub(crate) psqt: i32,
    /// White's material less black's.
    pub(crate) material: i32,
    /// The pair term's score, which joins material outside the divide.
    pub(crate) machine: i32,
    pub(crate) mobility: i32,
    pub(crate) king_attack: i32,
    pub(crate) shelter: i32,
    pub(crate) pawn_structure: i32,
    /// Each side's mobility counts and its king attack counts, white's
    /// first, as the walk returned them.
    pub(crate) scope: [[i32; 4]; 2],
    pub(crate) ring: [[i32; 4]; 2],
}

/// An evaluation, recorded when the sum returns. `at` is the line in the
/// search that asked for it.
pub(crate) fn evaluated(at: &'static Location<'static>, board: &Board, e: &Evaluation) {
    let mut context = CONTEXT.with(Cell::get);
    if !context.sampled {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let site = sink.site(at);
        sink.evals += 1;
        context.eval = sink.evals;
        let walked = std::mem::take(&mut sink.walked);
        let mut record = [0u8; 96];
        record[0..8].copy_from_slice(&context.node.to_le_bytes());
        record[8..16].copy_from_slice(&sink.evals.to_le_bytes());
        record[16..24].copy_from_slice(&board.key.to_le_bytes());
        record[24..26].copy_from_slice(&site.to_le_bytes());
        let (shelter_hit, pawns_hit) = e.hits.unwrap_or((false, false));
        record[26] = u8::from(e.drawn)
            | u8::from(e.hits.is_some()) << 1
            | u8::from(shelter_hit) << 2
            | u8::from(pawns_hit) << 3;
        record[27] = walked;
        record[28..30].copy_from_slice(&e.score.to_le_bytes());
        record[30..32].copy_from_slice(&u16::try_from(e.phase).unwrap_or(u16::MAX).to_le_bytes());
        for (i, value) in [
            e.psqt,
            e.material,
            e.machine,
            e.mobility,
            e.king_attack,
            e.shelter,
            e.pawn_structure,
        ]
        .into_iter()
        .enumerate()
        {
            record[32 + 4 * i..36 + 4 * i].copy_from_slice(&value.to_le_bytes());
        }
        // 32 + 7 * 4 = 60, then four bytes of padding
        let counts = e.scope[0]
            .iter()
            .chain(&e.scope[1])
            .chain(&e.ring[0])
            .chain(&e.ring[1]);
        for (i, count) in counts.enumerate() {
            let count = u16::try_from(*count).unwrap_or(u16::MAX);
            record[64 + 2 * i..66 + 2 * i].copy_from_slice(&count.to_le_bytes());
        }
        sink.write(Stream::Evals, &record);
    });
    CONTEXT.with(|c| c.set(context));
}

/// One piece the evaluation's walk visited: its attack set, and the squares
/// of it each term counted (`u8::MAX` for a term that does not count it).
pub(crate) fn walked(color: Color, kind: usize, square: u8, attacks: u64, scope: u8, ring: u8) {
    let context = CONTEXT.with(Cell::get);
    if !context.sampled {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        sink.walked = sink.walked.saturating_add(1);
        let mut record = [0u8; 24];
        record[0..8].copy_from_slice(&(sink.evals + 1).to_le_bytes());
        record[8..16].copy_from_slice(&attacks.to_le_bytes());
        record[16] = square;
        record[17] = u8::try_from(kind).unwrap_or(u8::MAX);
        record[18] = color as u8;
        record[19] = scope;
        record[20] = ring;
        sink.write(Stream::Walks, &record);
    });
}

/// What the search compared an evaluation with, or did with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    /// Quiescence's stand pat against beta: at or above it, the node
    /// returns the evaluation.
    StandPatBeta = 1,
    /// The same against alpha: at or above it, the evaluation is alpha.
    StandPatAlpha = 2,
    /// The delta test on one capture before the ordering, at the alpha the
    /// stand pat left. The capture is dropped when the evaluation is under
    /// the threshold.
    DeltaFilter = 3,
    /// The same test in the move loop, at the alpha the loop has reached.
    DeltaLoop = 4,
    /// Quiescence returning: whether its value is the evaluation, which no
    /// capture beat.
    Returned = 5,
    /// Reverse futility: at or above the threshold, the node returns the
    /// evaluation less the margin.
    ReverseFutility = 6,
    /// The null move's gate against beta. The pass's reduction reads how
    /// far above beta the evaluation stands.
    NullMove = 7,
    /// Quiet futility: at or under the threshold, the node's later quiets
    /// are dropped.
    QuietFutility = 8,
    /// The late move gate's score, which reads the evaluation linearly.
    /// The threshold column holds the score with the evaluation's part
    /// taken out, and the outcome is whether the score is at or under the
    /// pruning threshold (a move that gives check is searched all the same).
    LateMoveGate = 9,
}

/// One use of the node's last evaluation. `threshold` is the bound the
/// value was compared with, `value` the evaluation as the rule read it and
/// `aux` whatever else the rule needs (the captured piece, the gate's
/// score).
pub(crate) fn bound(kind: Bound, threshold: i32, value: i32, aux: i32, outcome: bool) {
    let context = CONTEXT.with(Cell::get);
    if !context.sampled {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let mut record = [0u8; 32];
        record[0..8].copy_from_slice(&context.node.to_le_bytes());
        record[8..16].copy_from_slice(&context.eval.to_le_bytes());
        record[16] = kind as u8;
        record[17] = u8::from(outcome);
        record[20..24].copy_from_slice(&threshold.to_le_bytes());
        record[24..28].copy_from_slice(&value.to_le_bytes());
        record[28..32].copy_from_slice(&aux.to_le_bytes());
        sink.write(Stream::Bounds, &record);
    });
}

/// A read of the state `make_move` keeps, as the `makes` stream counts it.
/// Each is counted where the search asks, whether or not the node is
/// sampled, so a sampled move's record says what its whole subtree read.
/// The upkeep's own reads (the next move's xor into the key, `undo_move`'s
/// restore) are not reads here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Read {
    /// The transposition table's probe, or a lookup of its move, by the key.
    KeyProbe = 0,
    /// A store into the table, by the key.
    KeyStore = 1,
    /// The repetition test: the key against the keys the history ring holds.
    Repetition = 2,
    /// The fifty move counter, by the draw rule or the probe's guard.
    Fifty = 3,
    /// The pawn key, by the pawn structure's table or the shelter's key.
    PawnKey = 4,
    /// The accumulator and the pair term's sums, by the taper.
    Accumulator = 5,
    /// Whether the side to move stands in check, asked by the search, the
    /// generator's choice of list, or a draw test.
    InCheck = 6,
    /// The same, asked by a move made from here, whose legality probe it
    /// chooses.
    CheckedByMake = 7,
    /// The checking pieces themselves, which the evasion mask reads.
    Checkers = 8,
    /// What stands on a square, read from the `squares` array.
    Squares = 9,
    /// The piece boards, by a generator, the evaluation, the swap, a check
    /// test, the table move's test or the zugzwang guard. A move made below
    /// reads them too; the record counts those makes separately.
    Boards = 10,
}

/// How many kinds of read the `makes` stream counts.
pub(crate) const READS: usize = 11;

thread_local! {
    static READ_COUNTS: [Cell<u64>; READS] = const { [const { Cell::new(0) }; READS] };
}

/// One read of the kept state.
#[inline(always)]
pub(crate) fn read(what: Read) {
    if is_muted() {
        return;
    }
    READ_COUNTS.with(|c| {
        let c = &c[what as usize];
        c.set(c.get() + 1);
    });
}

fn read_counts() -> [u64; READS] {
    READ_COUNTS.with(|c| std::array::from_fn(|i| c[i].get()))
}

/// Bytes a `makes` record.
const MAKE_WIDTH: usize = 232 + 8 * READS;

/// A move made and not yet taken back.
struct Frame {
    /// The read counts when it was made.
    counts: [u64; READS],
    /// The same when the first move below it was made, or none yet.
    first: Option<[u64; READS]>,
    entered: u64,
    makes: u64,
    /// Its record, whose last columns are filled when it is taken back;
    /// none where the node that made it is not sampled.
    record: Option<Box<[u8; MAKE_WIDTH]>>,
}

/// What a made move (or a pass) left on the board, as the `makes` stream
/// records it. Taken when the legality probe has answered: an illegal move
/// is recorded as made, just before it is taken back.
pub(crate) struct Made {
    pub(crate) play: Play,
    pub(crate) pass: bool,
    pub(crate) legal: bool,
    /// The piece that moved, as `squares` held it.
    pub(crate) moved: Option<Piece>,
    /// Which legality probe ran: 0 none, 1 a rook line, 2 a bishop line, 3
    /// the whole attack test.
    pub(crate) exposure: u8,
    pub(crate) key_before: u64,
    pub(crate) fifty_before: u16,
    pub(crate) snapshot: Snapshot,
    pub(crate) pawn_key: u64,
    /// Zero for an illegal move, whose checkers are never computed.
    pub(crate) checkers: u64,
    pub(crate) fifty: u16,
    pub(crate) psqt: i32,
    pub(crate) material: [u32; 2],
    pub(crate) phase: i32,
    pub(crate) sums: [[i16; 16]; 2],
    pub(crate) diagonal: [i32; 2],
}

/// `gives_check` answered for a move at the node being searched.
pub(crate) fn asked_check(m: &Play, answer: bool) {
    let context = CONTEXT.with(Cell::get);
    if !context.sampled {
        return;
    }
    SINK.with(|sink| {
        if let Some(sink) = sink.borrow_mut().as_mut() {
            sink.asked.entry(context.node).or_default().push((
                m.from,
                m.to,
                promote_code(m),
                answer,
            ));
        }
    });
}

/// A move made, or a pass. `made` is asked only where the node making it is
/// sampled. `at` is the line that asked for the move.
pub(crate) fn made(at: &'static Location<'static>, made: impl FnOnce() -> Made) {
    if is_muted() {
        return;
    }
    let context = CONTEXT.with(Cell::get);
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let counts = read_counts();
        if let Some(top) = sink.frames.last_mut() {
            top.first.get_or_insert(counts);
        }
        sink.makes += 1;
        let record = if context.sampled && sink.writers[Stream::Makes as usize].records < sink.cap {
            let m = made();
            let site = sink.site(at);
            let asked = sink
                .asked
                .get(&context.node)
                .and_then(|asked| {
                    asked.iter().rev().find(|&&(from, to, promote, _)| {
                        (from, to, promote) == (m.play.from, m.play.to, promote_code(&m.play))
                    })
                })
                .map_or(0, |&(_, _, _, answer)| 1 + u8::from(answer));
            let mut r = Box::new([0u8; MAKE_WIDTH]);
            r[0..8].copy_from_slice(&context.node.to_le_bytes());
            r[8..16].copy_from_slice(&m.key_before.to_le_bytes());
            r[16..24].copy_from_slice(&m.snapshot.key.to_le_bytes());
            r[24..32].copy_from_slice(&m.pawn_key.to_le_bytes());
            r[32..40].copy_from_slice(&m.checkers.to_le_bytes());
            for (i, board) in m.snapshot.boards.iter().enumerate() {
                r[40 + 8 * i..48 + 8 * i].copy_from_slice(&board.to_le_bytes());
            }
            // 40 + 8 * 8 = 104
            r[104] = m.play.from;
            r[105] = m.play.to;
            r[106] = piece_code(m.play.capture);
            r[107] = promote_code(&m.play);
            r[108] = u8::from(m.play.en_passant)
                | u8::from(m.play.castle) << 1
                | u8::from(m.pass) << 2
                | u8::from(m.legal) << 3;
            r[109] = piece_code(m.moved);
            r[110] = m.exposure;
            r[111] = m.snapshot.castle;
            r[112] = m.snapshot.en_passant;
            r[113] = m.snapshot.side;
            r[114..116].copy_from_slice(&m.fifty.to_le_bytes());
            r[116..118].copy_from_slice(&site.to_le_bytes());
            r[118] = context.kind;
            r[119] = asked;
            r[120..124].copy_from_slice(&m.psqt.to_le_bytes());
            r[124..128].copy_from_slice(&m.material[0].to_le_bytes());
            r[128..132].copy_from_slice(&m.material[1].to_le_bytes());
            r[132..136].copy_from_slice(&m.phase.to_le_bytes());
            for (p, sums) in m.sums.iter().enumerate() {
                for (lane, sum) in sums.iter().enumerate() {
                    let at = 136 + 32 * p + 2 * lane;
                    r[at..at + 2].copy_from_slice(&sum.to_le_bytes());
                }
            }
            // 136 + 64 = 200
            r[200..204].copy_from_slice(&m.diagonal[0].to_le_bytes());
            r[204..208].copy_from_slice(&m.diagonal[1].to_le_bytes());
            let tail = 216 + 8 * READS;
            // the child's number, if the move is searched, is the next node
            r[tail..tail + 8].copy_from_slice(&(sink.entered + 1).to_le_bytes());
            r[tail + 8..tail + 10].copy_from_slice(&m.fifty_before.to_le_bytes());
            Some(r)
        } else {
            None
        };
        sink.frames.push(Frame {
            counts,
            first: None,
            entered: sink.entered,
            makes: sink.makes,
            record,
        });
    });
}

/// The innermost move made is being taken back: its record, if it has one,
/// is finished with what its subtree read and written.
pub(crate) fn unmade() {
    if is_muted() {
        return;
    }
    SINK.with(|sink| {
        let mut sink = sink.borrow_mut();
        let Some(sink) = sink.as_mut() else {
            return;
        };
        let Some(frame) = sink.frames.pop() else {
            return;
        };
        let Some(mut r) = frame.record else {
            return;
        };
        let counts = read_counts();
        let first = frame.first.unwrap_or(counts);
        let nodes = u32::try_from(sink.entered - frame.entered).unwrap_or(u32::MAX);
        if nodes == 0 {
            let tail = 216 + 8 * READS;
            r[tail..tail + 8].copy_from_slice(&0u64.to_le_bytes());
        }
        let makes = u32::try_from(sink.makes - frame.makes).unwrap_or(u32::MAX);
        r[208..212].copy_from_slice(&nodes.to_le_bytes());
        r[212..216].copy_from_slice(&makes.to_le_bytes());
        for i in 0..READS {
            let whole = u32::try_from(counts[i] - frame.counts[i]).unwrap_or(u32::MAX);
            let before = u32::try_from(first[i] - frame.counts[i]).unwrap_or(u32::MAX);
            r[216 + 4 * i..220 + 4 * i].copy_from_slice(&whole.to_le_bytes());
            let at = 216 + 4 * READS + 4 * i;
            r[at..at + 4].copy_from_slice(&before.to_le_bytes());
        }
        sink.write(Stream::Makes, &r[..]);
    });
}

/// A node's number scrambled, so a sample of every n-th hash is spread over
/// the tree rather than taking every n-th node in visit order.
fn mix(node: u64) -> u64 {
    let mut x = node.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The board as the `nodes` stream records it.
pub(crate) struct Snapshot {
    /// The six piece boards, pawns to kings, then white's and black's.
    pub(crate) boards: [u64; 8],
    /// The side to move as the engine numbers it: 0 black, 1 white.
    pub(crate) side: u8,
    pub(crate) castle: u8,
    /// The en passant square, or 64 for none.
    pub(crate) en_passant: u8,
    pub(crate) key: u64,
}

/// What a trace argument asked for.
pub struct Settings {
    pub depth: u8,
    /// Search each position to this many nodes rather than to `depth`, as
    /// `bench games` does.
    pub nodes: Option<u64>,
    /// The streams to record besides the nodes, or none for all of them.
    pub streams: Option<Vec<String>>,
    pub every: u64,
    pub window: u8,
    pub cap: u64,
    pub out: PathBuf,
    pub epd: Option<String>,
    pub positions: Vec<Position>,
}

/// What a run recorded, which prints as the manifest's summary.
pub struct Report {
    pub settings_line: String,
    pub out: PathBuf,
    pub positions: usize,
    pub nodes: u64,
    pub sampled: u64,
    pub streams: Vec<(&'static str, u64, u64)>,
    pub sites: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "trace {}", self.settings_line)?;
        writeln!(
            f,
            "{} positions, {} nodes, {} sampled, {} call sites, written to {}",
            self.positions,
            self.nodes,
            self.sampled,
            self.sites,
            self.out.display()
        )?;
        for (name, records, dropped) in &self.streams {
            writeln!(
                f,
                "{name:<8} {records:>12} records {dropped:>12} past the cap"
            )?;
        }
        Ok(())
    }
}

/// Search the suite with recording armed, under the default configuration,
/// and write the streams and the manifest into `settings.out`.
pub fn run(settings: &Settings) -> io::Result<Report> {
    let depth = settings.depth.max(1);
    let sink = Sink::create_only(
        &settings.out,
        settings.every,
        settings.window,
        settings.cap,
        settings.streams.as_deref(),
    )?;
    SINK.with(|s| *s.borrow_mut() = Some(sink));
    for (i, position) in settings.positions.iter().enumerate() {
        let board = Board::from_fen(&position.fen)
            .unwrap_or_else(|e| panic!("trace position {} does not parse: {}", position.id, e));
        SINK.with(|s| {
            if let Some(sink) = s.borrow_mut().as_mut() {
                sink.position = u16::try_from(i).expect("fewer than 65,536 positions");
                // the line a report walks is made on a copy and never taken
                // back, so its frames are dropped between positions
                sink.frames.clear();
            }
        });
        let mut engine = AlphaBeta::with_config(board, bench::TABLE_BYTES, SearchConfig::default());
        let parameters = match settings.nodes {
            Some(nodes) => SearchParameters::new(
                None,
                crate::limits::Limits::starting_now(None, Some(nodes.max(1))),
            ),
            None => SearchParameters::to_depth(depth),
        };
        engine.iterative_deepening_search(parameters, |_, _, _, _| {});
    }
    let mut sink = SINK
        .with(|s| s.borrow_mut().take())
        .expect("the sink armed above");
    for writer in &mut sink.writers {
        writer.out.flush()?;
    }
    if let Some(e) = sink.failed.take() {
        return Err(e);
    }
    let settings_line = format!(
        "{} every {} window {} cap {}{}{}",
        match settings.nodes {
            Some(nodes) => format!("nodes {nodes}"),
            None => format!("depth {depth}"),
        },
        sink.every,
        sink.window,
        sink.cap,
        settings
            .epd
            .as_ref()
            .map(|e| format!(" epd {e}"))
            .unwrap_or_default(),
        settings
            .streams
            .as_ref()
            .map(|s| format!(" streams {}", s.join(",")))
            .unwrap_or_default()
    );
    write_manifest(
        &settings.out,
        &settings_line,
        settings.positions.len(),
        &sink,
    )?;
    Ok(Report {
        settings_line,
        out: settings.out.clone(),
        positions: settings.positions.len(),
        nodes: sink.entered,
        sampled: sink.sampled,
        // the streams written: calls is left out unless it was named
        streams: Stream::ALL
            .iter()
            .filter(|&&s| sink.enabled[s as usize])
            .map(|&s| {
                let w = &sink.writers[s as usize];
                (s.name(), w.records, w.dropped)
            })
            .collect(),
        sites: sink.site_list.len(),
    })
}

/// A string as a json string's contents: a path may hold a backslash, and
/// on Windows every source path does.
fn escaped(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The manifest, as json written by hand: the format is small and fixed.
fn write_manifest(
    dir: &Path,
    settings_line: &str,
    positions: usize,
    sink: &Sink,
) -> io::Result<()> {
    let mut out = BufWriter::new(File::create(dir.join("manifest.json"))?);
    writeln!(out, "{{")?;
    writeln!(out, "  \"version\": {VERSION},")?;
    writeln!(
        out,
        "  \"engine\": \"{} {}\",",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    )?;
    writeln!(out, "  \"settings\": \"{}\",", escaped(settings_line))?;
    writeln!(out, "  \"positions\": {positions},")?;
    writeln!(out, "  \"entered\": {},", sink.entered)?;
    writeln!(out, "  \"sampled\": {},", sink.sampled)?;
    writeln!(out, "  \"history_writes\": {},", sink.history_writes)?;
    writeln!(out, "  \"history_cutoffs\": {},", sink.history_cutoffs)?;
    writeln!(out, "  \"streams\": [")?;
    for (i, stream) in Stream::ALL.iter().enumerate() {
        let w = &sink.writers[*stream as usize];
        writeln!(
            out,
            "    {{\"name\": \"{}\", \"file\": \"{}.bin\", \"width\": {}, \"records\": {}, \"dropped\": {}}}{}",
            stream.name(),
            stream.name(),
            stream.width(),
            w.records,
            w.dropped,
            if i + 1 < Stream::ALL.len() { "," } else { "" }
        )?;
    }
    writeln!(out, "  ],")?;
    writeln!(out, "  \"sites\": [")?;
    for (i, (file, line, column)) in sink.site_list.iter().enumerate() {
        writeln!(
            out,
            "    {{\"id\": {i}, \"file\": \"{}\", \"line\": {line}, \"column\": {column}}}{}",
            escaped(file),
            if i + 1 < sink.site_list.len() {
                ","
            } else {
                ""
            }
        )?;
    }
    writeln!(out, "  ]")?;
    writeln!(out, "}}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::fixtures::recording_leaves_the_search_where_it_was;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("arche-trace-{}-{}", name, std::process::id()))
    }

    #[test]
    fn recording_leaves_the_measured_search_where_it_was() {
        let dir = scratch("unmoved");
        recording_leaves_the_search_where_it_was(
            4,
            |_| {
                let sink = Sink::create(&dir, 8, 1, DEFAULT_CAP).expect("a scratch directory");
                SINK.with(|s| *s.borrow_mut() = Some(sink));
            },
            |_| {
                let sink = SINK
                    .with(|s| s.borrow_mut().take())
                    .expect("the sink comes back");
                sink.writers.iter().map(|w| w.records as usize).sum()
            },
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_string_escapes_what_json_would_misread() {
        assert_eq!(escaped(r"C:\suite\a.epd"), r"C:\\suite\\a.epd");
        assert_eq!(escaped("say \"x\"\n"), r#"say \"x\"\u000a"#);
    }

    #[test]
    fn a_run_writes_each_stream_it_records_and_a_manifest() {
        let dir = scratch("streams");
        let settings = Settings {
            depth: 4,
            nodes: None,
            streams: None,
            every: 4,
            window: 1,
            cap: DEFAULT_CAP,
            out: dir.clone(),
            epd: None,
            positions: crate::recorder::fixtures::suite(),
        };
        let report = run(&settings).expect("the run writes its files");
        for (name, records, _) in &report.streams {
            assert!(*records > 0, "{name} recorded nothing");
            let bytes = std::fs::metadata(dir.join(format!("{name}.bin")))
                .expect("the stream's file")
                .len();
            let width = Stream::ALL
                .iter()
                .find(|s| s.name() == *name)
                .expect("a stream")
                .width() as u64;
            assert_eq!(bytes, 24 + records * width, "{name}");
        }
        assert!(dir.join("manifest.json").exists());
        assert!(report.sites > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
