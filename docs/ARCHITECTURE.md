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
and one per colour, with one bit per square. Most operations on them compile
down to one or two instructions. The search is alpha beta with iterative
deepening, quiescence search and a transposition table. Evaluation is
material plus piece square tables, tapered between middlegame and endgame.

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
  seeding the next through the transposition table. At the horizon,
  quiescence search keeps following captures until the position goes
  quiet, since evaluating in the middle of an exchange scores a hanging
  queen as material. Also here: principal variation search (ask a move
  after the first whether it beats the best so far before searching it
  properly), reverse futility pruning (answer a node from its evaluation
  when that is already far above what the opponent can accept), null move
  pruning (answer it from a reduced search of the position left by passing
  the move), the late move reduction (scout a quiet move tried late a ply
  shallower and trust it when it comes back low) and the check extension.
- **ordering.rs**: The order moves are tried in. The transposition table's
  move first, then the captures the swap prices as winning or even, by
  what each wins with most valuable victim / least valuable attacker
  breaking the ties, then the quiet moves by two memories the search fills
  as it goes: the killers, which are the quiet moves that cut off at this
  distance from the root, and a history table of how often each quiet move
  has cut off anywhere. The losing captures close the list. Alpha beta
  prunes more the sooner a good move is found, so ordering has an outsized
  effect on tree size.
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
- **psqt.rs**: The piece square tables. Every piece has a second table
  for the endgame; both phases are packed into one integer so the taper
  costs one multiply.
- **zobrist.rs**: The position hash, updated incrementally as pieces move.
- **bench.rs**: A fixed suite of positions searched to a fixed depth,
  printing exact node counts. This is what a commit's `Bench:` trailer
  states and what CI verifies.
- **recorder.rs**: What the three recorders below share. The reservoir
  that hangs off an engine and keeps one node in every n, the loop that
  searches a suite with one armed, the spread the three key by, and the
  window a sample reads off the node. An engine with nothing armed
  searches the tree it searched before there was a reservoir at all.
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
- **tune.rs**: What a position's evaluation is made of. The evaluation is
  linear in the tables and the material values, so a position's score is a
  dot product, and this writes down the coefficients: one per weight the
  position touches, in the side to move's frame. `reconstruct` folds a row
  back against the live tables and has to give the evaluation exactly,
  which is asserted on every row printed as well as over three suites in a
  test. Driven by the `terms` argument, and read by `scripts/tune.py`.
- **tactics.rs**: 300 tactical positions with a pinned pass count, gated
  in CI.
- **strategy.rs**: 1500 quiet positions, each move graded out of a
  hundred, with a pinned point total, gated in CI beside the pass count.

## Code map: src

- **main.rs**: Argument handling. `bench` runs the suite and exits, and so
  do the four research commands, `residuals`, `cutoffs`, `reductions` and
  `terms`. No argument starts the UCI loop.
- **uci.rs**: The protocol: what each command means, the options the
  handshake advertises, and what a `go` may spend. Every line reaches it
  through the session loop, on the thread the engine was built on.
- **instruments.rs**: What the four research commands take, and what each
  one runs. Not the protocol (an interface cannot ask for any of them, and
  would not wait for the answer), which is why they are here rather than
  beside the commands they are spelled like.
- **session.rs**: The threads a session runs on. A reader owns stdin and
  acts on the commands that cannot wait for a search to end (`stop`,
  `quit`, `isready`); everything else is queued for the session loop,
  which hands each line back to the protocol in order. A search is
  interrupted by setting the stop flag. A pipe closing counts as a quit,
  so a dead GUI cannot leave a search running.
- **params.rs**: Reads the word/value pairs UCI commands are made of, and
  the phrases where a name runs to more than one word, as `Clear Hash` does.
- **time_control.rs**: Reads the time part of a `go` line, and turns a clock
  into a time budget for one move.

## Code map: scripts

Most of `scripts/` is measurement plumbing, described in DEVELOPMENT.md
where each measurement is. Three of them are the offline half of the
evaluation tuner, and they have tests under `scripts/tests` gated by the
Scripts workflow:

- **groups.py**: Which of the three groups a game falls in, by the first
  byte of its key. The two scripts below both need it, and a second copy of
  the mapping would be a corpus built to one split and fitted against
  another, which neither run would say a word about.
- **harvest_games.py**: The strength runs' game artifacts down into an
  archive, then the corpus rebuilt from the whole of it. What keeps the
  games from expiring unharvested.
- **build_corpus.py**: Archived strength-run pgns in, an epd of unique
  post-book positions out, each carrying the game it belongs to, the result
  from the side to move's point of view, and how many times it was reached.
  The game is named by the sha256 of its movetext, which is what the split
  reads. A position two games reached belongs to the group of the lower key
  and is labelled and weighted by that group's games alone, and the
  appearances in other groups are dropped rather than merged.
- **tune.py**: The loss harness and the fit. Reads an `arche terms` run and
  the corpus above, rebuilds every row against the weights the run printed,
  and either scores weight vectors on the selection games, cross validates one
  way of fitting against another, or fits new weights. The unit throughout is
  the game and not the position, because the label is the game's, and the
  objective weights a position by how often the corpus reached it. There are
  three groups: three fifths of the games train, a fifth ranks the ridge, and
  a fifth is sealed. The sealed rows are not in the matrices anything here
  scores, so no command can read a sealed row, and the duplicate rule above
  keeps a sealed game's result out of every label a fit sees. Nothing here
  knows how to evaluate a position: the engine states the coefficients and
  states the weights, and a row this cannot rebuild stops the run.

## Measurement

The engine measures itself, and most of the project's conventions hang off
that. DEVELOPMENT.md covers the bench and the matches, and
[INSTRUMENTS.md](INSTRUMENTS.md) covers the measurements of the search itself
and of the evaluation.
