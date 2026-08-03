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
master. A release is tagged `latest`, `<major>.<minor>`, `<version>` and `v<version>`, the last
of which matches the git tag and is the one quoted in the release notes along with its digest.

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

- null move pruning
- killer moves
- better evaluation
  - mobility in evaluation
  - evaluate drawn positions
  - special cases (bishop pair, open files etc)
- read an opening book in the engine, only the lichess-bot image has one at the moment and it is
  lichess-bot that reads it rather than the engine
- the rest of the uci protocol
  - `setoption` is not handled and no options are advertised, so the 500MB transposition table
    can only be changed in code
  - `stop` is not handled, which rules out pondering and `go infinite`
  - `ponderhit`, `debug` and `register` are not handled either
- winboard
- known issues
  - fail low (alpha) nodes are never stored in the transposition table, only exact and beta nodes
  - transposition table scores don't account for repetition or fifty move history, make_move_str
    clears the key for played moves as a workaround
  - a fen is barely validated, so one without a king is accepted and then panics as soon as the
    position is searched
  - the history array is a fixed size and ply is initialised to twice the full move number, so a
    game of more than about five hundred moves runs past the end of it. A fen with a full move
    number that high panics on the first search rather than after five hundred moves
  - en passant is hashed for every double pawn push, even when no capture is possible, which
    slightly reduces transposition hits

## Ideas

The TODO list is the things every engine has that this one does not yet. This list is the
opposite: things that would make it unlike other engines. They are speculative and none of
them is worth much elo, which is rather the point, the engine is not going to compete on
strength. Roughly in order of how well they fit what is already here.

- **A machine-checked move generator.** Perft proves make/unmake works for the positions it
  enumerates and says nothing about the rest. [Kani](https://github.com/model-checking/kani)
  is a bounded model checker for rust and could prove properties over all inputs in a bounded
  space instead: that make/unmake is an involution, that the zorbrist key is a pure function of
  the position, that the magic index is always in range, that the bitboards stay consistent
  with each other. The last of those is the "a fen without a king panics" bug in the known
  issues. proptest already covers some of this by sampling, this is the same idea taken to
  exhaustion. No engine appears to have done it.
- **Search-tree diffing.** `test_node_counts_have_not_moved` says a change moved the tree from
  218,290 nodes to some other number. It does not say where. Emitting a canonical transcript of
  the search and diffing two of them structurally would, and the benchmark workflow already has
  somewhere to post the answer. This is the one that pays for itself the moment there is any
  pruning to debug.
- **Elo attributed per commit.** The strength workflow already plays any ref against any other
  and the commit scopes already say which commits touch the engine. Running it per engine commit
  rather than per release would give a changelog where each entry carries what it measured, and
  an honest record of how many changes measured as nothing.
- **Conspiracy numbers for time management.** Time management here, and largely everywhere, is
  proxies for one quantity: how many leaves would have to change their mind to change the move
  at the root. That quantity is computable, was studied in the late eighties and is not used by
  anything modern. `time_control.rs` computes a budget once before the search; letting the
  search revise it is the structural change needed.
- **Endgame tables computed on demand.** Rather than shipping syzygy, run retrograde analysis in
  the background for the exact material left on the board once it is down to four or five men.
  Four men is seconds and megabytes. It would help most where material plus untapered piece
  square tables is at its worst.
- **Calibrated weakness.** Playing at a target rating is normally done by cutting depth, which
  produces an engine that plays well and then hangs a queen. Sampling the root move from the
  distribution of root scores, at a temperature tuned so the error profile matches a rating
  band, is measured by calibration against real games rather than by elo. It is a thing a weak
  engine can be best at.

## Acknowledgements

- https://stackoverflow.com/questions/30680559/how-to-find-magic-bitboards
- https://stackoverflow.com/questions/16925204/sliding-move-generation-using-magic-bitboard
- https://www.youtube.com/playlist?list=PLZ1QII7yudbc-Ky058TEaOstZHVbT-2hg
- https://github.com/bluefeversoft/Vice_Chess_Engine
