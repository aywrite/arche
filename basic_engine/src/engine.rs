use crate::Game;
use crate::board::{Board, MoveList};
use crate::limits::Limits;
use crate::misc::{Color, Score};
use crate::play::Play;
use crate::transposition::{DEFAULT_TABLE_BYTES, GhiCounters, Probe, TranspositionTable};
use std::fmt;
use std::time;

const CHECKMATE_SCORE: Score = 30_000;
pub(crate) const MAX_DEPTH: u8 = 20;
// Move lists up to this long are sorted on the stack in order_moves, and it
// is a MoveList's inline capacity, so that is all of them bar a spill.
// Swept by callgrind rather than chosen: smaller cutoffs are a wash,
// because the mid length lists of full width nodes keep paying the
// allocating fallback while every call pays the extra branch.
const SORT_ON_THE_STACK_UP_TO: usize = 64;
// Any score this close to CHECKMATE_SCORE is a forced mate. Regular evals are
// bounded by the material on the board, which cannot come near it.
pub(crate) const CHECKMATE_THRESHOLD: Score = CHECKMATE_SCORE - 1000;

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

    /// Give the engine a transposition table of `bytes` bytes, discarding
    /// whatever the old one held. Reallocating rather than resizing in place
    /// is what the protocol expects of a size change, since a bucket is
    /// chosen from the number of buckets there are and every entry moves
    /// when that number does.
    ///
    /// False if the buckets could not be reserved, in which case the engine
    /// keeps the table it had: the size arrives from an interface, which is
    /// free to ask for more than the machine running us has, and a game is
    /// better carried on with the old table than lost with no engine.
    ///
    /// The answer is the allocator's, so it catches a size larger than the
    /// machine can address or than it will hand out. It is not a promise that
    /// the memory is there to use: where the kernel overcommits, a size that
    /// fits in ram and swap is granted here and the process killed later as
    /// the entries are written.
    #[must_use]
    fn set_table_bytes(&mut self, bytes: usize) -> bool;

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
    /// The depth to deepen to, or none for as deep as the engine goes.
    /// Reaching it is how a search finishes, which is why it is not one of
    /// the limits below.
    pub depth: Option<u8>,
    /// What the search may spend: the clock it started on and the nodes it
    /// may visit.
    pub limits: Limits,
}

impl SearchParameters {
    /// A search to the depth given, under the limits given.
    pub fn new(depth: Option<u8>, limits: Limits) -> Self {
        Self { depth, limits }
    }

    /// A search to a fixed depth and nothing else: no clock, no budget.
    pub fn to_depth(depth: u8) -> Self {
        Self::new(Some(depth), Limits::unlimited())
    }
}

/// The policies a search runs under: the shortcuts it takes and the scores
/// it trusts. Each one is a fact about the tree searched, so changing one
/// moves the bench.
///
/// Two configurations are named. The reference has every shortcut off and
/// every refusal on: alpha-beta with a table that only speeds it up, which
/// is why a position searched warm answers as it does cold, deepened as it
/// does direct, and with a small table as with a large one. The tests hold
/// the reference to that and it stays as it is when shortcuts arrive. A
/// change claiming to be sound keeps those tests green whatever else it
/// moves; a change that prunes moves the default and leaves the reference
/// alone; and the two played against each other say what the shortcuts are
/// worth. The default is what the engine plays with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchConfig {
    /// Whether to refuse a score from a draw tainted entry, trusting only
    /// its move.
    ///
    /// On in the reference and by default. A tainted score describes the
    /// path that stored it rather than the position, so a search arriving
    /// another way can read a draw it cannot actually reach. Refusing
    /// costs, over the bench at its depth and table, +3.3% nodes: +0.2% in
    /// the openings and +0.1% in the middlegames, which store almost no
    /// taint, and +41% in the endgames, where a tenth of what lands in
    /// the table is tainted and a fifth of the cutoffs it offers are
    /// refused. The cost grows with depth, +0.4% at depth five and +2.2% at
    /// nine, where the endgames cost +53% and the queen endgame three times
    /// its nodes, and it hardly moves with the table, except at 1 MB and
    /// depth nine, where sixty five thousand slots are asked to take
    /// seventy million stores and the two searches no longer share enough
    /// of the table to compare. No root
    /// move differed between refusing and trusting on any position at any
    /// depth or table measured; at depth nine one score did, lucena's, by
    /// five centipawns at 16 MB and above. What the refusal buys has yet to
    /// be seen.
    pub refuse_tainted_cutoffs: bool,
}

