use crate::Game;
use crate::board::{Board, MoveList};
use crate::misc::{Color, Score};
use crate::play::Play;
use std::fmt;
use std::mem;
use std::time;

const CHECKMATE_SCORE: Score = 30_000;
const MAX_DEPTH: u8 = 20;
const DEFAULT_TABLE_BYTES: usize = 256 * 1024 * 1024;
// Any score this close to CHECKMATE_SCORE is a forced mate. Regular evals are
// bounded by the material on the board, which cannot come near it.
const CHECKMATE_THRESHOLD: Score = CHECKMATE_SCORE - 1000;

/// Convert a score to its transposition table form. Mate scores are stored
/// relative to the node they are stored at (plies-to-mate from this node)
/// rather than relative to the root of the search, so that they remain correct
/// when the entry is reused at a different distance from the root.
fn score_to_tt(score: Score, line_ply: usize) -> Score {
    if score > CHECKMATE_THRESHOLD {
        score + line_ply as Score
    } else if score < -CHECKMATE_THRESHOLD {
        score - line_ply as Score
    } else {
        score
    }
}

/// The inverse of score_to_tt: convert a stored mate score back to being
/// relative to the root of the current search.
fn score_from_tt(score: Score, line_ply: usize) -> Score {
    if score > CHECKMATE_THRESHOLD {
        score - line_ply as Score
    } else if score < -CHECKMATE_THRESHOLD {
        score + line_ply as Score
    } else {
        score
    }
}

/// What the protocol interface asks of an engine: positions in, answers out.
/// How an implementation searches is its own business, which is why the
/// deepening loop is required here rather than provided.
pub trait Engine {
    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String>;

    /// Forget what was learned from the game just finished. Stored scores do not
    /// account for repetition or the fifty move counter, so a position that
    /// comes up again in a new game would otherwise be scored from a line that
    /// no longer applies to it.
    fn new_game(&mut self);

    fn make_move_str(&mut self, play: &str) -> bool;

    fn display_board(&self);

    fn perft(&mut self, depth: u8) -> u64;

    fn active_color(&self) -> Color;

    /// Search each depth in turn until one is the last to finish. The caller
    /// hears about every completed iteration through on_depth, which is where
    /// a protocol adapter reports progress from; the library itself never
    /// prints. A result's node count covers the whole deepening so far, not
    /// the one iteration, which is what the uci info convention expects and
    /// what makes it divisible by the time since the search began.
    fn iterative_deepening_search(
        &mut self,
        search_options: SearchParameters,
        on_depth: impl FnMut(u8, &SearchResult, PvLine),
    ) -> SearchOutcome;
}

pub struct SearchParameters {
    pub depth: Option<u8>,
    pub search_duration: Option<time::Duration>,
    pub start_time: time::Instant,
}

impl Default for SearchParameters {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchParameters {
    pub fn new() -> Self {
        Self {
            depth: None,
            search_duration: None,
            start_time: time::Instant::now(),
        }
    }

    pub fn new_with_depth(depth: u8) -> Self {
        Self {
            depth: Some(depth),
            search_duration: None,
            start_time: time::Instant::now(),
        }
    }
}

pub struct AlphaBeta {
    pub board: Board,
    nodes: u64,
    moves: HashTable,
    selective_depth: u8,
    // search state
    /// Whether the value the last search call returned was draw tainted. The
    /// search is depth first and single threaded, so one flag threads the taint
    /// up without changing every return type.
    tainted: bool,
    pub ghi: GhiCounters,
    start_time: time::Instant,
    search_duration: Option<time::Duration>,
}

impl AlphaBeta {
    pub fn with_table_bytes(board: Board, bytes: usize) -> Self {
        Self {
            board,
            nodes: 0,
            moves: HashTable::with_capacity_bytes(bytes),
            selective_depth: 0,
            tainted: false,
            ghi: GhiCounters::default(),
            start_time: time::Instant::now(),
            search_duration: None,
        }
    }

    fn eval(&self) -> Score {
        self.board.eval()
    }

    pub fn clear_cache(&mut self) {
        self.moves.clear();
    }

    /// Cooperative deadline check, polled every few thousand nodes so the
    /// clock is not read on every one of them.
    fn poll_deadline(&self) -> Result<(), Aborted> {
        if self.nodes % 3000 == 0 {
            if let Some(search_time) = self.search_duration {
                if self.start_time.elapsed() >= search_time {
                    return Err(Aborted);
                }
            }
        }
        Ok(())
    }

    fn result_for(&self, best_move: Play, score: Score) -> SearchResult {
        SearchResult {
            nodes: self.nodes,
            score,
            selective_depth: self.selective_depth,
            best_move,
        }
    }

    /// MVV-LVA order, with the table's move for this position, if there is
    /// one, ahead of everything else.
    ///
    /// The search may already have played that move without generating, in
    /// which case it skips it here. The bonus still earns its keep: a table
    /// move it declined to play early was never searched, so it is still in
    /// this list and still has to be the first one tried.
    fn order_moves(&self, moves: &mut MoveList, pv_play: Option<Play>) {
        moves.sort_by_cached_key(|m| {
            let mut score = m.mvv_lva(&self.board);
            if pv_play == Some(*m) {
                score += 100_000;
            }
            -score
        });
    }

    fn quiescence(&mut self, mut alpha: Score, beta: Score) -> Result<Score, Aborted> {
        // quiescence looks at captures and promotions, and evasions when in
        // check, and never checks for a repetition: a capture cannot repeat a
        // position, and the quiet moves here are evasions, so a cycle needs a
        // line of nothing but mutual quiet checks, which MAX_DEPTH bounds and
        // real positions do not sustain
        self.tainted = false;
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        if self.board.line_ply >= MAX_DEPTH.into() {
            return Ok(self.eval());
        }

        self.poll_deadline()?;
        self.nodes += 1;

        // Standing pat is declining to move, which only the side not in check
        // may do: the static eval is no floor for a side that has to get out
        // of check and may have no quiet way to. The full search never enters
        // here in check, the check extension searches those nodes full width,
        // so a check seen here was delivered by a capture searched here.
        let in_check = self.board.in_check();
        if !in_check {
            let score = self.eval();
            if score >= beta {
                return Ok(beta);
            } else if score >= alpha {
                alpha = score;
            }
        }

        let mut best_move: Option<Play> = None;
        let old_alpha = alpha;
        let pv_play = self.moves.get(self.board.key).map(|pv| pv.play);
        // in check the position is not quiet whatever the material says, so
        // every evasion is searched, quiet or not. Most of what full width
        // generation returns cannot answer a check and would only be refused
        // by make_move, so it is dropped before it is even sorted
        let mut moves = if in_check {
            let mut moves = self.board.generate_moves();
            self.board.retain_evasions(&mut moves);
            moves
        } else {
            self.board.generate_captures()
        };
        self.order_moves(&mut moves, pv_play);

        let mut found_legal_move = false;
        for m in &moves {
            if self.board.make_move(m) {
                found_legal_move = true;
                // undo before an abort can propagate, or the board would keep
                // the aborted line
                let result = self.quiescence(-beta, -alpha);
                self.board.undo_move();
                let score = -result?;
                if score > alpha {
                    if score >= beta {
                        return Ok(beta);
                    }
                    alpha = score;
                    best_move = Some(*m);
                }
            }
        }

        if in_check && !found_legal_move {
            // checkmate, at the end of a capture sequence: report it as the
            // search does, so the line that forces it reads as the mate it is
            return Ok(-CHECKMATE_SCORE + (self.board.line_ply as Score));
        }

        if alpha != old_alpha {
            // depth zero and an Ordering bound: never read back in place of
            // evaluating a position, only to sort its moves
            self.moves.set(
                self.board.key,
                Pv {
                    play: best_move.unwrap(),
                    score: score_to_tt(alpha, self.board.line_ply),
                    depth: 0,
                    bound: Bound::Ordering,
                    tainted: false,
                    ply: self.board.ply as u16,
                },
            );
        }
        Ok(alpha)
    }

