# Architecture

An overview of how the code is organized and how the main parts work. See
[DEVELOPMENT.md](DEVELOPMENT.md) for how to build, test and measure a change,
and [ROADMAP.md](ROADMAP.md) for what is not implemented yet.

## Overview

The workspace has two crates:

- **arche-core/**: The engine itself. Board, move generation, search,
  evaluation and the transposition table.
- **src/**: The `arche` binary. Wraps the engine in the UCI protocol (the
  text protocol chess GUIs and match runners speak).

The split means the engine can be tested without spawning a process and the
protocol can be tested without running a search.

The board is represented using bitboards: one 64 bit integer per piece type
and one per colour, with one bit per square. Most operations on them compile
down to one or two instructions. The search is alpha beta with iterative
deepening, quiescence search and a transposition table. Evaluation is
material plus piece square tables, tapered between middlegame and endgame.

## Code map: arche-core

- **board.rs**: The position and the rules. Holds the piece bitboards plus
  some state that could be recomputed from them but would be too slow to:
  an array of what stands on each square, the zobrist key, a second key
  over the pawns alone, running totals for material and the piece square
  evaluation, and a ring of the last ~1024 plies (used by the repetition
  and fifty move rules, and to undo moves). All piece placement goes
  through one function, which is what keeps the derived state in sync.
  Debug builds recompute the derived state from scratch after every move
  and assert it matches, so a bug in an incremental update fails tests
  instead of misevaluating quietly.
  Move generation also lives here. It is pseudo-legal: moves are generated
  without checking whether they leave the king in check, and `make_move`
  rejects the ones that do. When already in check the list is first
  filtered down to moves that could address the check, which saves sorting
  and playing moves that would only be rejected.
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
  seeding the next through the transposition table. At the horizon,
  quiescence search keeps following captures until the position goes
  quiet, since evaluating in the middle of an exchange scores a hanging
  queen as material. Also here: reverse futility pruning (answer a node
  from its evaluation when that is already far above what the opponent
  can accept), null move pruning (answer it from a reduced search of the
  position left by passing the move) and the check extension.
- **ordering.rs**: The order moves are tried in. The transposition table's
  move first, then captures by most valuable victim / least valuable
  attacker, then the quiet moves by two memories the search fills as it
  goes: the killers, which are the quiet moves that cut off at this
  distance from the root, and a history table of how often each quiet move
  has cut off anywhere. Alpha beta prunes more the sooner a good move is
  found, so ordering has an outsized effect on tree size.
- **limits.rs**: When to stop searching. A clock, a node budget, a soft
  rule that skips starting an iteration which would get less than half
  done, and a stop flag shared with the interface thread. The flag is read
  in the same place as the clock, roughly every three thousand nodes.
- **value.rs**: A score plus a taint bit recording whether it depended on
  a repetition or fifty move draw somewhere down its line. Such a score is
  only true of the path that produced it, which the table needs to know.
  The mate arithmetic lives here too: what scores count as a forced mate,
  and how one is read back as moves to mate.
- **transposition.rs**: The transposition table: a cache of positions
  searched before, keyed by zobrist hash, holding the score and best move
  found last time. Entries are 16 bytes, four to a cache line, replaced by
  age and depth. A hit can answer a node outright or just say which move
  to try first. Tainted scores are counted and by default trusted anyway,
  except close to the fifty move horizon where every cutoff is refused.
  The policies were played against each other and the cautious one lost
  by about 45 elo, so the error is carried knowingly and the bench prints
  the taint counters on every run. A `reference` configuration keeps the
  cautious search as a baseline for classifying future changes.
- **eval.rs**: What a position scores. The board hosts an accumulator and
  tells it about every piece placed, removed and moved, so material and the
  piece square score are carried rather than counted; anything too dear to
  keep in step is computed at the leaf instead.
- **psqt.rs**: The piece square tables. The pawn and the king have a
  second table for the endgame (they are the pieces the phases disagree
  about); both phases are packed into one integer so the taper costs one
  multiply.
- **zobrist.rs**: The position hash, updated incrementally as pieces move.
- **bench.rs**: A fixed suite of positions searched to a fixed depth,
  printing exact node counts. This is what a commit's `Bench:` trailer
  states and what CI verifies.
- **residual.rs**: What the shortcuts cost in accuracy. It samples the
  nodes reverse futility and the null move pass answered, then replays
  each one under the reference search to see whether the cutoff was one
  the position allowed. Driven by the `residuals` argument.
- **tactics.rs**: 300 tactical positions with a pinned pass count, gated
  in CI.

## Code map: src

- **main.rs**: Argument handling. `bench` runs the suite and exits,
  `residuals` measures the shortcuts and exits, no argument starts the
  UCI loop.
- **uci.rs**: The protocol: what each command means, the options the
  handshake advertises, and what a `go` may spend. Every line reaches it
  through the session loop, on the thread the engine was built on.
- **session.rs**: The threads a session runs on. A reader owns stdin and
  acts on the commands that cannot wait for a search to end (`stop`,
  `quit`, `isready`); everything else is queued for the session loop,
  which hands each line back to the protocol in order. A search is
  interrupted by setting the stop flag. A pipe closing counts as a quit,
  so a dead GUI cannot leave a search running.
- **params.rs**: Reads the word/value pairs UCI commands are made of, and
  the phrases where a name runs to more than one word, as `Clear Hash` does.
- **time_control.rs**: Turns a clock into a time budget for one move.

## Measurement

The engine measures itself, and most of the project's conventions hang off
that: exact node counts pinned per commit, speed claims measured against a
named commit, and play changes settled by matches. DEVELOPMENT.md covers
all of it.