impl SearchConfig {
    /// The search with every shortcut off: what the exactness tests hold
    /// the search to, and the side a shortcut is measured against.
    pub const fn reference() -> Self {
        Self {
            refuse_tainted_cutoffs: true,
        }
    }

    /// The word the bench prints for what this configuration does with a
    /// draw tainted score, `refuse` it and search or `trust` it and cut,
    /// and reads back with `with_taint`. The words are the policies the
    /// graph history experiments compare.
    pub fn taint_word(self) -> &'static str {
        if self.refuse_tainted_cutoffs {
            "refuse"
        } else {
            "trust"
        }
    }

    /// The default with its taint policy set by word, or none for a word
    /// that is no policy. The default rather than the reference, so that
    /// `refuse` names what a bench runs when it is told nothing and the
    /// header it prints can be handed back to it; `trust` is then the
    /// engine as it plays with that one refusal off, the control arm of
    /// the experiments.
    // the default with the one switch set; written that way so it still
    // reads so once there are more switches
    #[allow(clippy::needless_update)]
    pub fn with_taint(word: &str) -> Option<Self> {
        let refuse_tainted_cutoffs = match word {
            "refuse" => true,
            "trust" => false,
            _ => return None,
        };
        Some(Self {
            refuse_tainted_cutoffs,
            ..Self::default()
        })
    }
}

impl Default for SearchConfig {
    /// What the engine plays with. The reference, for now: there is no
    /// shortcut yet to turn on.
    fn default() -> Self {
        Self::reference()
    }
}

pub struct AlphaBeta {
    pub board: Board,
    config: SearchConfig,
    nodes: u64,
    transpositions: TranspositionTable,
    selective_depth: u8,
    // search state
    /// Whether the value the last search call returned was draw tainted. The
    /// search is depth first and single threaded, so one flag threads the taint
    /// up without changing every return type.
    tainted: bool,
    /// What the search call under way may spend. The deepening loop hands
    /// each iteration its own, which is how depth one runs with none.
    limits: Limits,
    /// The node count at which the limits are looked at next, which the
    /// limits themselves decide.
    next_check: u64,
    /// The nodes quiescence visited, a part of nodes. Counted for the bench,
    /// which reports what share of the tree the captures are. Never reset,
    /// like the ghi counters: it runs over the engine's whole life, and the
    /// bench reads it from an engine made for the one search
    quiescence_nodes: u64,
}

/// What sort_by_cached_key does, minus its allocation, for a list that fits
/// the buffer: a stable insertion sort over keys computed once each.
/// Shifting only while strictly greater keeps equal keys in their generated
/// order, exactly as the stable sort does, so the two produce the same order
/// and the tree searched is the same whichever runs; the node count tests
/// hold both to that. Generic over the key, so this call compiles to the
/// same code it replaced rather than to one through a pointer.
#[inline]
fn sort_on_the_stack(moves: &mut [Play], key: impl Fn(&Play) -> i64) {
    debug_assert!(moves.len() <= SORT_ON_THE_STACK_UP_TO);
    let mut keys = [0i64; SORT_ON_THE_STACK_UP_TO];
    for (i, m) in moves.iter().enumerate() {
        keys[i] = key(m);
    }
    for i in 1..moves.len() {
        let k = keys[i];
        let m = moves[i];
        let mut j = i;
        while j > 0 && keys[j - 1] > k {
            keys[j] = keys[j - 1];
            moves[j] = moves[j - 1];
            j -= 1;
        }
        keys[j] = k;
        moves[j] = m;
    }
}

impl AlphaBeta {
    pub fn with_table_bytes(board: Board, bytes: usize) -> Self {
        Self::with_config(board, bytes, SearchConfig::default())
    }

    /// An engine searching under the policies given, with a table of the
    /// size given.
    pub fn with_config(board: Board, bytes: usize, config: SearchConfig) -> Self {
        Self {
            board,
            config,
            nodes: 0,
            transpositions: TranspositionTable::of_bytes(bytes),
            selective_depth: 0,
            tainted: false,
            limits: Limits::unlimited(),
            next_check: 0,
            quiescence_nodes: 0,
        }
    }

    fn eval(&self) -> Score {
        self.board.eval()
    }