    /// Look up the current position in the transposition table.
    ///
    /// Returns the stored best move (if any) which is always safe to use for
    /// move ordering, and a score when the stored entry is deep enough and its
    /// bound allows a cutoff at the current alpha/beta window.
    fn get_transposition(
        &mut self,
        key: u64,
        alpha: Score,
        beta: Score,
        depth: u8,
    ) -> (Option<Play>, Option<Score>) {
        // copied out so the counters below are not borrowing the table
        let Some(pv) = self.moves.get(key).copied() else {
            return (None, None);
        };
        if pv.depth >= depth {
            let score = score_from_tt(pv.score, self.board.line_ply);
            let cuts = match pv.bound {
                Bound::Exact => true,
                Bound::Upper => score <= alpha,
                Bound::Lower => score >= beta,
                Bound::Ordering => false,
            };
            if cuts && REFUSE_TAINTED_CUTOFFS && pv.tainted {
                // the stored draw was reachable by the path that stored it and
                // may not be reachable by this one, so the move is still worth
                // ordering by but the score is not worth trusting
                return (Some(pv.play), None);
            }
            if cuts {
                self.ghi.score_cutoffs += 1;
                self.ghi.tainted_score_cutoffs += u64::from(pv.tainted);
                // whatever the stored score depended on, this frame now depends
                // on too
                self.tainted = pv.tainted;
                return (Some(pv.play), Some(score));
            }
        }
        (Some(pv.play), None)
    }

    fn alpha_beta(
        &mut self,
        mut alpha: Score,
        beta: Score,
        mut depth: u8,
    ) -> Result<Score, Aborted> {
        self.poll_deadline()?;
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        self.nodes += 1;

        // every node here sits below the root, which search() owns: a
        // repetition there is not a finished game because the engine still has
        // to move, but from here on it is a draw either side can take
        let in_check = self.board.in_check();
        if self.board.fifty_move_rule >= 100 {
            // a mate delivered by the hundredth half move is a mate: the game
            // ends on it, before the side mated has a move on which to claim
            // the draw. Only asked here and not of a repetition, which cannot
            // be a mate since the position would have ended the game the
            // first time it came up, so the cost stays off the lines that
            // repeat
            if in_check && !self.board.has_legal_move() {
                self.tainted = false;
                return Ok(-CHECKMATE_SCORE + (self.board.line_ply as Score));
            }
            // where the taint starts: the draw is true of the path that
            // reached this position, not of the position itself
            self.tainted = true;
            return Ok(0);
        }
        if self.board.has_repeated() {
            self.tainted = true;
            return Ok(0);
        }
        let mut node_tainted = false;
        if in_check {
            depth += 1;
        }

        if depth == 0 {
            return self.quiescence(alpha, beta);
        }

        let old_alpha = alpha;
        let mut found_legal_move = false;
        let mut best_move: Option<Play> = None;
        let (pv_play, tt_score) = self.get_transposition(self.board.key, alpha, beta, depth);
        if let Some(tt_score) = tt_score {
            return Ok(tt_score);
        }

        // The table's move sorts ahead of everything else below, and when there
        // is one it takes the cutoff nine times in ten. Searching it before
        // generating means the nodes it cuts never generate or sort at all.
        // The order is the one the sort would have produced either way, so the
        // tree searched is unchanged.
        let mut tt_tried: Option<Play> = None;
        if let Some(tt) = pv_play {
            if self.board.is_pseudo_legal(&tt) {
                tt_tried = Some(tt);
                if self.board.make_move(&tt) {
                    found_legal_move = true;
                    let result = self.alpha_beta(-beta, -alpha, depth - 1);
                    self.board.undo_move();
                    // the table's move is searched before the rest are even
                    // generated, so it taints this node the same way any other
                    // child would
                    node_tainted |= self.tainted;
                    let tt_score = -result?;
                    if tt_score > alpha {
                        best_move = Some(tt);
                        if tt_score >= beta {
                            self.moves.set(
                                self.board.key,
                                Pv {
                                    play: tt,
                                    depth,
                                    score: score_to_tt(beta, self.board.line_ply),
                                    bound: Bound::Lower,
                                    ply: self.board.ply as u16,
                                    tainted: node_tainted,
                                },
                            );
                            self.ghi.stores += 1;
                            self.ghi.tainted_stores += u64::from(node_tainted);
                            self.tainted = node_tainted;
                            return Ok(beta);
                        }
                        alpha = tt_score;
                    }
                }
            }
        }

        let mut moves = self.board.generate_moves();
        if in_check {
            // most of the list cannot answer the check and would only be
            // refused by make_move; drop it before it is even sorted
            self.board.retain_evasions(&mut moves);
        }
        self.order_moves(&mut moves, pv_play);

        for m in &moves {
            if tt_tried == Some(*m) {
                continue;
            }
            if self.board.make_move(m) {
                found_legal_move = true;
                // undo before an abort can propagate, or the board would keep
                // the aborted line. Propagating also keeps the meaningless
                // score of an aborted frame away from the stores below.
                let result = self.alpha_beta(-beta, -alpha, depth - 1);
                self.board.undo_move();
                // a value built from a tainted child is tainted, whether or not
                // it turns out to be the best one here
                node_tainted |= self.tainted;
                let score = -result?;
                if score > alpha {
                    best_move = Some(*m);
                    if score >= beta {
                        self.moves.set(
                            self.board.key,
                            Pv {
                                play: *m,
                                depth,
                                score: score_to_tt(beta, self.board.line_ply),
                                bound: Bound::Lower,
                                ply: self.board.ply as u16,
                                tainted: node_tainted,
                            },
                        );
                        self.ghi.stores += 1;
                        self.ghi.tainted_stores += u64::from(node_tainted);
                        self.tainted = node_tainted;
                        return Ok(beta);
                    }
                    alpha = score;
                }
            }
        }

        if !found_legal_move {
            // mate and stalemate are properties of the position, not of the
            // path that reached it
            self.tainted = false;
            if in_check {
                return Ok(-CHECKMATE_SCORE + (self.board.line_ply as Score));
            }
            return Ok(0);
        }

        if alpha != old_alpha {
            self.moves.set(
                self.board.key,
                Pv {
                    play: best_move.unwrap(),
                    depth,
                    score: score_to_tt(alpha, self.board.line_ply),
                    bound: Bound::Exact,
                    ply: self.board.ply as u16,
                    tainted: node_tainted,
                },
            );
            self.ghi.stores += 1;
            self.ghi.tainted_stores += u64::from(node_tainted);
        }
        self.tainted = node_tainted;
        Ok(alpha)
    }

