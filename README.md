# Arche
Andrew's Rust Chess Engine

## Usage

The engine does not ship with any GUI. It currently implements a subset of the UCI protocol, you can use it with an open source GUI such as [Arena](http://www.playwitharena.de/).

The program does not accept posix style arguments it will immediately start in UCI mode.

## TODO

[x] transposition table
- null move pruning
- killer moves
- perft command from uci
- fix magics to load on engine start
- better evaluation
  - mobility in evaluation
  - evaluate drawn positions
  - special cases (bishop pair, open files etc)
- winboard

## Acknowledgements

- https://stackoverflow.com/questions/30680559/how-to-find-magic-bitboards
- https://stackoverflow.com/questions/16925204/sliding-move-generation-using-magic-bitboard
- https://www.youtube.com/playlist?list=PLZ1QII7yudbc-Ky058TEaOstZHVbT-2hg
- https://github.com/bluefeversoft/Vice_Chess_Engine
