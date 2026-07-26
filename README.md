# Arche
Andrew's Rust Chess Engine

## About

This project is mostly indented for self-edification. The engine is not intended to be particularly novel or powerful.

The board is currently represented using only bitboards (with magic bitboards for move generation of sliding pieces).

## Usage

The engine does not ship with any GUI. It currently implements a subset of the UCI protocol, you can use it with an open source GUI such as [Arena](http://www.playwitharena.de/).

The program does not accept posix style arguments it will immediately start in UCI mode.

## TODO

[x] transposition table
- null move pruning
- killer moves
- known issues
  - fail low (alpha) nodes are never stored in the transposition table, only exact and beta nodes
  - transposition table scores don't account for repetition or fifty move history, make_move_str
    clears the key for played moves as a workaround
  - hash table replacement doesn't compare entry depth and protects exact entries even when the
    stored key is for a different position
  - the replacement strategy change in 0b858f8 uses game ply rather than depth despite its title,
    and dropped the same key guard added by the earlier collision fixes (51657b2, 6b2aac6)
  - history array is fixed at 375 plies and ply is initialised to twice the full move number, so
    very long games can overflow it
  - each engine instance allocates a 500MB hash table up front (including in tests)
  - quiescence stand pat raises alpha on score >= alpha instead of score > alpha
  - pv_line panics if the root entry has been evicted from the hash table
  - en passant is hashed for every double pawn push, even when no capture is possible, which
    slightly reduces transposition hits
- perft command from uci
- fix magics to load on engine start
- better evaluation
  - mobility in evaluation
  - evaluate drawn positions
  - special cases (bishop pair, open files etc)
- winboard
- Implement the rest of the UCI protocol
- opening books

## Acknowledgements

- https://stackoverflow.com/questions/30680559/how-to-find-magic-bitboards
- https://stackoverflow.com/questions/16925204/sliding-move-generation-using-magic-bitboard
- https://www.youtube.com/playlist?list=PLZ1QII7yudbc-Ky058TEaOstZHVbT-2hg
- https://github.com/bluefeversoft/Vice_Chess_Engine