    pub fn new(board: Board) -> Self {
        AlphaBeta::with_table_bytes(board, DEFAULT_TABLE_BYTES)
    }

    pub fn configure(
        &mut self,
        start_time: time::Instant,
        search_duration: Option<time::Duration>,
    ) {
        self.start_time = start_time;
        self.search_duration = search_duration;
    }

    /// The root loop: the one node whose answer must include a play, which is
    /// why it runs here rather than in alpha_beta. The root probes the
    /// transposition table to order moves and stores its entry when done, but
    /// never takes a stored score in place of searching: a stored score can
    /// come from a line whose repetition and fifty move context differ from
    /// the game being played, and the answer must not depend on one.
    pub fn search(&mut self, depth: u8) -> SearchOutcome {
        self.search_within(depth, self.search_duration)
    }

    /// One fixed depth search under the limits given for it. `search` passes
    /// the configured deadline through; the deepening loop decides per
    /// iteration, which is how depth one runs with none.
    fn search_within(&mut self, depth: u8, deadline: Option<time::Duration>) -> SearchOutcome {
        self.search_duration = deadline;
        self.nodes = 0;
        self.selective_depth = depth;
        self.board.line_ply = 0;

        // the game is already drawn, there is no move to look for
        if self.board.fifty_move_rule >= 100 {
            return SearchOutcome::GameOver;
        }

        if self.poll_deadline().is_err() {
            return SearchOutcome::Aborted(None);
        }
        self.nodes += 1;

        let mut depth = depth;
        if self.board.in_check() {
            depth += 1;
        }

        let mut alpha = Score::MIN + 1;
        let beta = Score::MAX - 1;
        let mut best: Option<Play> = None;
        let mut found_legal_move = false;
        let mut root_tainted = false;

        let pv_play = self.moves.get(self.board.key).map(|pv| pv.play);
        let mut moves = self.board.generate_moves();
        self.order_moves(&mut moves, pv_play);

        for m in &moves {
            if self.board.make_move(m) {
                found_legal_move = true;
                // undo before an abort can propagate, or the board would keep
                // the aborted line
                let result = self.alpha_beta(-beta, -alpha, depth - 1);
                self.board.undo_move();
                match result {
                    Err(Aborted) => {
                        return SearchOutcome::Aborted(
                            best.map(|play| self.result_for(play, alpha)),
                        );
                    }
                    Ok(s) => {
                        root_tainted |= self.tainted;
                        let score = -s;
                        if score > alpha {
                            alpha = score;
                            best = Some(*m);
                        }
                    }
                }
            }
        }

        if !found_legal_move {
            // checkmate or stalemate: either way there is nothing to play
            return SearchOutcome::GameOver;
        }

        let play = best.expect("any legal move's score beats the opening alpha of Score::MIN + 1");
        // stored past the depth contest: this is the move about to be
        // answered, and the line reported for it is read back from this slot
        self.moves.set_always(
            self.board.key,
            Pv {
                play,
                depth,
                score: score_to_tt(alpha, self.board.line_ply),
                bound: Bound::Exact,
                ply: self.board.ply as u16,
                tainted: root_tainted,
            },
        );
        self.ghi.stores += 1;
        self.ghi.tainted_stores += u64::from(root_tainted);
        SearchOutcome::Complete(self.result_for(play, alpha))
    }

    /// Replay the line the table holds on a copy of the board, one stored move
    /// at a time. Walking the positions rather than following a key from one
    /// entry to the next is what lets the line be checked as it is built: the
    /// board says whether a stored move is legal here, and whether the line has
    /// reached a position it would be a draw to play on from.
    pub fn pv_line(&self) -> PvLine {
        let mut line = Vec::new();
        // Board is Copy, so the search's own board is untouched by this.
        let mut board = self.board;
        while line.len() < MAX_DEPTH as usize {
            let Some(pv) = self.moves.get(board.key) else {
                break;
            };
            if matches!(pv.bound, Bound::Ordering) {
                // written by quiescence, which looks at captures and
                // promotions alone, so the move is fit for ordering the next
                // search and not for telling anyone what the engine intends
                // to play
                break;
            }
            let play = pv.play;
            // a probe compares the whole key, so what still gets through is
            // another position which hashed to the same one. Its move belongs
            // to that position, and playing it here would print a line the
            // rules do not allow
            if !board.generate_moves().contains(&play) {
                break;
            }
            if !board.make_move(&play) {
                break;
            }
            line.push(play);
            // the line is a draw from here, so whatever the table says comes
            // next is a continuation that would never be played
            if board.fifty_move_rule >= 100 || board.has_repeated() {
                break;
            }
        }
        PvLine { line }
    }
}

impl Engine for AlphaBeta {
    fn perft(&mut self, depth: u8) -> u64 {
        self.board.perft(depth)
    }

    fn active_color(&self) -> Color {
        self.board.active_color
    }

    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String> {
        self.nodes = 0;
        self.board = Board::from_fen(fen_string)?;
        Ok(())
    }

    fn new_game(&mut self) {
        self.clear_cache();
    }

