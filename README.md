# Arche
Andrew's Rust Chess Engine

## About

Arche is a UCI chess engine in Rust. Most of the effort goes into being able to tell whether a
change to it helped: every change states in its commit message how much of the tree the search
looks at afterwards, one that changes how the engine plays is measured in games before it
merges, and one that claims to be faster states how much faster it measured.

The board is bitboards, with magic bitboards for move generation of sliding pieces and a square
array beside them so that asking what stands on a square is a load rather than a walk down the
boards. The search is alpha beta with a transposition table, iterative deepening, quiescence
search, reverse futility pruning and a null move pass. Captures are ordered by MVV-LVA and the
quiet moves by the ones that have cut off before. Evaluation is material plus piece square
tables, tapered between a middlegame and an endgame score.

### Background

Since 2026 most of the changes in this repo are written by AI. I had abandoned this project
for want of time, but AI has changed that. AI allows me to explore new ideas — be they my own,
AI generated or borrowed from other engines. I still decide on the roadmap, but I can no longer
call the engine all my own work. It remains first and foremost a fun project that helps me
learn about how chess engines work and performance tuning in Rust.

The engine is something to experiment on rather than an example to copy.

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how the code is organized and how the main
  parts work
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — building, the tests, the benchmarks and the
  bench, playing a match against a previous version, placing the engine on the ccrl scale, and
  cutting a release
- [docs/ROADMAP.md](docs/ROADMAP.md) — what is not implemented yet, and the limitations of what is
- [docs/LICHESS.md](docs/LICHESS.md) — running the engine as a bot account on lichess
- [CHANGELOG.md](CHANGELOG.md) — what changed in each release

## Usage

The engine does not ship with any GUI. It currently implements a subset of the UCI protocol,
so an open source GUI such as [Arena](http://www.playwitharena.de/) can drive it.

The program starts in UCI mode immediately. Two arguments do anything else.
`bench [depth] [hash <MB>] [taint refuse|trust|skip|rule50] [audit]` searches a fixed set of
positions and prints what each search counted, for measuring a change to the search or the
speed of a machine, and is a UCI command as well as an argument.
`residuals [depth] [every <n>] [taint refuse|trust|skip|rule50]` searches the same positions and
then asks a search with the shortcuts off what the nodes they answered were really worth.
`--version` and `--help` are answered too.

Binaries for linux, macos and windows are attached to each
[release](https://github.com/aywrite/arche/releases), each with a sha256 checksum and a build
provenance attestation. The x86-64 archives come in three builds: the plain one runs on
anything, and `-v2` and `-v3` use progressively newer instructions — pick the newest your cpu
supports (`-v3` wants avx2, which is most machines since about 2013). All three search the
same tree; the newer ones just walk it faster. The checksum says a download arrived intact.
The attestation says where it came from, and is answered for by github rather than by the page
the download sits on:

```
gh attestation verify arche-v<version>-<target>.tar.gz --repo aywrite/arche
```

To build from source:

```
cargo build --release
```

The binary is written to `target/release/arche`. The engine allocates a 256MB transposition
table on startup; `setoption name Hash value <megabytes>` gives it one of another size, between
1 and 16384MB, and `setoption name Clear Hash` empties the one it has without resizing it. It
searches on one thread and says so, so a `Threads` of anything but one is reported and then
ignored.

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