    pub fn clear_transpositions(&mut self) {
        self.transpositions.clear();
    }

    /// The bytes the transposition table occupies. Whole buckets, so a size
    /// that does not divide by one reads back as the next size up.
    pub fn table_bytes(&self) -> usize {
        self.transpositions.bytes()
    }

    /// How much of the search's use of the transposition table depended on
    /// the path taken rather than on the position. A measurement, not a
    /// result: see the graph history interaction notes on the counters.
    pub fn ghi(&self) -> GhiCounters {
        self.transpositions.ghi()
    }

    /// How many of the nodes visited so far were quiescence's, over every
    /// search this engine has run.
    pub fn quiescence_nodes(&self) -> u64 {
        self.quiescence_nodes
    }

    /// Cooperative limit check. The limits say when to look at them again,
    /// which is every few thousand nodes for the clock and the node budget
    /// itself, exactly. The count is incremented after this is asked, so a
    /// budget of n is n nodes visited.
    fn poll_deadline(&mut self) -> Result<(), Aborted> {
        if self.nodes < self.next_check {
            return Ok(());
        }
        self.check_limits()
    }

    /// The slow half of poll_deadline, kept out of the search's own code:
    /// reading the clock and arming the next check happen once in thousands
    /// of nodes, and inlined at every poll they only made the hot loop larger.
    #[cold]
    #[inline(never)]
    fn check_limits(&mut self) -> Result<(), Aborted> {
        if self.limits.expired(self.nodes) {
            return Err(Aborted);
        }
        self.next_check = self.limits.next_check_after(self.nodes);
        Ok(())
    }