    fn iterative_deepening_search(
        &mut self,
        search_options: SearchParameters,
        mut on_depth: impl FnMut(u8, &SearchResult, PvLine),
    ) -> SearchOutcome {
        let mut best: Option<SearchResult> = None;
        // each search() counts its own nodes, so the deepening totals them:
        // what leaves here describes the whole search so far, which is the
        // count the time elapsed so far can honestly divide
        let mut total_nodes: u64 = 0;
        let max_depth = match search_options.depth {
            Some(depth) => depth,
            None => MAX_DEPTH,
        };
        self.configure(search_options.start_time, search_options.search_duration);

        for depth in 1..=max_depth {
            // a limit is only armed once a completed iteration is in hand:
            // whatever the clock says the answer has to be a legal move, and
            // depth one is the few dozen nodes that make sure there is one
            let deadline = if best.is_some() {
                search_options.search_duration
            } else {
                None
            };
            match self.search_within(depth, deadline) {
                SearchOutcome::Aborted(partial) => {
                    // A completed shallower iteration outranks the interrupted
                    // one's best-so-far, which may be a fail high that never
                    // got re-searched. The partial only fills in when depth 1
                    // itself ran out of time.
                    return SearchOutcome::Aborted(best.or(partial.map(|mut result| {
                        result.nodes += total_nodes;
                        result
                    })));
                }
                SearchOutcome::GameOver => {
                    // checkmate, stalemate or a rule draw: deeper searches
                    // cannot change it, so don't run them
                    return SearchOutcome::GameOver;
                }
                SearchOutcome::Complete(mut result) => {
                    total_nodes += result.nodes;
                    result.nodes = total_nodes;
                    let pv = self.pv_line();
                    // the root's entry was just stored past any leftover, so
                    // the line the table tells opens with the move answered
                    debug_assert_eq!(
                        pv.line.first(),
                        Some(&result.best_move),
                        "the reported line disagrees with the best move"
                    );
                    on_depth(depth, &result, pv);
                    best = Some(result);
                }
            }
        }
        match best {
            Some(result) => SearchOutcome::Complete(result),
            // a depth of zero runs no iterations, so there is nothing to
            // report beyond that no move was looked for
            None => SearchOutcome::Aborted(None),
        }
    }

    fn make_move_str(&mut self, play: &str) -> bool {
        for p in self.board.generate_moves() {
            let play_str = format!("{}", p).to_lowercase();
            if play == play_str {
                return self.board.make_move(&p); // TODO change this to return Result
            };
        }
        false
    }

    fn display_board(&self) {
        println!("{}", self.board);
    }
}

/// Whether to refuse a score from a draw tainted entry, trusting only its move.
///
/// On. A tainted score describes the path that stored it rather than the
/// position, so a search arriving another way can read a draw it cannot
/// actually reach. Refusing costs, over the pinned positions at their pinned
/// depths, nothing at all in kiwipete, promotions and the middlegame, which
/// have no tainted cutoffs to refuse, +0.037% in the opening and +2.970% in the
/// pawn endgame, for +0.617% overall.
const REFUSE_TAINTED_CUTOFFS: bool = true;

/// How often the transposition table hands back a score that depended on the
/// path taken rather than on the position, which is the graph history
/// interaction error every engine carries and none of them measure.
#[derive(Copy, Clone, Debug, Default)]
pub struct GhiCounters {
    /// Entries stored carrying a draw tainted score.
    pub tainted_stores: u64,
    /// Entries stored in total.
    pub stores: u64,
    /// Probes that returned a score, cutting the search off.
    pub score_cutoffs: u64,
    /// Probes that returned a tainted score, which is the error itself: the
    /// stored draw was reachable by the path that stored it and may not be
    /// reachable by this one.
    pub tainted_score_cutoffs: u64,
}

#[derive(Copy, Clone, Debug)]
struct Pv {
    play: Play,
    score: Score,
    /// True if the score flowed from a repetition or fifty move draw somewhere
    /// below it, so it describes the path taken to this position and not the
    /// position itself. See docs on graph history interaction.
    tainted: bool,
    // a depth cannot exceed MAX_DEPTH and a ply is bounded by the history
    // array, so neither needs a word. Both sit in the padding the key leaves
    // behind, which is why widening ply to u16 costs nothing.
    depth: u8,
    bound: Bound,
    ply: u16,
}

/// What the stored score means: the searched window decides whether a score
/// is the truth, a ceiling or a floor, and a reader can only use it for a
/// cutoff its kind allows.
#[derive(Copy, Clone, Debug)]
enum Bound {
    Exact,
    // fail low nodes are not stored yet, see the known issues in the readme
    #[allow(dead_code)]
    Upper,
    Lower,
    Ordering,
}

/// A slot in the table. The key is kept alongside the entry so that a probe can
/// tell a real hit from another position landing on the same index.
type Entry = Option<(Pv, u64)>;

#[derive(Debug)]
struct HashTable {
    table: Vec<Entry>,
}

impl HashTable {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            table: vec![None; capacity],
        }
    }

    fn clear(&mut self) {
        self.table.fill(None);
    }

    fn with_capacity_bytes(bytes: usize) -> Self {
        // ask for the size of what is actually stored. Adding up the fields
        // gives the same answer today only because Pv happens to need no
        // padding on the end of it, and would quietly over allocate if that
        // stopped being true.
        Self::with_capacity(bytes / mem::size_of::<Entry>())
    }

    #[inline]
    fn index_for(&self, key: u64) -> usize {
        // multiply-shift: maps key uniformly onto 0..len without a 64 bit
        // division on every probe
        (((key as u128) * (self.table.len() as u128)) >> 64) as usize
    }

    fn get(&self, key: u64) -> Option<&Pv> {
        let index = self.index_for(key);
        if let Some((pv, k)) = &self.table[index] {
            if *k == key {
                return Some(pv);
            }
        }
        None
    }

    fn set(&mut self, key: u64, pv: Pv) {
        let index = self.index_for(key);
        if let Some((old_pv, old_key)) = self.table[index] {
            // entries left over from an earlier point in the game are always replaced
            let stale = (pv.ply as isize - old_pv.ply as isize) > (MAX_DEPTH as isize + 3);
            if !stale {
                if pv.depth < old_pv.depth {
                    return;
                }
                if pv.depth == old_pv.depth
                    && old_key == key
                    && matches!(old_pv.bound, Bound::Exact)
                    && !matches!(pv.bound, Bound::Exact)
                {
                    return;
                }
            }
        }
        self.table[index] = Some((pv, key));
    }

    /// Store without the depth contest above. The root's end-of-iteration
    /// entry is for this: it names the move the search is about to answer
    /// with, and the reported line is read back from its slot, so an entry a
    /// deeper search left there earlier in the game must not outrank it.
    /// When one did, the engine answered one move while its line opened with
    /// another, which is a lie the match tools flag. The leftover's extra
    /// depth is no loss: its score describes repetition and fifty move
    /// context the game has since moved past.
    fn set_always(&mut self, key: u64, pv: Pv) {
        let index = self.index_for(key);
        self.table[index] = Some((pv, key));
    }
}

pub struct PvLine {
    line: Vec<Play>,
}

impl PvLine {
    /// A line built by hand, which is how a protocol adapter's tests pin the
    /// format of a reported line without running a search.
    pub fn new(line: Vec<Play>) -> Self {
        Self { line }
    }
}

impl fmt::Display for PvLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let moves: Vec<String> = self.line.iter().map(|p| p.to_string()).collect();
        write!(f, "{}", moves.join(" "))
    }
}

