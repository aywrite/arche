# Architecture

An overview of how the code is organized and how the main parts work. See
[DEVELOPMENT.md](DEVELOPMENT.md) for how to build, test and measure a change,
[INSTRUMENTS.md](INSTRUMENTS.md) for what the search measures about itself, and
[ROADMAP.md](ROADMAP.md) for what is not implemented yet.

## Overview

The workspace has two crates:

- **arche-core/**: The engine itself. Board, move generation, search,
  evaluation and the transposition table.
- **src/**: The `arche` binary. Wraps the engine in the UCI protocol (the
  text protocol chess GUIs and match runners speak).

The split means the engine can be tested without spawning a process and the
protocol can be tested without running a search.

The board is represented using bitboards: one 64 bit integer per piece type
and one per colour, with one bit per square. The search is alpha beta with
iterative deepening, quiescence search and a transposition table. Evaluation
is material plus piece square tables, tapered between middlegame and endgame,
plus four terms counted at the leaf: piece mobility, king safety, pawn
structure and the king attack zone.

## Code map: arche-core

- **board.rs**: The position and the rules. Holds the piece bitboards plus
  some state that could be recomputed from them but would be too slow to:
  an array of what stands on each square, the zobrist key, a second key
  over the pawns alone, running totals for material and the piece square
  evaluation, the pieces giving check, and a ring of the last ~1024 plies
  (used by the repetition and fifty move rules, and to undo moves). All
  piece placement goes through one function, which is what keeps the
  derived state in sync. Debug builds recompute the derived state from
  scratch after every move and assert it matches, so a bug in an
  incremental update fails tests instead of misevaluating quietly.
  It names no evaluation term: each one reads the boards it needs through
  `pub(crate)` accessors and keeps its own counts and masks beside its
  weights.
  Move generation also lives here. It is pseudo-legal: moves are generated
  without checking whether they leave the king in check, and `make_move`
  rejects the ones that do. When already in check the list is first
  filtered down to moves that could address the check, which saves sorting
  and playing moves that would only be rejected. Two questions the search
  asks about a move before making it are answered here as well: what a
  swap on its square is worth, and whether it gives check.
- **magic.rs**: Attack lookups for the sliding pieces (bishop, rook,
  queen), using magic bitboards: a table computed at compile time that maps
  "rook on this square, these pieces in the way" directly to the attacked
  squares. The "magic" is the multiplication trick used for the index.
- **bitboard.rs**: Bit level helpers for the boards.
- **misc.rs**: The piece, colour and coordinate types.
- **play.rs**: A single move. Six bytes, and the size is load bearing:
  move lists live on the stack at every level of the search, so a bigger
  move means a slower search. ROADMAP.md records the failed attempts to
  enlarge it.
- **engine.rs**: The search. Alpha beta (skip any line already proven
  worse than one we can force), deepening one ply at a time with each pass
  seeding the next through the transposition table and opening at an
  aspiration window around the last pass's score, widened on the side a
  pass falls outside it. At the horizon,
  quiescence search keeps following captures until the position goes
  quiet, since evaluating in the middle of an exchange scores a hanging
  queen as material. Also here: principal variation search (ask a move
  after the first whether it beats the best so far before searching it
  properly), reverse futility pruning (answer a node from its evaluation
  when that is already far above what the opponent can accept), null move
  pruning (answer it from a reduced search of the position left by passing
  the move) and the check extension.
- **late_move.rs**: What a node does with a quiet move its ordering put
  late. The late move reduction scouts such a move shallower, by plies read
  off a table by the node's depth and the move's index, and trusts the
  answer when it comes back low. The scout runs a ply deeper still once the
  move's index passes a floor that rises with the node's depth. The
  attention model, a logistic regression over what the node knows about the
  move, fitted offline and carried as integers, decides which moves are not
  searched at all. At depths one to three, below the model's floor, two
  rules drop a quiet move after the node's first: quiet futility, where the
  static evaluation plus a pawn a ply cannot reach alpha, and a count, once
  the node has searched four moves a ply. The features the model scores are
  the ones the reduction ledger records.
- **ordering.rs**: The order moves are tried in. The transposition table's
  move first, then the captures the swap prices as winning or even, by
  what each wins with most valuable victim / least valuable attacker
  breaking the ties, then the quiet moves by two memories the search fills
  as it goes: the killers, which are the quiet moves that cut off at this
  distance from the root, and a history table of how often each quiet move
  has cut off anywhere against how often it was tried and did not. A move
  the table has marked down sorts behind the quiet moves nothing is known
  about. The losing captures close the list. The quiet moves are put in
  order only as far as the move loop reads them. Alpha beta prunes more the
  sooner a good move is found, so ordering has an outsized effect on tree
  size.
- **limits.rs**: When to stop searching. A clock, a node budget, a soft
  rule that skips starting an iteration which would get less than half
  done, and a stop flag shared with the interface thread. The flag is read
  in the same place as the clock, roughly every three thousand nodes.
- **value.rs**: A score plus a taint bit recording whether it depended on
  a repetition or fifty move draw somewhere down its line. Such a score is
  only true of the path that produced it, which the table needs to know.
  The mate arithmetic lives here too: what scores count as a forced mate,
  how one is read back as moves to mate, and the window mate distance
  pruning leaves a node.
- **transposition.rs**: The transposition table: a cache of positions
  searched before, keyed by zobrist hash, holding the score and best move
  found last time. Entries are 16 bytes, four to a cache line, replaced by
  age and depth. A hit can answer a node outright or just say which move
  to try first. Tainted scores are counted and by default trusted anyway,
  except close to the fifty move horizon where every cutoff is refused;
  ROADMAP.md has the match that chose that, and the reference search keeps
  the refusal.
- **eval/**: What a position scores, a file per leaf term and two for what
  they share.
  - **mod.rs**: The material values, the phase weights the taper is read at,
    the accumulator, and the sum the search asks for. The board tells the
    accumulator about every piece placed, removed and moved, so material and
    the piece square score are carried rather than counted; the leaf terms are
    computed at the leaf. Material that cannot mate is answered with a hard
    zero, which with the pair term below is where the score is not a sum over
    the weights. `TERMS`, a descriptor per leaf term, is what the tuner lays
    its slot vector out from. One walk over each side's pieces probes each
    attack set once for both mobility and the king attack zone.
  - **factors.rs**: The pair term, a factorization machine over the piece
    square features: a weight for every pair of pieces, as the inner product
    of two rows of sixteen factors. The accumulator keeps each perspective's
    sum of the rows, so the leaf reads two sums of squares. The table is
    `factors16.rs`, generated from the fit; the `machine-test` feature swaps
    in a seeded rank 8 table so the tests check the term on a table no fit
    chose.
  - **cache.rs**: The direct mapped cache a remembered term is kept in, one
    per term, holding a score under the whole of its key.
  - **mobility.rs**: How many squares each side's pieces cover. Read at every
    leaf and not remembered, since a piece that moves changes what every
    slider looking through its square sees.
  - **shelter.rs**: What stands between each king and the board, its own
    pawns and the enemy pawns coming for it. Computed at the leaf and then
    remembered under the pawns and the two king squares it is a function of,
    in a small table the searcher owns, because a king move rewrites a whole
    side's reading and there is nothing there to keep in step.
  - **pawn_structure.rs**: Each side's passed pawns by rank, its isolated
    pawns and its doubled ones. Remembered the same way in a table of its
    own, under the pawn key alone: it reads neither king, so what misses is a
    pawn move and the capture of a pawn, where the shelter's key misses on a
    king move as well.
  - **king_attack.rs**: How many squares of the enemy king's ring each side's
    knights, bishops, rooks and queens attack, off the same attack sets
    mobility walks but with nothing taken out of them. Read at every leaf and
    not remembered, for mobility's reason.
- **psqt.rs**: The piece square tables. Every piece has a second table
  for the endgame; both phases are packed into one integer so the taper
  costs one multiply.
- **zobrist.rs**: The position hash, updated incrementally as pieces move.
- **bench.rs**: A fixed suite of positions searched to a fixed depth,
  printing exact node counts. This is what a commit's `Bench:` trailer
  states and what CI verifies.
- **recorder.rs**: What the four recorders below share: the reservoir that
  hangs off an engine and keeps one node in every n, the loop that searches
  a suite with one armed, and the lanes that keep their samples apart. An
  engine with nothing armed searches the tree it would without them.
- **residual.rs**: What the shortcuts cost in accuracy. It samples the
  nodes reverse futility and the null move pass answered, and the nodes
  reverse futility could have answered and did not, then replays each one
  under the reference search to see whether the cutoff was one the
  position allowed. Driven by the `residuals` argument.
- **census.rs**: Which move cuts a node off, and what it cut ahead of. One
  row per sampled full width node, whether it cut or ran out of moves, so
  the two can be read against each other. Driven by the `cutoffs`
  argument.
- **reduction.rs**: What trusting a reduced scout decided. One row per
  sampled scout, and a fail low is replayed at the depth its move was
  denied to say whether the reduction threw a move away. Driven by the
  `reductions` argument.
- **effort.rs**: What a rule frees, and where the freed effort goes. The
  one instrument that describes two trees: it searches the suite twice,
  once with a named switch off, and joins the two runs by the node, so a
  row says whether each side reached it and what each spent under it.
  Driven by the `effort` argument.
- **tune.rs**: What a position's evaluation is made of. The evaluation is linear
  in its weights except where material cannot mate and for the pair term,
  which a row carries as a number of its own, so a position's score is a dot
  product and that number, and this writes down the position's side of it,
  one coefficient per weight the position touches. `reconstruct` folds a row
  back against the live tables and has to give the evaluation exactly. Driven by the `terms`
  argument, and read by `scripts/tune.py`.
- **tactics.rs**: 300 tactical positions with a pinned pass count, gated
  in CI.
- **strategy.rs**: 1500 quiet positions, each move graded out of a
  hundred, with a pinned point total, gated in CI beside the pass count.

## Code map: src

- **main.rs**: Argument handling. `bench` and the research commands run
  and exit; no argument starts the UCI loop.
- **uci.rs**: The protocol: what each command means, the options the
  handshake advertises, and what a `go` may spend. Every line reaches it
  through the session loop, on the thread the engine was built on.
- **instruments.rs**: What the research commands take and run. They are
  not the protocol (an interface cannot ask for any of them), which is why
  they are here rather than in uci.rs.
- **session.rs**: The threads a session runs on. A reader owns stdin and
  acts on the commands that cannot wait for a search to end (`stop`,
  `quit`, `isready`); everything else is queued for the session loop,
  which hands each line back to the protocol in order. A search is
  interrupted by setting the stop flag. A pipe closing counts as a quit,
  so a dead GUI cannot leave a search running.
- **params.rs**: Reads the word/value pairs UCI commands are made of, and
  the phrases where a name runs to more than one word, as `Clear Hash` does.
- **command.rs**: What a command line argument is called and what words it
  takes, declared once, so `--help` cannot describe a line the parser does
  not take.
- **time_control.rs**: Reads the time part of a `go` line, and turns a clock
  into a time budget for one move.

## Code map: scripts

Most of `scripts/` is measurement plumbing, described in DEVELOPMENT.md
where each measurement is, or in the script's own header where it is not.
`fit_attention.py` fits the `ATTENTION_*` integers `late_move.rs` carries,
from a `reductions` ledger. The four below are the offline half of the
evaluation tuner, described in [INSTRUMENTS.md](INSTRUMENTS.md), and each
carries its reasoning in its docstring:

- **groups.py**: Which of the three groups (train, selection, sealed) a pair
  of games falls in. Both scripts below import it, so a corpus cannot be built
  to one split and fitted against another.
- **harvest_games.py**: Downloads the strength runs' game artifacts into an
  archive before they expire, then rebuilds the corpus from the whole of it.
- **build_corpus.py**: Archived strength-run pgns in, an epd of unique
  post-book positions out, each labelled from its own group's games.
- **tune.py**: The loss harness and the fit. Reads an `arche terms` run and
  the corpus, rebuilds every row against the weights the run printed, and
  scores, cross validates or fits weight vectors.