    fn result_for(&self, best_move: Play, score: Score) -> SearchResult {
        SearchResult {
            nodes: self.nodes,
            elapsed: self.limits.elapsed(),
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
        let key = |m: &Play| {
            let mut score = m.mvv_lva(&self.board);
            if pv_play == Some(*m) {
                score += 100_000;
            }
            -score
        };
        // Most lists here are short: quiescence sorts a handful of captures
        // or the evasions the filter kept, and the counts say under nine
        // moves on average. sort_by_cached_key allocates scratch on every
        // call, which at that size costs more than the sorting, so lists
        // take the stack sort instead, keeping the allocating sort only for
        // a list that spilled the buffer.
        if moves.len() <= SORT_ON_THE_STACK_UP_TO {
            sort_on_the_stack(moves, key);
        } else {
            moves.sort_by_cached_key(key);
        }
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
        self.quiescence_nodes += 1;

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
        let pv_play = self.transpositions.ordering_play(&self.board);
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
            self.transpositions
                .record_ordering(&self.board, best_move.unwrap(), alpha);
        }
        Ok(alpha)
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
        let pv_play = match self.transpositions.probe(
            &self.board,
            alpha,
            beta,
            depth,
            self.config.refuse_tainted_cutoffs,
        ) {
            Probe::Cut { score, tainted } => {
                // whatever the stored score depended on, this frame now depends
                // on too
                self.tainted = tainted;
                return Ok(score);
            }
            Probe::Order(play) => Some(play),
            Probe::Miss => None,
        };

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
                            self.transpositions.record_cutoff(
                                &self.board,
                                tt,
                                beta,
                                depth,
                                node_tainted,
                            );
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
                        self.transpositions.record_cutoff(
                            &self.board,
                            *m,
                            beta,
                            depth,
                            node_tainted,
                        );
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
            let best = best_move.expect("alpha only rises when a move raises it");
            self.transpositions
                .record_best(&self.board, best, alpha, depth, node_tainted);
        }
        self.tainted = node_tainted;
        Ok(alpha)
    }

    pub fn new(board: Board) -> Self {
        AlphaBeta::with_table_bytes(board, DEFAULT_TABLE_BYTES)
    }

    /// The root loop: the one node whose answer must include a play, which is
    /// why it runs here rather than in alpha_beta. The root probes the
    /// transposition table to order moves and stores its entry when done, but
    /// never takes a stored score in place of searching: a stored score can
    /// come from a line whose repetition and fifty move context differ from
    /// the game being played, and the answer must not depend on one.
    pub fn search(&mut self, depth: u8) -> SearchOutcome {
        self.transpositions.new_search();
        self.search_within(depth, Limits::unlimited())
    }

    /// One fixed depth search under the limits given for it. The deepening
    /// loop hands each iteration its own, which is how depth one runs with
    /// none.
    ///
    /// The caller owns the table's generation. `search` and the deepening
    /// loop each start one with `new_search`, so that however many
    /// iterations follow, what they store belongs to the one search and ages
    /// together from the next. Calling this on its own skips that, and a
    /// table whose generation never moves keeps every entry looking current:
    /// nothing goes stale and the oldest entries are never the ones given up.
    pub fn search_within(&mut self, depth: u8, limits: Limits) -> SearchOutcome {
        self.limits = limits;
        self.next_check = 0;
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

        let pv_play = self.transpositions.ordering_play(&self.board);
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
        self.transpositions
            .record_answer(&self.board, play, alpha, depth, root_tainted);
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
            let Some(play) = self.transpositions.intended_play(&board) else {
                break;
            };
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
        self.clear_transpositions();
    }

    fn set_table_bytes(&mut self, bytes: usize) -> bool {
        match TranspositionTable::with_capacity_bytes(bytes) {
            Some(table) => {
                self.transpositions = table;
                true
            }
            None => false,
        }
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
        // one search, however many iterations: what the iterations store is
        // one generation's, and ages together from the next go
        self.transpositions.new_search();

        for depth in 1..=max_depth {
            let limits = search_options
                .limits
                .for_iteration(best.is_some(), total_nodes);
            match self.search_within(depth, limits) {
                SearchOutcome::Aborted(_) => {
                    // the interrupted iteration's best-so-far is discarded: a
                    // completed shallower iteration outranks it, and depth one
                    // runs without limits so there always is one
                    return SearchOutcome::Aborted(best);
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
    /// A limit ran out partway through, carrying a best-so-far when the
    /// root got far enough to have one. Weaker than any Complete result: the
    /// play may be a fail high that never got re-searched.
    Aborted(Option<SearchResult>),
}

/// The search hit a limit and unwound without finishing. The score of an
/// aborted frame is meaningless, and returning this instead of a score is what
/// keeps it out of the transposition table: propagation with `?` never reaches
/// the stores.
struct Aborted;

#[derive(Debug)]
pub struct SearchResult {
    pub nodes: u64, // The number of positions visited during the search
    /// How long the search took, measured over the same interval as the
    /// nodes beside it, so that one divides the other honestly.
    pub elapsed: time::Duration,
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
    use super::{Limits, Play, SearchConfig, SearchOutcome, SearchParameters, SearchResult};
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

    #[test]
    fn a_resized_table_is_the_size_asked_for_and_still_searched_on() {
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(1024 * 1024));

        // the table a new engine asked for a megabyte would have been given,
        // which is the whole buckets that fit in one rather than the megabyte
        assert_eq!(
            e.table_bytes(),
            AlphaBeta::with_table_bytes(Board::new(), 1024 * 1024).table_bytes()
        );
        assert!(e.table_bytes() <= 1024 * 1024);
        assert!(matches!(e.search(4), SearchOutcome::Complete(_)));
    }

    #[test]
    fn a_table_there_is_no_memory_for_leaves_the_old_one_in_place() {
        // the size arrives from an interface, which may ask for more than the
        // machine has. Losing the engine mid game over it would be worse than
        // playing on with the table we already had.
        //
        // both ways of failing: usize::MAX asks for more bytes than an
        // allocation may describe and is refused before the allocator is
        // reached, while isize::MAX asks for a number it may describe and no
        // machine can meet, which is the refusal a real oversized Hash hits
        for bytes in [usize::MAX, isize::MAX as usize] {
            let mut e = engine(Board::new());
            assert!(!e.set_table_bytes(bytes), "{}", bytes);
            assert_eq!(e.table_bytes(), engine(Board::new()).table_bytes());
            assert!(matches!(e.search(3), SearchOutcome::Complete(_)));
        }
    }

    #[test]
    fn a_table_too_small_to_hold_an_entry_is_still_a_table() {
        // the size arrives from the protocol, so a nonsense one has to be
        // survivable rather than a panic in the middle of a game
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(0));
        assert!(e.table_bytes() > 0);
        assert!(matches!(e.search(3), SearchOutcome::Complete(_)));
    }

    /// The reference search, for the tests that hold it to answering the
    /// same whatever the table holds. They are the search's exactness
    /// contract and stay green through every shortcut: a shortcut moves the
    /// default, which `engine` builds, and not this.
    fn reference(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(board, TABLE_BYTES, SearchConfig::reference())
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

    /// The depth a seeded entry claims: deep enough that nothing these tests
    /// search can outrank it.
    const SEEDED_DEPTH: u8 = 5;

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
        e.transpositions.record_best(&e.board, quiet, 0, 14, false);
        let result = completed(e.search(2));
        let takes = play_named(&e.board, "d2d5");
        assert_eq!(result.best_move, takes);
        assert_eq!(e.pv_line().line.first(), Some(&takes));
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
            let mut cold = reference(Board::from_fen(fen).unwrap());
            let expected = completed(cold.search(5));
            let mut warm = reference(Board::from_fen(fen).unwrap());
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

    /// A budget of nodes and no clock.
    fn nodes_only(nodes: u64) -> Limits {
        Limits::starting_at(time::Instant::now(), None, nodes)
    }

    /// A search whose clock ran out before it began.
    fn already_spent() -> Limits {
        Limits::starting_at(
            time::Instant::now() - time::Duration::from_secs(1),
            Some(time::Duration::from_millis(1)),
            u64::MAX,
        )
    }

    #[test]
    fn a_search_with_no_time_budget_aborts_without_a_move() {
        let mut e = engine(Board::new());
        assert!(matches!(
            e.search_within(5, already_spent()),
            SearchOutcome::Aborted(None)
        ));
    }

    #[test]
    fn a_blown_deadline_stops_before_it_searches() {
        // the first poll happens before the root is counted, so a clock
        // already gone costs nothing at all rather than a poll interval
        let mut e = engine(Board::new());
        assert!(matches!(
            e.search_within(5, already_spent()),
            SearchOutcome::Aborted(None)
        ));
        assert_eq!(e.nodes, 0, "it searched past a clock that had run out");
    }

    #[test]
    fn deepening_with_no_time_budget_still_answers_depth_one() {
        // a clock that has already run out must still get a legal move back:
        // depth one is a few dozen nodes, so it runs whatever the clock says,
        // and only then is the budget allowed to stop anything
        let mut e = engine(Board::new());
        let options = SearchParameters::new(None, already_spent());
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
    fn a_node_budget_stops_the_search_on_exactly_that_node() {
        // a spread of budgets, so the last node falls in the full search and
        // in quiescence by turns, and now and then exactly on the end of an
        // iteration, which leaves the next one nothing and aborts it at once
        for limit in (50..6_000).step_by(97) {
            let mut e = engine(Board::new());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut completed: u64 = 0;
            let outcome =
                e.iterative_deepening_search(options, |_, result, _| completed = result.nodes);
            assert!(
                matches!(outcome, SearchOutcome::Aborted(Some(_))),
                "{}",
                limit
            );
            // the reported total is the last completed depth's, and the
            // aborted iteration's own count is still on the engine: together
            // they are every node visited, and that has to be the budget to
            // the node
            assert_eq!(completed + e.nodes, limit, "budget {}", limit);
        }
    }

    #[test]
    fn a_node_budget_and_a_clock_stop_at_whichever_comes_first() {
        // the clock wins: a spent clock and a generous budget end after
        // depth one, which runs whatever either says
        let mut e = engine(Board::new());
        let options = SearchParameters::new(
            None,
            Limits::starting_at(
                time::Instant::now() - time::Duration::from_secs(1),
                Some(time::Duration::from_millis(1)),
                1_000_000,
            ),
        );
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _| depths.push(depth));
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(depths, vec![1]);

        // the budget wins: a clock with time to spare and a small budget stop
        // on the budget's node
        let mut e = engine(Board::new());
        let limit = 1_000;
        let options = SearchParameters::new(
            None,
            Limits::starting_now(Some(time::Duration::from_secs(10)), Some(limit)),
        );
        let mut completed: u64 = 0;
        let outcome =
            e.iterative_deepening_search(options, |_, result, _| completed = result.nodes);
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(completed + e.nodes, limit);
    }

    #[test]
    fn a_node_budget_too_small_for_depth_one_still_answers_a_move() {
        let mut e = engine(Board::new());
        let options = SearchParameters::new(None, nodes_only(0));
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _| depths.push(depth));
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(depths, vec![1]);
    }

    #[test]
    fn a_node_budget_and_a_depth_stop_at_whichever_comes_first() {
        let mut e = engine(Board::new());
        let options = SearchParameters::new(Some(2), nodes_only(1_000_000));
        assert!(matches!(
            e.iterative_deepening_search(options, |_, _, _| {}),
            SearchOutcome::Complete(_)
        ));

        let mut e = engine(Board::new());
        let options = SearchParameters::new(Some(super::MAX_DEPTH), nodes_only(1_000));
        let mut last_depth = 0;
        let outcome = e.iterative_deepening_search(options, |depth, _, _| last_depth = depth);
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert!(last_depth < super::MAX_DEPTH);
    }

    #[test]
    fn deepening_reports_each_completed_depth() {
        use super::SearchParameters;
        let mut e = engine(Board::new());
        let mut depths = Vec::new();
        let mut node_counts = Vec::new();
        let outcome =
            e.iterative_deepening_search(SearchParameters::to_depth(3), |depth, result, _| {
                assert!(result.nodes > 0);
                depths.push(depth);
                node_counts.push(result.nodes);
            });
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
    fn a_finished_game_is_game_over_with_no_depth_to_report() {
        use super::SearchParameters;
        // fool's mate, white to move with no reply, and a stalemate: there is
        // nothing to play, so a search says so and deepening reports nothing
        let fens = [
            "rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3",
            "k7/8/1Q6/8/8/8/8/7K b - - 0 1",
        ];
        for fen in fens {
            let mut e = engine(Board::from_fen(fen).unwrap());
            assert!(matches!(e.search(3), SearchOutcome::GameOver), "{fen}");
            let outcome = e.iterative_deepening_search(SearchParameters::to_depth(3), |_, _, _| {
                panic!("a finished game has no depths to report")
            });
            assert!(matches!(outcome, SearchOutcome::GameOver), "{fen}");
        }
    }

    #[test]
    fn a_clock_that_runs_out_mid_deepening_still_answers_with_a_completed_depth() {
        // the one clock in the suite that is not zero. A zero budget aborts
        // on the first poll, before the next check is ever armed, so this
        // is the only test of the clock being read again thousands of nodes
        // on. Fifty milliseconds from the opening is orders of magnitude
        // short of MAX_DEPTH, so the clock wins, and the answer has to be
        // the move of a depth that completed
        let mut e = engine(Board::new());
        let params = SearchParameters::new(
            None,
            Limits::starting_now(Some(time::Duration::from_millis(50)), None),
        );
        let outcome = e.iterative_deepening_search(params, |_, _, _| {});
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
    }

    #[test]
    fn deepening_to_depth_zero_finds_nothing() {
        use super::SearchParameters;
        let mut e = engine(Board::new());
        let outcome = e.iterative_deepening_search(SearchParameters::to_depth(0), |_, _, _| {});
        assert!(matches!(outcome, SearchOutcome::Aborted(None)));
    }

    #[test]
    fn draw_taint_is_still_recorded_and_never_trusted() {
        // The pawn endgame carries the most draw traffic of the bench
        // positions, so it is the one that exercises both halves of the graph
        // history work. tainted_stores going to zero means taint propagation
        // broke, and then the refusal in get_transposition is refusing nothing
        // while the hole silently reopens. tainted_score_cutoffs is zero by
        // construction while the refusal holds, so it moving means a probe
        // path that consumes scores without the refusal guard was added.
        // The refusal is a policy of the reference, so the reference is what
        // is built: a default told one day to trust those scores is an
        // experiment to measure, not a hole to find here.
        let fen = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
        let mut e = reference(Board::from_fen(fen).unwrap());
        for depth in 1..=7 {
            completed(e.search(depth));
        }
        assert!(e.ghi().stores > 0, "the search stored nothing");
        assert!(
            e.ghi().tainted_stores > 0,
            "no draw taint was recorded: propagation is broken"
        );
        assert_eq!(
            e.ghi().tainted_score_cutoffs,
            0,
            "a path dependent score was trusted"
        );
        // the refusals are what the policy costs, so they are counted as
        // the cutoffs would have been: a refusal that went uncounted would
        // make the policy look free
        assert!(
            e.ghi().refused_cutoffs > 0,
            "the refusal refused nothing, or refused without counting"
        );
    }

    #[test]
    fn every_taint_word_names_a_policy_and_the_policy_names_it_back() {
        // the bench reads a word and prints one, and they have to be the
        // same word or a report could not be rerun from its header; and
        // the word the default prints has to name the default, or a bench
        // told nothing and a bench told that word would run apart
        for word in ["refuse", "trust"] {
            let config =
                SearchConfig::with_taint(word).unwrap_or_else(|| panic!("{word} is not a policy"));
            assert_eq!(config.taint_word(), word);
        }
        let default = SearchConfig::default();
        assert_eq!(
            SearchConfig::with_taint(default.taint_word()),
            Some(default)
        );
        assert_eq!(SearchConfig::with_taint("maybe"), None);
    }

    #[test]
    fn a_search_told_to_trust_tainted_scores_takes_their_cutoffs() {
        // the refusal is the one policy the config carries so far, and a
        // search told to trust those scores is the control arm of the graph
        // history experiments. The switch has to reach the probe: a field
        // the search never reads would make every comparison against the
        // reference a comparison of the reference with itself
        let fen = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
        // the reference with the one switch flipped, which is what a control
        // arm is; written that way so it still says so once there are more
        #[allow(clippy::needless_update)]
        let trusting = SearchConfig {
            refuse_tainted_cutoffs: false,
            ..SearchConfig::reference()
        };
        let mut e = AlphaBeta::with_config(Board::from_fen(fen).unwrap(), TABLE_BYTES, trusting);
        for depth in 1..=7 {
            completed(e.search(depth));
        }
        assert!(
            e.ghi().tainted_score_cutoffs > 0,
            "the scores the search was told to trust cut nothing"
        );
        assert_eq!(
            e.ghi().refused_cutoffs,
            0,
            "a search trusting tainted scores refused one"
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
        let mut warm = reference(Board::from_fen(near_draw).unwrap());
        completed(warm.search(6));
        warm.parse_fen(fresh).unwrap();
        let result = completed(warm.search(6));

        let mut cold = reference(Board::from_fen(fresh).unwrap());
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
        let mut warm = reference(Board::new());
        for fen in fens {
            warm.parse_fen(fen).unwrap();
            completed(warm.search(5));
        }
        for fen in fens {
            let game = Board::from_fen(fen).unwrap();
            let mut cold = reference(game);
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
        let mut big = reference(Board::from_fen(fen).unwrap());
        let expected = completed(big.search(5));
        let mut small = AlphaBeta::with_config(
            Board::from_fen(fen).unwrap(),
            8 * 1024,
            SearchConfig::reference(),
        );
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
        assert!(
            e.transpositions.ordering_play(&e.board).is_some(),
            "nothing was stored"
        );
        assert_ne!(format!("{}", e.pv_line()), "");

        e.new_game();
        assert!(e.transpositions.ordering_play(&e.board).is_none());
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
            e.transpositions
                .record_best(&board, play, 0, SEEDED_DEPTH, false);
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
            e.transpositions
                .record_best(&board, play, 0, SEEDED_DEPTH, false);
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
        e.transpositions
            .record_best(&e.board, colliding, 0, SEEDED_DEPTH, false);

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_quiescence_entry() {
        // quiescence looks at captures and promotions alone, so its move is
        // fit for ordering the next search and not for saying what the engine
        // means to play
        let mut e = engine(Board::new());
        let play = play_named(&e.board, "e2e4");
        e.transpositions.record_ordering(&e.board, play, 0);

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
        e.transpositions
            .record_best(&e.board, pinned, 0, SEEDED_DEPTH, false);

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
            e.transpositions
                .record_best(&board, play, 0, SEEDED_DEPTH, false);
            assert!(board.make_move(&play), "failed to play {}", name);
        }

        assert!(names.len() > super::MAX_DEPTH as usize);
        assert_eq!(e.pv_line().line.len(), super::MAX_DEPTH as usize);
    }

    #[test]
    fn a_stopped_search_does_not_poison_the_cache() {
        let fen = SHARP_MIDDLEGAME;
        let game = Board::from_fen(fen).unwrap();
        let mut cold = reference(game);
        let expected = completed(cold.search(6));

        // a search whose clock has run out stops immediately, and must not
        // leave partial results in the table which change the outcome of the
        // next search
        let game = Board::from_fen(fen).unwrap();
        let mut e = reference(game);
        assert!(matches!(
            e.search_within(6, already_spent()),
            SearchOutcome::Aborted(_)
        ));

        let result = completed(e.search(6));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }
}
