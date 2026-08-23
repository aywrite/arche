# Roadmap

What the engine does not do yet, and what it does badly enough to be worth writing down.
See [DEVELOPMENT.md](DEVELOPMENT.md) for how to measure whether one of these helped.

## Not implemented yet

Roughly in the order they look worth doing.

- null move pruning
- killer moves
- evaluate drawn positions
- the rest of evaluation: mobility, and special cases such as the bishop pair and open files
- the rest of the uci protocol
  - `setoption` is not handled and no options are advertised, so the 256MB transposition table
    can only be changed in code
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
