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