/// The verdict of one fixed-depth search of the root.
#[derive(Debug)]
pub enum SearchOutcome {
    /// The search finished the requested depth; its result can be trusted
    /// without checking anything else.
    Complete(SearchResult),
    /// The root has no play to make: checkmate, stalemate, or a rule draw.
    /// Searching deeper cannot change it.
    GameOver,
    /// The deadline arrived partway through, carrying a best-so-far when the
    /// root got far enough to have one. Weaker than any Complete result: the
    /// play may be a fail high that never got re-searched.
    Aborted(Option<SearchResult>),
}

/// The search hit its deadline and unwound without finishing. The score of an
/// aborted frame is meaningless, and returning this instead of a score is what
/// keeps it out of the transposition table: propagation with `?` never reaches
/// the stores.
struct Aborted;

#[derive(Debug)]
pub struct SearchResult {
    pub nodes: u64,          // The number of positions visited during the search
    pub selective_depth: u8, // Selective search depth in plies
    pub best_move: Play,     // The best move found as part of the search
    pub score: Score,        // The estimated score for the best move if played
}

impl SearchResult {
    pub fn checkmate_in(&self) -> Option<Score> {
        if self.score.abs() > CHECKMATE_THRESHOLD {
            let mut mate = (CHECKMATE_SCORE - self.score.abs() + 1) / 2;
            if self.score < 0 {
                mate = -mate;
            };
            return Some(mate);
        }
        None
    }
}

#[cfg(test)]
mod search {
    use super::AlphaBeta;
    use super::Board;
    use super::Engine;
    use super::Game;
    use super::{Bound, Play, Pv, SearchOutcome, SearchParameters, SearchResult};
    use crate::board::{fens, play_named};
    use pretty_assertions::assert_eq;
    use std::time;

    /// The default table is 256MB, which is the right size to play with and
    /// the wrong size to test with: one per test dominated both the memory and
    /// the run time of the suite. This is still far larger than anything here
    /// searches deeply enough to fill.
    const TABLE_BYTES: usize = 16 * 1024 * 1024;

    fn engine(board: Board) -> AlphaBeta {
        AlphaBeta::with_table_bytes(board, TABLE_BYTES)
    }

    /// A tactical middlegame the cache tests search over and over: sharp
    /// enough that a wrongly reused score would move the verdict.
    const SHARP_MIDDLEGAME: &str = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";

    /// Unwrap the outcome these tests expect: a search that ran to the depth
    /// asked of it.
    fn completed(outcome: SearchOutcome) -> SearchResult {
        match outcome {
            SearchOutcome::Complete(result) => result,
            other => panic!("expected a completed search, got {:?}", other),
        }
    }

    /// An entry of the kind a completed search leaves behind.
    fn searched(play: Play, ply: usize) -> Pv {
        Pv {
            play,
            score: 0,
            depth: 5,
            bound: Bound::Exact,
            ply: ply as u16,
            tainted: false,
        }
    }

    #[test]
    fn a_losing_position_is_still_losing_with_a_warm_table() {
        // This is a losing position but running a search on a previous position then the losing
        // position seems to cause hash/cache collisions in some cases.
        let game =
            Board::from_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(7));
        assert!(
            result.score < -800,
            "expect bad score (first) got {}",
            result.score
        );

