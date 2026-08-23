# Roadmap

What the engine does not do yet, and what it does badly enough to be worth writing down.
See [DEVELOPMENT.md](DEVELOPMENT.md) for how to measure whether one of these helped.

## Not implemented yet

The measurement spine — the bench, the commit trailers, a reference search to
compare against — lands before the first change to the search, so that every
later commit carries its numbers and the series has no gap in it. Roughly in
the order they look worth doing after that.

- null move pruning
- killer moves
- evaluate drawn positions
- the rest of evaluation: mobility, and special cases such as the bishop pair and open files
- the rest of the uci protocol
  - the only options advertised are `Hash` and a `Threads` fixed at one, so everything else an
    interface might set, `Ponder` and `Clear Hash` among them, is refused rather than acted on
  - `stop` is not handled, which rules out pondering and `go infinite`
  - `ponderhit`, `debug` and `register` are not handled either
- read an opening book in the engine, only the lichess-bot image has one at the moment and it is
  lichess-bot that reads it rather than the engine
- winboard

## Known limitations

- fail low nodes (upper bounds) are never stored in the transposition table, only exact
  scores and fail highs
- a transposition score that came from a repetition or fifty move draw is refused rather than
  trusted, so the search cannot read a draw down a path that could not reach it. The reverse
  direction is still open: a score stored with the draw out of reach can be read by a path
  with the draw in reach, say a fifty move counter about to run out, and be trusted
- a fen is only validated as far as what the search cannot survive, a king a side and the side
  not to move being out of check. A position which is illegal in other ways, such as one with
  nine pawns or castling rights without a rook, is accepted and played from

## Measured and rejected

Ideas that look right on paper and have already been tried. Do not propose one
of these again without saying what is different this time.

- Carrying the moving piece in `Play`, to save the `get_piece_index` walks in
  make, unmake and MVV-LVA. Implemented correctly, node counts identical, and
  4-6% slower: the struct grows from six bytes to seven, which takes the inline
  `MoveList` from 384 bytes to 448. It would only pay bit-packed into the spare
  bits of `from` and `to`, which is a different change to measure.
- A `MoveList` inline capacity other than 64. Thirty-two and forty-eight are
  4.2% and 3.4% slower, ninety-six and 128 marginally slower. The list is a
  stack local at every recursion frame, so inline bytes multiply by depth and
  trade against a 48KB L1D. Spilling is already negligible; there is nothing
  there to fix.
- Prefetching a child's transposition slot straight after `make_move`, 6.7%
  slower over six interleaved rounds. The prefetch sits immediately before the
  recursive call and the child probes the table almost first, so there is no
  latency to hide and all that is added is an index multiply on every made
  move. Inside `make_move`, after the key is finalised, is the only placement
  that could pay, and it needs the key folded up front first.
