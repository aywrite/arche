# Development

## Building

```
cargo build --release
```

The binary is written to `target/release/arche` (`arche.exe` on windows). It starts
in uci mode immediately, it does not take any arguments.

The release profile uses link time optimisation and a single codegen unit, so a
release build is noticeably slower to compile than a debug one but is several
times faster to search. Always measure with a release build.

## Tests

```
cargo test --workspace --release
```

The perft tests dominate the runtime, which is why the release profile is the
usual choice. Run them at least once without `--release` before changing
anything that indexes into the history or does arithmetic on ply counts, the
debug profile keeps the overflow checks and debug assertions that catch that
class of bug:

```
cargo test --workspace
```

## Lints

Both of these are gated in ci:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The pre-commit configuration runs them too, along with a check that the commit
message is a conventional commit, which is what the changelog is generated from:

```
pip install pre-commit
pre-commit install --install-hooks
```

## Commit messages

The changelog separates changes to the engine from changes to everything around
it, which it works out from the type and the scope:

```
perf(search): Index the hash table without dividing
perf(bench): Stop the search benchmark timing a 500MB memset
```

The first is the engine getting faster and belongs under **Performance**. The
second is a benchmark getting faster, which says nothing about how the engine
plays, and belongs under **Development**. Only the scope tells them apart.

So the scope is required, and has to be one of these. The build ones put a
commit under **Development** whatever its type is:

| scope | covers |
| --- | --- |
| `bench` | the criterion benchmarks |
| `book` | the opening book generator |
| `ci` | github workflows and the pre-commit configuration |
| `deps`, `deps-dev` | dependency bumps, what dependabot uses |
| `docker` | the lichess-bot image |
| `lint` | fmt and clippy fallout |
| `release` | cargo-release, git-cliff and the release workflows |

The engine ones are grouped by type instead:

| scope | covers |
| --- | --- |
| `board` | the board representation and make/unmake |
| `eval` | evaluation and the piece square tables |
| `movegen` | move generation, magics and bitboards |
| `search` | alpha beta, quiescence and the transposition table |
| `uci` | the protocol and time management |
| `zorbrist` | the hash keys, spelled as the module is |

`feat`, `fix`, `perf`, `refactor` and `docs` get a section each, and `build`,
`chore`, `ci`, `style` and `test` all go to **Development**.

The commit-msg hook rejects a scope that is not on the list, which is the point:
a mistyped `perf(benchmark)` would otherwise be published as if the engine had
got faster. Adding a scope means adding it to the hook arguments in
`.pre-commit-config.yaml`, and to `cliff.toml` as well if it is a build one.
Merge commits are exempt.

`git-cliff --unreleased` prints what the next release would say, which is the
quickest way to check a scope landed where it should.

## Benchmarks

```
cargo bench -p basic_engine --bench benchmark
```

Criterion stores its previous results under `target/criterion`, so the useful
sequence is to benchmark master, apply a change, and benchmark again. Wall clock
numbers move around on a loaded machine, so treat anything under about ten
percent as noise.

The benchmark workflow runs the same benchmarks on every pull request and
compares them against the last run on master. It only reports, it will not fail
a build, for the same reason.

## Playing a match against a previous version

Benchmarks measure speed, they do not measure whether the engine plays better.
A change can search twice as fast and still lose games. For that, play the two
versions against each other with
[fastchess](https://github.com/Disservin/fastchess) and an opening book such as
[8moves_v3](https://github.com/official-stockfish/books).

```
git checkout --detach v0.3.7 && cargo build --release && cp target/release/arche /tmp/old
git checkout - && cargo build --release && cp target/release/arche /tmp/new

fastchess \
  -engine name=new cmd=/tmp/new \
  -engine name=old cmd=/tmp/old \
  -each proto=uci tc=10+0.1 -startup-ms 60000 \
  -openings file=8moves_v3.pgn format=pgn order=random \
  -rounds 200 -repeat 2 -concurrency 2 -recover
```

Notes on the flags:

- `-startup-ms 60000` is needed because the engine generates its magic bitboards
  and allocates the transposition table before it answers `uci`, which is longer
  than the ten second default when several copies start at once.
- `-repeat 2` plays each opening twice with the colours reversed, which removes
  most of the advantage of drawing a good opening.
- `-recover` keeps the match going if an engine crashes rather than aborting.
  Watch for `disconnect` in the output, a crashing engine scores zero for that
  game and the result is no longer a fair estimate of strength.

The error bar shrinks with the square root of the number of games. Fifty games
only detects very large differences, a few hundred is needed before a result of
twenty or thirty elo means anything.

The **Strength** workflow does the same thing on a runner. Run it from the
actions tab against any branch or tag and give it a number of games, and it
plays that version against the last full release, ignoring release candidates.
The result goes to the run summary.

The release workflow calls the same workflow with fifty games, and that run is
the only one that appends its result to the release notes.

## Cutting a release

Releases are driven by [cargo-release](https://github.com/crate-ci/cargo-release)
with the changelog generated by [git-cliff](https://github.com/orhun/git-cliff)
from the conventional commit messages. The whole workspace shares one version,
declared once in `[workspace.package]` in the root `Cargo.toml`.

### From the actions tab

Run the **Prepare release** workflow from `master` and pick how much to bump the
version by. It runs the tests, bumps the version, writes the changelog and opens
a pull request with the result. `rc` gives a release candidate, `0.3.7` becomes
`0.3.8-rc.1`.

Merging that pull request tags the release and starts the build. Closing it
without merging calls the release off, nothing is tagged or published until it
lands.

Two things are worth knowing about why it is shaped this way:

- `master` only takes changes through a pull request, so a workflow cannot push
  the release commit to it directly. The release commit goes through a pull
  request like anything else.
- A tag pushed by a workflow does not set off another workflow. Github
  suppresses that so workflows cannot trigger each other in a loop, and
  `workflow_dispatch` is the documented exception, which is why the tag workflow
  starts the release workflow explicitly rather than leaving it to the tag
  filter.

**Tag release** decides whether to tag by comparing the version in `Cargo.toml`
against the existing tags, rather than by looking at the commit message, because
the message of a merged pull request depends on whether it was merged, squashed
or rebased.

Github requires a maintainer to approve the first workflow run on a pull request
opened by a workflow, so the checks on a release pull request may need the
**Approve and run** button before they start.

The release workflow can also be run from the actions tab on its own, against an
existing tag, if a release needs redoing.

### By hand

The same thing locally, if something has gone wrong or a release needs to be
inspected before it goes anywhere.

```
cargo install cargo-release git-cliff
```

From an up to date `master`:

```
cargo release patch        # or minor / major, prints what it would do
cargo release patch --execute
```

The first of those is a dry run, but it still runs the pre release hook, so it
leaves a new section prepended to `CHANGELOG.md` that has to be reverted if the
release is not going ahead.

`release.toml` sets `push = false`, so nothing leaves the machine until:

```
git push
git push origin vX.Y.Z
```

Pushing the tag is what triggers the release workflow, which runs the tests,
creates the github release from the changelog, builds and uploads binaries for
linux, macos and windows, and then plays the strength match. The docker
workflow publishes the lichess-bot image from the same tag.