        let game = Board::from_fen(SHARP_MIDDLEGAME).unwrap();
        let mut e = engine(game);
        completed(e.search(7));
        let _ = e.parse_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16");
        let result = completed(e.search(7));
        assert!(result.score < -800, "expect bad score got {}", result.score);
    }

    #[test]
    fn the_reported_line_opens_with_the_move_actually_answered() {
        // A deeper entry for the root position, left by an earlier search of
        // it, used to win the depth contest against the root's own store:
        // the table then told a line opening with the leftover's move while
        // bestmove answered the fresh one, and the two disagreed in front of
        // whatever was relaying the search. Here the queen hangs, so a fresh
        // search must answer with the capture, while the planted leftover
        // claims a quiet king move from a depth no shallow search can beat.
        let game = Board::from_fen("k7/8/8/3q4/8/8/3R4/K7 w - - 0 1").unwrap();
        let mut e = engine(game);
        let quiet = play_named(&e.board, "a1b1");
        e.moves.set(
            e.board.key,
            Pv {
                depth: 14,
                ..searched(quiet, e.board.ply)
            },
        );
        let result = completed(e.search(2));
        let takes = play_named(&e.board, "d2d5");
        assert_eq!(result.best_move, takes);
        assert_eq!(e.pv_line().line.first(), Some(&takes));
    }

    #[test]
    fn checkmate_in_two_is_found_for_white() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(2));
        assert_eq!(format!("{}", result.best_move), "g3g6");
    }

    #[test]
    fn the_mate_distance_survives_a_deeper_warm_search() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(2));
        // searching again deeper with a warm cache reuses mate scores stored
        // at different plies, the reported mate distance must not change
        let result = completed(e.search(6));
        assert_eq!(result.checkmate_in(), Some(2));
        assert_eq!(format!("{}", result.best_move), "g3g6");
    }

    #[test]
    fn checkmate_in_one_is_found_for_black() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbNQp/3pN3/2pP4/2P5/PPB4P/R4RK1 b - - 1 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(-1));
    }

    #[test]
    fn quiescence_does_not_stand_pat_out_of_a_mate() {
        // the queen on a8 hangs, and taking it is losing: Rxa8 Nxf2 is mate,
        // the knight covered by nothing and capturable by nothing, the king
        // shut in by its own rook and pawns. The mate arrives by a capture
        // two plies into quiescence, where the mated node used to stand pat
        // as though it could decline to move: the capture only cost a pawn,
        // so taking the queen read as winning it, and the search took it.
        let game = Board::from_fen("q7/7k/8/8/6n1/8/5PPP/R5RK w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_ne!(format!("{}", result.best_move), "a1a8");
    }

    #[test]
    fn the_horizon_sees_a_promotion_coming() {
        // the rook can win the knight across the board or take the pawn one
        // step from promoting. The pawn's push captures nothing, so
        // quiescence used not to generate it: the knight looked free to take
        // and the queen appeared only after the horizon. Taking the pawn is
        // the move.
        let game = Board::from_fen("4k3/8/8/R5n1/8/8/p5K1/8 w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_eq!(format!("{}", result.best_move), "a5a2");
    }

    #[test]
    fn a_shallow_search_still_sees_the_recapture() {
        // the queen can take a pawn which another pawn defends. A depth one
        // search ends on the capture, so only quiescence sees the recapture
        // that loses the queen for it. Shallow searches used to skip
        // quiescence and walk into it.
        let game = Board::from_fen("4k3/8/3p4/2p5/8/2Q5/8/4K3 w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_ne!(format!("{}", result.best_move), "c3c5");
    }

    #[test]
    fn deepening_through_shallow_depths_matches_a_cold_search() {
        // iterations shallower than four used to store scores whose leaves
        // were never quiesced, and deeper iterations then read those entries
        // back as if they had been: the same depth then answered differently
        // warm than cold. On the promotions position the difference was
        // visible at the root, deepening promoted to a queen where a cold
        // search of the same depth chose the rook.
        // a_warm_cache_matches_a_cold_search cannot see any of this because
        // it searches each depth directly rather than deepening to it.
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        ];
        for fen in fens {
            let mut cold = engine(Board::from_fen(fen).unwrap());
            let expected = completed(cold.search(5));
            let mut warm = engine(Board::from_fen(fen).unwrap());
            let result = (1..=5)
                .map(|depth| completed(warm.search(depth)))
                .next_back()
                .unwrap();
            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
        }
    }

    #[test]
    fn a_losing_side_plays_for_the_fifty_move_draw() {
        // white is down material in this position so should play for fifty move draw
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    #[test]
    fn a_triggered_fifty_move_rule_is_game_over() {
        // The fifty move rule has been triggered - the game is already drawn,
        // there is no move to look for
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 100 112").unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(3), SearchOutcome::GameOver));
    }

    #[test]
    fn a_mate_on_the_hundredth_half_move_is_a_mate_not_a_draw() {
        // Rh8 mates, and it is the hundredth half move since anything
        // irreversible. Checkmate ends the game on the spot, before the mated
        // side has a move on which to claim the draw, so the mate outranks
        // the fifty move rule rather than the other way round
        let game = Board::from_fen("k7/8/1K6/8/8/8/8/7R w - - 99 100").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(2));
        assert_eq!(result.checkmate_in(), Some(1));
        assert_eq!(format!("{}", result.best_move), "h1h8");
    }

    #[test]
    fn a_check_that_does_not_mate_on_the_hundredth_half_move_is_still_a_draw() {
        // the same rook gives check on h8 but the king slips out to a7, so
        // the hundredth half move ends the game as a draw after all
        let game = Board::from_fen("k7/8/2K5/8/8/8/8/7R w - - 99 100").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    #[test]
    fn a_checkmated_root_is_game_over() {
        // fool's mate: white to move with no reply
        let game = Board::from_fen("rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3")
            .unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(3), SearchOutcome::GameOver));
    }

    #[test]
    fn a_stalemated_root_is_game_over() {
        let game = Board::from_fen("k7/8/1Q6/8/8/8/8/7K b - - 0 1").unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(3), SearchOutcome::GameOver));
    }

    #[test]
    fn a_search_with_no_time_budget_aborts_without_a_move() {
        let mut e = engine(Board::new());
        e.configure(time::Instant::now(), Some(time::Duration::ZERO));
        assert!(matches!(e.search(5), SearchOutcome::Aborted(None)));
    }

    #[test]
    fn deepening_with_no_time_budget_still_answers_depth_one() {
        // a clock that has already run out must still get a legal move back:
        // depth one is a few dozen nodes, so it runs whatever the clock says,
        // and only then is the budget allowed to stop anything
        let mut e = engine(Board::new());
        let mut options = SearchParameters::new();
        options.search_duration = Some(time::Duration::ZERO);
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _| depths.push(depth));
        assert!(
            matches!(outcome, SearchOutcome::Aborted(Some(_))),
            "expected a move from depth one, got {:?}",
            outcome
        );
        assert_eq!(depths, vec![1]);
    }

    #[test]
    fn timed_out_deepening_still_returns_a_move() {
        // The budget runs out long before MAX_DEPTH can complete, so the
        // deepening loop ends on an Aborted outcome and must fall back to a
        // completed iteration's move
        use super::SearchParameters;
        let mut e = engine(Board::new());
        let params = SearchParameters {
            depth: None,
            search_duration: Some(time::Duration::from_millis(50)),
            start_time: time::Instant::now(),
        };
        let outcome = e.iterative_deepening_search(params, |_, _, _| {});
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
    }

    #[test]
    fn deepening_reports_each_completed_depth() {
        use super::SearchParameters;
        let mut e = engine(Board::new());
        let mut depths = Vec::new();
        let mut node_counts = Vec::new();
        let outcome = e.iterative_deepening_search(
            SearchParameters::new_with_depth(3),
            |depth, result, _| {
                assert!(result.nodes > 0);
                depths.push(depth);
                node_counts.push(result.nodes);
            },
        );
        assert_eq!(depths, vec![1, 2, 3]);
        // the count covers the whole deepening so far, so each report says
        // more than the one before: reporting one iteration's count against
        // the whole search's clock is the bug this pins shut
        assert!(
            node_counts.windows(2).all(|w| w[0] < w[1]),
            "node counts must grow with each depth: {:?}",
            node_counts
        );
        let SearchOutcome::Complete(result) = outcome else {
            panic!("expected a completed search, got {:?}", outcome);
        };
        assert_eq!(
            Some(result.nodes),
            node_counts.last().copied(),
            "the returned result must carry the same total the last report did"
        );
    }

    #[test]
    fn deepening_stops_reporting_at_game_over() {
        use super::SearchParameters;
        // fool's mate again: there is nothing to report and nothing to play
        let game = Board::from_fen("rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3")
            .unwrap();
        let mut e = engine(game);
        let outcome = e
            .iterative_deepening_search(SearchParameters::new_with_depth(3), |_, _, _| {
                panic!("a finished game has no depths to report")
            });
        assert!(matches!(outcome, SearchOutcome::GameOver));
    }

    #[test]
    fn deepening_to_depth_zero_finds_nothing() {
        use super::SearchParameters;
        let mut e = engine(Board::new());
        let outcome =
            e.iterative_deepening_search(SearchParameters::new_with_depth(0), |_, _, _| {});
        assert!(matches!(outcome, SearchOutcome::Aborted(None)));
    }

    #[test]
    fn draw_taint_is_still_recorded_and_never_trusted() {
        // The pawn endgame carries the most draw traffic of the pinned
        // positions, so it is the one that exercises both halves of the graph
        // history work. tainted_stores going to zero means taint propagation
        // broke, and then the refusal in get_transposition is refusing nothing
        // while the hole silently reopens. tainted_score_cutoffs is zero by
        // construction while the refusal holds, so it moving means a probe
        // path that consumes scores without the refusal guard was added.
        let fen = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
        let mut e = engine(Board::from_fen(fen).unwrap());
        for depth in 1..=7 {
            completed(e.search(depth));
        }
        assert!(e.ghi.stores > 0, "the search stored nothing");
        assert!(
            e.ghi.tainted_stores > 0,
            "no draw taint was recorded: propagation is broken"
        );
        assert_eq!(
            e.ghi.tainted_score_cutoffs, 0,
            "a path dependent score was trusted"
        );
    }

    #[test]
    fn a_warm_cache_matches_cold_across_draw_context() {
        // The same pieces hash to the same key whatever the fifty move counter
        // says, so a search made a few plies from the draw fills the table
        // with scores that are true of that path only. A fresh game reaching
        // the same position must not read them: its search has the whole
        // clock ahead of it.
        let near_draw = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 96 112";
        let fresh = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 0 1";
        let mut warm = engine(Board::from_fen(near_draw).unwrap());
        completed(warm.search(6));
        warm.parse_fen(fresh).unwrap();
        let result = completed(warm.search(6));

        let mut cold = engine(Board::from_fen(fresh).unwrap());
        let expected = completed(cold.search(6));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn a_warm_cache_matches_a_cold_search() {
        // Searching a position with a cache warmed by unrelated positions must give the same
        // result as searching it with an empty cache
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        ];
        let mut warm = engine(Board::new());
        for fen in fens {
            warm.parse_fen(fen).unwrap();
            completed(warm.search(5));
        }
        for fen in fens {
            let game = Board::from_fen(fen).unwrap();
            let mut cold = engine(game);
            let expected = completed(cold.search(5));
            warm.parse_fen(fen).unwrap();
            let result = completed(warm.search(5));
            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
        }
    }

    #[test]
    fn a_small_table_matches_a_large_table() {
        // a table small enough to force constant collisions must not change the result
        let fen = SHARP_MIDDLEGAME;
        let mut big = engine(Board::from_fen(fen).unwrap());
        let expected = completed(big.search(5));
        let mut small = AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), 8 * 1024);
        let result = completed(small.search(5));
        assert_eq!(result.score, expected.score);
    }

    #[test]
    fn a_search_at_a_repetition_still_returns_a_move() {
        let game = Board::from_fen(fens::SHUFFLE).unwrap();
        let mut e = engine(game);
        for m in [
            "a8b8", "a1b1", "b8a8", "b1a1", "a8b8", "a1b1", "b8a8", "b1a1",
        ] {
            assert!(e.make_move_str(m), "failed to play {}", m);
        }
        assert!(e.board.is_repetition());
        assert!(matches!(e.search(3), SearchOutcome::Complete(_)));
    }

    #[test]
    fn a_new_game_forgets_the_previous_game() {
        let fen = SHARP_MIDDLEGAME;
        let mut e = engine(Board::from_fen(fen).unwrap());
        completed(e.search(4));
        assert!(e.moves.get(e.board.key).is_some(), "nothing was stored");
        assert_ne!(format!("{}", e.pv_line()), "");

        e.new_game();
        assert!(e.moves.get(e.board.key).is_none());
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_is_empty_without_a_cache_entry() {
        let game = Board::new();
        let e = engine(game);
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_stops_at_a_repetition() {
        // a shuffle both sides are content with leaves the table holding a line
        // that goes round for ever. The line stops once the position comes back,
        // because from there it is a draw either side can take, rather than
        // reporting a continuation nobody would go on to play.
        let game = Board::from_fen(fens::SHUFFLE).unwrap();
        let mut e = engine(game);
        let cycle = ["a8b8", "a1b1", "b8a8", "b1a1"];
        let mut board = e.board;
        for name in cycle.iter().cycle().take(16) {
            let play = play_named(&board, name);
            e.moves.set(board.key, searched(play, board.ply));
            assert!(board.make_move(&play), "failed to play {}", name);
        }

        assert_eq!(format!("{}", e.pv_line()), "a8b8 a1b1 b8a8 b1a1");
    }

    #[test]
    fn the_pv_line_stops_when_the_fifty_move_counter_runs_out() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let mut board = e.board;
        for name in ["c3d4", "f8g8"] {
            let play = play_named(&board, name);
            e.moves.set(board.key, searched(play, board.ply));
            assert!(board.make_move(&play), "failed to play {}", name);
        }
        assert_eq!(board.fifty_move_rule, 101);

        // the first move draws by the fifty move rule, so the reply the table
        // holds is one the game never gets to
        assert_eq!(format!("{}", e.pv_line()), "c3d4");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_move_which_is_illegal_here() {
        // two positions which hash to the same key share an entry, so the move
        // a probe comes back with is not always a move of the position asked
        // about
        let mut e = engine(Board::new());
        let a2 = 8;
        let a5 = 32;
        let colliding = Play::new(a2, a5, None, None, false, false);
        e.moves.set(e.board.key, searched(colliding, e.board.ply));

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_quiescence_entry() {
        // quiescence looks at captures and promotions alone, so its move is
        // fit for ordering the next search and not for saying what the engine
        // means to play
        let mut e = engine(Board::new());
        let play = play_named(&e.board, "e2e4");
        e.moves.set(
            e.board.key,
            Pv {
                play,
                score: 0,
                depth: 0,
                bound: Bound::Ordering,
                ply: 0,
                tainted: false,
            },
        );

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_move_which_leaves_the_king_in_check() {
        // moves are generated pseudo legally, so a pinned piece's move is in
        // the list for this position and still cannot be played. Asking whether
        // the move belongs to this position is not enough on its own, which is
        // why the walk goes on to check that making it succeeds.
        let board = Board::from_fen("4r2k/8/8/8/8/8/4N3/4K3 w - - 0 1").unwrap();
        let mut e = engine(board);
        let pinned = play_named(&e.board, "e2d4");
        e.moves.set(e.board.key, searched(pinned, e.board.ply));

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_is_bounded_by_the_search_depth() {
        // pawns only, so no position comes up twice and the fifty move counter
        // keeps being reset: nothing stops this line but the bound
        let mut names = Vec::new();
        for file in "abcdefgh".chars() {
            names.push(format!("{file}2{file}3"));
            names.push(format!("{file}7{file}6"));
        }
        for file in "abcdefgh".chars() {
            names.push(format!("{file}3{file}4"));
            names.push(format!("{file}6{file}5"));
        }

        let mut e = engine(Board::new());
        let mut board = e.board;
        for name in &names {
            let play = play_named(&board, name);
            e.moves.set(board.key, searched(play, board.ply));
            assert!(board.make_move(&play), "failed to play {}", name);
        }

        assert!(names.len() > super::MAX_DEPTH as usize);
        assert_eq!(e.pv_line().line.len(), super::MAX_DEPTH as usize);
    }

    #[test]
    fn a_stopped_search_does_not_poison_the_cache() {
        let fen = SHARP_MIDDLEGAME;
        let game = Board::from_fen(fen).unwrap();
        let mut cold = engine(game);
        let expected = completed(cold.search(6));

        // a search with no time budget stops immediately, it must not leave partial results in
        // the hash table which change the outcome of the next search
        let game = Board::from_fen(fen).unwrap();
        let mut e = engine(game);
        e.configure(time::Instant::now(), Some(time::Duration::ZERO));
        assert!(matches!(e.search(6), SearchOutcome::Aborted(_)));

        e.configure(time::Instant::now(), None);
        let result = completed(e.search(6));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }
}

#[cfg(test)]
mod hash_table {
    use super::{Bound, HashTable, Play, Pv};
    use pretty_assertions::assert_eq;

    fn new_pv(bound: Bound, depth: u8, ply: u16) -> Pv {
        Pv {
            play: Play::new(0, 1, None, None, false, false),
            score: 0,
            depth,
            bound,
            ply,
            tainted: false,
        }
    }

    #[test]
    fn get_compares_the_key_not_just_the_slot() {
        // two different keys which map to the same slot must not be confused for each other
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Bound::Exact, 1, 1));
        assert!(table.get(1).is_some());
        assert!(table.get(2).is_none());
    }

    #[test]
    fn an_exact_entry_replaces_a_non_exact_entry() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Bound::Lower, 1, 1));
        table.set(1, new_pv(Bound::Exact, 1, 1));
        assert!(matches!(table.get(1).unwrap().bound, Bound::Exact));
    }

    #[test]
    fn a_deeper_exact_entry_survives_a_shallower_one() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Bound::Exact, 8, 1));
        table.set(1, new_pv(Bound::Exact, 2, 1));
        assert_eq!(table.get(1).unwrap().depth, 8);
    }

    #[test]
    fn a_deeper_entry_replaces_an_exact_entry_for_another_position() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Bound::Exact, 1, 1));
        table.set(2, new_pv(Bound::Lower, 8, 1));
        assert!(table.get(2).is_some());
        assert!(table.get(1).is_none());
    }

    #[test]
    fn a_shallower_entry_does_not_evict_a_deeper_one_for_another_position() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Bound::Lower, 8, 1));
        table.set(2, new_pv(Bound::Exact, 1, 1));
        assert_eq!(table.get(1).unwrap().depth, 8);
    }

    #[test]
    fn a_quiescence_entry_does_not_evict_a_searched_entry() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Bound::Exact, 5, 1));
        table.set(2, new_pv(Bound::Ordering, 0, 1));
        assert_eq!(table.get(1).unwrap().depth, 5);
    }

    #[test]
    fn a_stale_entry_is_replaced_regardless_of_depth() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Bound::Exact, 8, 1));
        table.set(2, new_pv(Bound::Lower, 1, 100));
        assert!(table.get(2).is_some());
    }
}

