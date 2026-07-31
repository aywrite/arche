# Arche
Andrew's Rust Chess Engine

## About

This project is mostly indented for self-edification. The engine is not intended to be particularly novel or powerful.

The board is currently represented using only bitboards (with magic bitboards for move generation of sliding pieces).

The search is alpha beta with a transposition table, iterative deepening, quiescence search and
MVV-LVA move ordering. Evaluation is material plus piece square tables.

## Usage

The engine does not ship with any GUI. It currently implements a subset of the UCI protocol, you can use it with an open source GUI such as [Arena](http://www.playwitharena.de/).

The program does not accept posix style arguments it will immediately start in UCI mode.

Binaries for linux, macos and windows are attached to each
[release](https://github.com/aywrite/arche/releases). To build from source:

```
cargo build --release
```

The binary is written to `target/release/arche`. Note that the engine allocates a 500MB
transposition table on startup and that the size is not yet configurable over UCI.

## Strength

Each release plays a short match against its predecessor and the result is added to the
release notes. The estimate is only as good as the number of games behind it, which is why
the error bar is published alongside it.

The engine has not been entered into any rating list, so there is no comparable rating
against other engines.

## Development

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for how to run the tests and benchmarks,
play a match against a previous version, and cut a release.

## Lichess

`docker/Dockerfile` builds an image containing
[lichess-bot](https://github.com/lichess-bot-devs/lichess-bot), the engine and an opening book,
which is enough to run the engine as a bot account on [lichess.org](https://lichess.org).
Images are published to `ghcr.io/aywrite/arche-lichess-bot`, tagged `master` on every push to
master and `latest`, `<major>.<minor>` and `<version>` on every release tag.

```
docker run -e LICHESS_BOT_TOKEN=<token> ghcr.io/aywrite/arche-lichess-bot:master
```

The token is a lichess API token for a bot account with the `bot:play` scope. The engine
allocates a 500MB transposition table, so give the container at least 1GB of memory. To change
any other setting, mount a replacement over `/lichess-bot/config.yml`; the defaults are in
`docker/config.yml`.

The book is generated at build time by `docker/build_book.py` from
[lichess-org/chess-openings](https://github.com/lichess-org/chess-openings) (CC0). Each move is
weighted by the number of named openings that play it, so common theory is chosen far more often
than novelties.

To build and check an image locally:

```
docker build -f docker/Dockerfile -t arche-lichess-bot .
docker/smoke_test.sh arche-lichess-bot
```

## TODO

[x] transposition table
- null move pruning
- killer moves
- known issues
  - fail low (alpha) nodes are never stored in the transposition table, only exact and beta nodes
  - transposition table scores don't account for repetition or fifty move history, make_move_str
    clears the key for played moves as a workaround
  - hash table size is only configurable from code, there is no UCI Hash option, so every engine
    allocates the 500MB default
  - the history array is a fixed size and ply is initialised to twice the full move number, so a
    game of more than about five hundred moves would still run past the end of it
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
