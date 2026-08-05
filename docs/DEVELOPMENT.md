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
usual choice for a quick pass. The debug run is the one that checks the most,
so run it before landing a change: the debug profile keeps the overflow checks
and the board's state-in-step assertions, which verify the position key, the
eval accumulators and the en passant rule against a recompute on every move
made. Release compiles all of that out, so a green release run alone says
nothing about them.

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
| `docs` | the readme and this file, when the change spans more than one area |
| `lint` | fmt and clippy fallout |
| `release` | cargo-release, git-cliff and the release workflows |

The engine ones are grouped by type instead:

| scope | covers |
| --- | --- |
| `board` | the board representation and make/unmake |
| `eval` | evaluation and the piece square tables |
| `magic` | move generation, the magic bitboards and their tables |
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

The benchmark workflow runs the same benchmarks on every pull request, once for
the commit the pull request would land on and once for the pull request itself,
both on the same runner. It writes the comparison to the job summary. It only
reports and will not fail a build, for the same reason: even measured back to
back on one machine, criterion calls single digit differences significant.

What does hold search behaviour still is `test_node_counts_have_not_moved` in
`basic_engine/src/engine.rs`. The number of nodes a search visits is exact
rather than timed, so it says the same thing on any machine. A deliberate change
to the search is expected to move it, and the numbers are updated in the same
commit so the diff shows how much more, or less, of the tree is being looked at.

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
  -each proto=uci tc=10+0.1 -startup-ms 20000 \
  -openings file=8moves_v3.pgn format=pgn order=random \
  -rounds 200 -repeat -concurrency 2 -recover
```

Notes on the flags:

- `-startup-ms 20000` was needed when the engine allocated a 500MB
  transposition table before answering `uci`, which four copies starting at once
  could take longer over than the ten second default allows. The default is now
  128MB and the magic bitboards are constants rather than searched for, so the
  allowance is far more generous than it needs to be.
- `-repeat` plays each opening twice with the colours reversed, which removes most
  of the advantage of drawing a good opening. It takes no argument, it is a spelling
  of `-games 2`.
- `-recover` keeps the match going if an engine crashes rather than aborting.
  Watch for `disconnect` in the output, a crashing engine scores zero for that
  game and the result is no longer a fair estimate of strength.

The error bar shrinks with the square root of the number of games, so roughly
`800 / sqrt(games)` elo is the smallest difference a match can tell from none.
Fifty games settles nothing below about a hundred elo. A change worth ten needs
thousands of games, which is worth knowing before spending an afternoon on one
that cannot be measured:

| games | smallest difference worth believing |
| --- | --- |
| 50 | ~110 elo |
| 500 | ~35 elo |
| 5000 | ~11 elo |

The **Strength** workflow does the same thing on a runner. Run it from the
actions tab and it plays one version against another, reporting to the run
summary.

Both sides can be named, as a branch, a tag, a commit or a pull request number:

- `candidate` is what is being tested, and defaults to the ref the workflow was
  run against
- `baseline` is what it is measured against, and defaults to the last full
  release, ignoring release candidates
- `games` and `time_control` are the size and the speed of the match

So a change can be measured before it is merged by running the workflow with
`candidate` set to the pull request number and leaving the rest alone, or two
arbitrary commits compared by naming both.

The release workflow calls the same workflow with fifty games, and that run is
the only one that appends its result to the release notes.

## Placing the engine on the ccrl scale

A match against the previous version says which of the two is better. It cannot
say where either of them sits, because both sides of it are this engine. For a
number that means something next to other engines, the opponents have to be
engines that already have a rating.

The **Calibrate** workflow plays a gauntlet against old releases of
[Stash](https://github.com/mhouppin/stash-bot), which are ranked on the
[ccrl](https://computerchess.org.uk/) blitz list, build in about a second from a
plain makefile and are eighty kilobytes each. It holds each opponent at its
published rating and fits the one number that is unknown, which is ours:

```
python3 scripts/rating_estimate.py gauntlet.pgn arche stash-v11:1690,stash-v12:1886
```

Locally the same thing is the fastchess command from the previous section with
more `-engine` arguments and `-tournament gauntlet`, which plays the first
engine against all the others.

Every engine in it is given `option.Hash=256`, the size ccrl plays these
opponents at. Ours was fixed at 500MB until it had a Hash option, so the match
used to hand us a memory advantage the published ratings were not played with.

### Choosing the opponents

`ladder` is a list of stash tags and the rating each one holds on ccrl blitz.
The rungs that are worth playing are the ones close enough to trade games with:
a pairing that ends 25-0 puts no upper bound on the winner, so it contributes
almost nothing however many games it is given. The list is an input so it can be
moved up as the engine improves.

These are the versions around the range the engine is in, and whether ccrl
ranked the version itself or the figure is a community estimate from the games
around it:

| tag | ccrl blitz | |
| --- | --- | --- |
| v9 | 1275 | ranked |
| v10 | 1620 | estimated |
| v11 | 1690 | ranked |
| v12 | 1886 | ranked |
| v13 | 1972 | ranked |
| v14 | 2060 | ranked |
| v17 | 2298 | ranked |

### Reading the result

The margin printed with the figure is how much a score of that size wobbles, and
it describes the games and nothing else.

Whether one rating describes the results at all is a different question, and it
is asked separately rather than folded into the margin. A single number cannot
describe an engine that does better against one opponent than another predicts,
so each pairing's score is compared with the one the fit expects of it, and when
they disagree by more than chance allows the run says so and the margin should
be read as an understatement. It is a test rather than a second opinion on
purpose: reporting whichever of the two was larger sounded careful and was not,
because both of them estimate the same thing when the fit is sound, so taking
the larger inflated the margin by about a fifth and cried wolf on around two
runs in five of perfectly ordinary data.

Neither covers the part that matters most. The opponents earned their ratings at
2m+1s on other hardware, and this runs at twenty seconds on a shared runner, so
the placement carries a systematic error worth something like a hundred points.
More games shrink the margin printed next to the number and do nothing at all to
that. It is a placement, not a rating.

The ladder is held as exact, too. A rung whose rating is a community estimate
rather than a ccrl ranking, which is what v10 in the default ladder is, hands
whatever it is wrong by straight to the answer, and no error bar here covers
that either.

The time control is a compromise rather than a default worth keeping by
accident. Ten seconds runs the hundred games in about twenty minutes but leaves
so little headroom that a runner hiccup shows up as a loss on time, and one
forfeit in a twenty-five game pairing is worth about thirty elo of noise. Two
minutes would match the list it is calibrated against and takes most of a day.
Twenty seconds costs about an hour and sits closer to the list than ten does.

Games run long here, a little under two hundred plies on average, so most of the
clock a game uses is increment rather than the base time. That is why doubling
the base from ten to twenty costs closer to three times the wall clock than
twice it, and worth remembering before raising it again.

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

A candidate gets a changelog section like any other release, because the github
release is created from the section matching the tag and cannot be created
without one. The section is a preview rather than a record: it covers everything
since the last full release, which is the same range the release itself will
cover, so the next thing written replaces it rather than being added alongside
it. A release at the end of a run of candidates ends up with the changelog it
would have had if none of them had happened.

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
creates the github release from the changelog, and then builds and uploads
binaries for linux, macos and windows. It goes on to call two workflows that
add to the release once it exists: **Docker** publishes the lichess-bot image
and quotes it in the notes with its digest, and **Strength** plays the match
and adds the elo estimate.

Both edit the notes by reading them and writing them back, so they share a
concurrency group and take turns rather than one landing on top of the other.
