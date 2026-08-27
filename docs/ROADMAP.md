# Roadmap

What the engine does not do yet, and what it does badly enough to be worth writing down.
See [DEVELOPMENT.md](DEVELOPMENT.md) for how to measure whether one of these helped.

## Not implemented yet

The measurement spine — the bench, the commit trailers, a reference search to
compare against — is in place, so each of these arrives with its numbers: a
`Bench:` trailer always, and an `Elo:` trailer from an SPRT when it changes how
the engine plays. Roughly in the order they look worth doing.

- null move pruning
- killer moves and a history heuristic, staged in the ordering module
- principal variation search, and a delta margin in quiescence
- late move reductions
- evaluate drawn positions
- the rest of evaluation: mobility, and special cases such as the bishop pair and open files
- the rest of the uci protocol
  - the only options advertised are `Hash` and a `Threads` fixed at one, so everything else an
    interface might set, `Ponder` and `Clear Hash` among them, is refused rather than acted on
  - `ponderhit`, `debug` and `register` are not handled, so pondering is still out of reach
    even though `stop` is answered now
  - a move in a `position` line that cannot be played is reported the same way whether
    no move of that name exists here or the move exists and leaves the king in check.
    `make_move_str` answers with a bool, so the interface is told which move failed and
    not what was wrong with it
- read an opening book in the engine, only the lichess-bot image has one at the moment and it is
  lichess-bot that reads it rather than the engine
- winboard

## Known limitations

- a transposition score that came from a repetition or fifty move draw is trusted, except
  within four plies of the fifty move horizon, where every cutoff is refused. The search can
  therefore read a draw down a path that could not reach it; the policies were played against
  each other and refusing such scores lost about forty five elo to trusting them, so the
  error is carried knowingly, measured by the graph history counters, and the refusing search
  remains as the reference. `taint refuse` restores the refusal alone, on top of whatever
  else the default does; the full reference has no command line spelling
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
- Requiring the game's own two prior occurrences before a pre-root repetition
  scores as a draw, the line Stockfish draws. Lost -22 ±18 over 446 games at
  5+0.05 (sprt [0, 10] failed, PR #107): an eval this simple is better off
  taking every draw the history makes available than re-fighting positions it
  half-understands, and the stricter rule spends depth keeping alive games it
  then loses. Tapered evaluation has since landed and this was not re-measured
  against it; worth re-asking once king safety lands too.
- Prefetching a child's transposition slot straight after `make_move`, 6.7%
  slower over six interleaved rounds. The prefetch sits immediately before the
  recursive call and the child probes the table almost first, so there is no
  latency to hide and all that is added is an index multiply on every made
  move. Inside `make_move`, after the key is finalised, is the only placement
  that could pay, and it needs the key folded up front first.
