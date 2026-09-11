# Working on Arche

Arche is a small chess engine. The code is short enough to read, so this file
is only for what reading it will not tell you.

## Before you commit

Read [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). It has the build, the two test
runs, the lints, the commit scopes and the trailers an engine commit carries. A
commit-msg hook checks the scope, so a guess at one is rejected rather than
quietly accepted.

[docs/INSTRUMENTS.md](docs/INSTRUMENTS.md) is reference, not prerequisite. It
has the measurements the engine makes of its own search and of its evaluation,
and the time to read it is when you are about to run one.

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
architecture overview", not "Draw the map a new reader looks for".

A title names the thing, in the words someone looking for it would use. Where
a change has a standard name in engine vocabulary (king safety, mobility, late
move reductions, null move pruning), the title carries that name and, where
there is room, how it is done: "Add a basic king safety term from pawn masks",
not "Measure what the king stands behind". Both describe the same commit, and
only the first tells someone scanning `git log` which feature landed. Where a
term arrives in stages, say which stage this is. The same goes for a pull
request title, which is the commit title when there is one commit.

That rule is about the title. A body still explains, and the paragraphs under
it are where the reasoning, the numbers and the rejected alternatives go.

Some things read as generated and are avoided: three-part parallel
constructions, hype words (robust, comprehensive, powerful, seamless), clever
closing lines, and the same idiom twice in one file. When unsure, read the
readme and the 2022 commits in `git log` and match those.
