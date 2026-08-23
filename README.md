# Arche
Andrew's Rust Chess Engine

## About

This project is mostly intended for self-edification. The engine is not intended to be
particularly novel or powerful, and most of the effort goes into being able to tell whether a
change to it helped. Every change to the engine states in its commit message how much of the
tree the search looks at afterwards, and a change that claims to be faster states how much
faster it measured.

Since 2026 most of the changes in this repo are written by AI. I had abandoned this project
for want of time, but AI has changed that. AI allows me to explore new ideas — be they my own,
AI generated or borrowed from other engines. I still decide on the roadmap, but I can no longer
call the engine all my own work. It remains first and foremost a fun project that helps me
learn about how chess engines work and performance tuning in Rust.

The board is currently represented using only bitboards (with magic bitboards for move
generation of sliding pieces).

The search is alpha beta with a transposition table, iterative deepening, quiescence search and
MVV-LVA move ordering. Evaluation is material plus piece square tables.

None of this is meant as a reference implementation, and it is not especially idiomatic Rust.
The engine is something to experiment on rather than an example to copy.

## Documentation

- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — building, the tests, the benchmarks and the
  bench, playing a match against a previous version, placing the engine on the ccrl scale, and
  cutting a release
- [docs/ROADMAP.md](docs/ROADMAP.md) — what is not implemented yet, and the limitations of what is
- [docs/LICHESS.md](docs/LICHESS.md) — running the engine as a bot account on lichess
- [CHANGELOG.md](CHANGELOG.md) — what changed in each release

## Usage

The engine does not ship with any GUI. It currently implements a subset of the UCI protocol,
you can use it with an open source GUI such as [Arena](http://www.playwitharena.de/).

The program starts in UCI mode immediately. The one argument it takes is `bench [depth]`, which
searches a fixed set of positions and prints what each search counted, for measuring a
change to the search or the speed of a machine. It is a UCI command as well as an argument.

Binaries for linux, macos and windows are attached to each
[release](https://github.com/aywrite/arche/releases). To build from source:

```
cargo build --release
```

The binary is written to `target/release/arche`. Note that the engine allocates a 256MB
transposition table on startup and that the size is not yet configurable over UCI.

## Strength

Each release plays a short match against its predecessor, and a gauntlet against old releases
of [Stash](https://github.com/mhouppin/stash-bot) which are ranked on the
[ccrl](https://computerchess.org.uk/) blitz list. Both results are added to the release notes.
The estimate is only as good as the number of games behind it, which is why the error bar is
published alongside it.

The engine has not been entered into any rating list itself, and the gauntlet is played faster
and on different hardware than the list it borrows its opponents from, so read the figure it
implies as a placement to within about a hundred points rather than a rating.
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) has the method, and what a match of a given size can
and cannot settle.

## Lichess

`docker/Dockerfile` builds an image containing
[lichess-bot](https://github.com/lichess-bot-devs/lichess-bot), the engine and an opening book,
which is enough to run the engine as a bot account on [lichess.org](https://lichess.org).
Images are published to `ghcr.io/aywrite/arche-lichess-bot`.

```
docker run -e LICHESS_BOT_TOKEN=<token> ghcr.io/aywrite/arche-lichess-bot:latest
```

See [docs/LICHESS.md](docs/LICHESS.md) for the tags, the token the container needs, how the
book is built, and how to build and check an image locally.

## Acknowledgements

- https://stackoverflow.com/questions/30680559/how-to-find-magic-bitboards
- https://stackoverflow.com/questions/16925204/sliding-move-generation-using-magic-bitboard
- https://www.youtube.com/playlist?list=PLZ1QII7yudbc-Ky058TEaOstZHVbT-2hg
- https://github.com/bluefeversoft/vice (MIT)

## License

Arche is free software: you can redistribute it and/or modify it under the terms of the GNU
General Public License as published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version. It is distributed in the hope that it will be
useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
FITNESS FOR A PARTICULAR PURPOSE. See [LICENSE](LICENSE) for the full text.

Copyright (C) 2022-2026 Andrew Wright

The source corresponding to a release binary is the tag it was built from, which GitHub attaches
to the same release page as the binary itself.

The lichess-bot image is an aggregate rather than a combined work. It bundles
[lichess-bot](https://github.com/lichess-bot-devs/lichess-bot), which is AGPL-3.0 and which runs
the engine as a separate process over UCI, and a book built from
[lichess-org/chess-openings](https://github.com/lichess-org/chess-openings), which is CC0. Each
keeps its own terms; only the engine is covered by the license here.