#[cfg(test)]
mod node_counts {
    use super::AlphaBeta;
    use super::Board;
    use super::Game;
    use crate::board::fens;
    use pretty_assertions::assert_eq;

    /// Pinned, because how often the transposition table collides decides how
    /// much of the tree is searched again.
    const TABLE_BYTES: usize = 1 << 20;

    /// Positions chosen to reach different parts of the search: a quiet
    /// opening, a tactical middlegame, a pawn endgame, a position full of
    /// captures, and one with castling and promotions available.
    const POSITIONS: [(&str, &str, u8); 5] = [
        ("opening", fens::START, 6),
        ("kiwipete", fens::KIWIPETE, 5),
        ("pawn endgame", fens::PAWN_ENDGAME, 7),
        ("promotions", fens::PROMOTIONS, 5),
        ("middlegame", fens::MIDDLEGAME, 5),
    ];

    /// Widens the way the engine does when it plays, so the table is warm from
    /// the previous iteration the way it is in a real search, and totals what
    /// each iteration visited.
    fn nodes(fen: &str, depth: u8) -> u64 {
        let mut engine = AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES);
        (1..=depth)
            .map(|d| match engine.search(d) {
                super::SearchOutcome::Complete(result) => result.nodes,
                other => panic!("search did not complete: {:?}", other),
            })
            .sum()
    }

    /// Reports how much of the search's use of the transposition table depends
    /// on the path taken rather than on the position. Not an assertion, it
    /// prints, which is why it is ignored.
    ///
    ///     cargo test -p basic_engine --release ghi_report -- --ignored --nocapture
    #[test]
    #[ignore = "prints a measurement, see the doc comment"]
    fn ghi_report() {
        println!(
            "{:<14} {:>10} {:>10} {:>8} {:>10} {:>10} {:>8}",
            "position", "stores", "tainted", "%", "cutoffs", "tainted", "%"
        );
        let mut totals = (0u64, 0u64, 0u64, 0u64);
        for (name, fen, depth) in POSITIONS {
            let mut engine =
                AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES);
            for d in 1..=depth {
                match engine.search(d) {
                    super::SearchOutcome::Complete(_) => (),
                    other => panic!("search did not complete: {:?}", other),
                }
            }
            let g = engine.ghi;
            let pct = |a: u64, b: u64| {
                if b == 0 {
                    0.0
                } else {
                    100.0 * a as f64 / b as f64
                }
            };
            println!(
                "{:<14} {:>10} {:>10} {:>7.3}% {:>10} {:>10} {:>7.3}%",
                name,
                g.stores,
                g.tainted_stores,
                pct(g.tainted_stores, g.stores),
                g.score_cutoffs,
                g.tainted_score_cutoffs,
                pct(g.tainted_score_cutoffs, g.score_cutoffs)
            );
            totals.0 += g.stores;
            totals.1 += g.tainted_stores;
            totals.2 += g.score_cutoffs;
            totals.3 += g.tainted_score_cutoffs;
        }
        let pct = |a: u64, b: u64| {
            if b == 0 {
                0.0
            } else {
                100.0 * a as f64 / b as f64
            }
        };
        println!(
            "{:<14} {:>10} {:>10} {:>7.3}% {:>10} {:>10} {:>7.3}%",
            "TOTAL",
            totals.0,
            totals.1,
            pct(totals.1, totals.0),
            totals.2,
            totals.3,
            pct(totals.3, totals.2)
        );
    }

    /// The search is deterministic, so how many nodes it visits is an exact
    /// figure rather than a timing, and it says the same thing on any machine.
    /// It moves whenever move ordering, quiescence, the transposition table or
    /// any pruning changes, including the many such changes that leave the move
    /// finally played untouched, which is what makes it worth pinning.
    ///
    /// A deliberate change to the search is expected to move these. Update them
    /// in the same commit: the diff is then a statement of how much less, or
    /// more, of the tree the engine now looks at.
    #[test]
    fn node_counts_have_not_moved() {
        let counted: Vec<(&str, u64)> = POSITIONS
            .iter()
            .map(|(name, fen, depth)| (*name, nodes(fen, *depth)))
            .collect();
        assert_eq!(
            counted,
            vec![
                ("opening", 151_429),
                ("kiwipete", 297_693),
                ("pawn endgame", 184_523),
                ("promotions", 119_840),
                ("middlegame", 220_237),
            ]
        );
    }
}
