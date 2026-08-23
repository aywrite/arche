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
