# Working on Arche

Arche is a small chess engine. The code is short enough to read, so this file
is only for what reading it will not tell you.

## Before you commit

Read [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). It has the build, the two test
runs, the lints, the commit scopes and the trailers an engine commit carries. A
commit-msg hook checks the scope, so a guess at one is rejected rather than
quietly accepted.

[docs/ROADMAP.md](docs/ROADMAP.md) has what is not implemented, what is known to
be wrong, and which experiments have already been measured and rejected.

## House rules

- Design and planning documents stay out of the repository unless one has been
  asked for. Keep them in a scratch directory.
- Review fixes are folded into the commits they fix rather than added on top.
  Rebuild the branch and push with `--force-with-lease`.
- Pull request descriptions are short, plain and self-contained.
- For a change that is not trivial, have a second agent review it if one is
  available, and check its findings against the code before acting on them.

## Writing

Anything a reader sees (commit messages, PR bodies, docs, comments) is written
plainly. Short declarative sentences. Parentheses rather than em dashes. Say
what changed and why without staging it: a commit title is "Add an
architecture overview", not "Draw the map a new reader looks for". Name the
thing rather than its kind: "Give the bench settings a run method", not "Run
a measurement from its settings". A title that would fit two different
changes has not said what this one did.

Some things read as generated and are avoided: three-part parallel
constructions, hype words (robust, comprehensive, powerful, seamless), clever
closing lines, the same idiom twice in one file, and one title shape repeated
across a run of commits ("in one place", "from one table"). When unsure, read
the readme and the 2022 commits in `git log` and match those.
