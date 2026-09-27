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
use crate::misc::Piece;
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
}

impl Stream {
    const ALL: [Stream; 5] = [
        Stream::Nodes,
        Stream::Sliders,
        Stream::Attacks,
        Stream::Swaps,
        Stream::Lists,
    ];

    fn name(self) -> &'static str {
        match self {
            Stream::Nodes => "nodes",
            Stream::Sliders => "sliders",
            Stream::Attacks => "attacks",
            Stream::Swaps => "swaps",
            Stream::Lists => "lists",
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
        }
    }
}

/// The node being searched, as the hooks see it.
#[derive(Clone, Copy, Default)]
struct Context {
    node: u64,
    /// The move list the node is working through, numbered when `order`
    /// was asked for it; zero before.
    list: u64,
    kind: u8,
    sampled: bool,
    /// How many more plies below this node are sampled because it is.
    left: u8,
}

thread_local! {
    static CONTEXT: Cell<Context> = const { Cell::new(Context { node: 0, list: 0, kind: 0, sampled: false, left: 0 }) };
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
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
    sampled: u64,
    writers: Vec<Writer>,
    sites: HashMap<(&'static str, u32, u32), u16>,
    site_list: Vec<(&'static str, u32, u32)>,
    failed: Option<io::Error>,
}

impl Sink {
    fn create(dir: &Path, every: u64, window: u8, cap: u64) -> io::Result<Self> {
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
            sampled: 0,
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
    if !context.sampled {
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
    if !context.sampled {
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

/// A piece as the streams number it, pawn 0 to king 5, and 6 for none.
fn piece_code(piece: Option<Piece>) -> u8 {
    piece.map_or(6, |p| p as u8)
}

/// A promotion as the streams number it: 0 for none, then knight 1 to
/// queen 4.
fn promote_code(m: &Play) -> u8 {
    m.promote.map_or(0, |p| p as u8 + 1)
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
    let sink = Sink::create(&settings.out, settings.every, settings.window, settings.cap)?;
    SINK.with(|s| *s.borrow_mut() = Some(sink));
    for (i, position) in settings.positions.iter().enumerate() {
        let board = Board::from_fen(&position.fen)
            .unwrap_or_else(|e| panic!("trace position {} does not parse: {}", position.id, e));
        SINK.with(|s| {
            if let Some(sink) = s.borrow_mut().as_mut() {
                sink.position = u16::try_from(i).expect("fewer than 65,536 positions");
            }
        });
        let mut engine = AlphaBeta::with_config(board, bench::TABLE_BYTES, SearchConfig::default());
        engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
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
        "depth {} every {} window {} cap {}{}",
        depth,
        sink.every,
        sink.window,
        sink.cap,
        settings
            .epd
            .as_ref()
            .map(|e| format!(" epd {e}"))
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
        streams: Stream::ALL
            .iter()
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
    fn a_run_writes_every_stream_and_a_manifest() {
        let dir = scratch("streams");
        let settings = Settings {
            depth: 4,
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
