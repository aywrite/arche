# Changelog

All notable changes to this project will be documented in this file.

## [0.4.1] - 2026-09-04

### Features

- *(search)* Answer a node from a pass that already fails high [bench 14773205] [elo +76 ±30 (sprt [0, 10] passed, 360 games, 5+0.05, vs 2db9961)]
- *(search)* Record the nodes a shortcut answered [bench 14773205]
- *(search)* Try the quiet moves that cut off first [bench 8182450] [elo +109 ±35 (sprt [0, 10] passed, 290 games, 5+0.05, vs 874b4f8)]
- *(search)* Count what the table's key signature costs [bench 8182450]
- *(board)* Give the board a key over the pawns alone [bench 8182450]
- *(search)* Count the narrow signature at three widths [bench 8182450]
- *(uci)* Answer the Clear Hash button [bench 8182450]
- *(uci)* Report the move an aborted iteration swaps in [bench 8182450]

### Performance

- *(board)* Generate into a buffer rather than a growing list [bench 8182450] [speed +3.4%]
- *(board)* Move a piece rather than take one off and put one on [bench 8182450] [speed +3.7%]
- *(board)* Fold the castle keys only when the rights changed [bench 8182450] [speed +2.3%]

### Refactor

- *(board)* Read the colour token and count material in a loop [bench 14773205]
- *(search)* Compare a move with the notation it prints [bench 14773205]
- *(search)* Move the mate window and the taint fold in with the score [bench 14773205]
- *(search)* Search a node's children in one place [bench 8182450]
- *(search)* Ask the two shortcuts in one place [bench 8182450]
- *(uci)* Give the session's threads a module of their own
- *(uci)* Report panics through the session's own writer
- *(uci)* Say the options from one table
- *(uci)* Keep a setter's error for lines it could not act on
- *(uci)* Run a measurement from its settings

### Development

- *(bench)* Add a residuals command that replays the samples
- *(eval)* Drop the king test and the black square assertions
- *(uci)* Drop the duplicate table resize and node limit tests
- *(docs)* Bring the roadmap up to date after null move
- *(docs)* Update the documentation to match the code
- *(bench)* Lay the speed report out as a table
- *(docs)* Record the cost-aware ordering verdict
- *(docs)* Record what an arena for the move lists could win
- *(uci)* Drive the scripted tests through the shipped session loop
- *(docs)* Split the session's threads from the protocol in the code map
- *(docs)* Record the refined evaluation verdict
- *(docs)* Record the correction history verdict
- *(ci)* Check a landed commit's bench against its own pins
- *(bench)* Label a residual by the decision, not the score [bench 8182450]
- *(docs)* Correct the comments later commits left behind
- *(docs)* Bring the readme and the docs tree up to date
- *(ci)* Raise the gauntlet one rung for the coming release
- *(workspace)* Optimise the profile the debug tests run under
- *(uci)* Check the left-out bench depth on the settings alone
- *(uci)* Pin the refusal the measuring tools lean on

## [0.4.0] - 2026-08-28

### Features

- *(search)* Take a node budget and stop on the node it names
- *(uci)* Honour go nodes
- *(uci)* Answer bench on the command line and as a command
- *(search)* Count the cutoffs the search refuses for their taint [bench 42073055]
- *(search)* Let the transposition table be resized after startup [bench 42073055]
- *(uci)* Take the table size from setoption Hash
- *(search)* Name the four graph history policies [bench 36130893] [elo not measured]
- *(search)* Trust the table behind the fifty move guard [bench 35561814] [elo +48 ±23 (sprt [0, 10] passed, 308 games, 5+0.05, vs e5b6026)]
- *(search)* Answer a node near the leaves from its own evaluation [bench 17657158] [elo +62 ±27 (sprt [0, 10] passed, 500 games, 5+0.05, vs bf791e9)]
- *(search)* Stop a deepening that cannot finish the next depth [bench 17657158] [elo +47 ±23 (sprt [0, 10] passed, 552 games, 5+0.05, vs a1e4d17)]
- *(eval)* Taper the piece square score between two phases [bench 20099718] [elo +84 ±32 (sprt [0, 10] passed, 392 games, 10+0.1, vs a1e4d17)]
- *(search)* Give each of the depth cap's three jobs its own bound [bench 20182103] [elo +4 ±10 (sprt [-10, 0] passed, 852 games, 5+0.05, vs 6f11ca4)]
- *(uci)* Answer a stop while the search is still running [bench 20182103]
- *(uci)* Say why the engine died where the interface can read it

### Bug Fixes

- *(search)* Score a mate on the hundredth half move as a mate
- *(uci)* Read a negative clock as an empty one
- *(search)* Finish depth one before the clock can stop the search
- *(search)* Keep the configured deadline apart from the iteration's
- *(search)* Count the transposition stores that land [bench 42073055]
- *(search)* Answer with the aborted iteration's move when it has one [bench 17657158] [elo not measured]
- *(uci)* Answer --version and --help, and reach the loop from a test

### Performance

- *(search)* Sort short move lists on the stack [bench 42847751] [speed +12.0%]
- *(search)* Keep a table entry in sixteen bytes [bench 42611639] [speed +0.1%]
- *(search)* Keep four entries to a cache line and pick among them [bench 42073055] [speed -2.6%] [elo +0 ±7 (sprt [-5, 0] inconclusive, 1622 games, 5+0.05, vs 4c9b8fb)]
- *(board)* Write a piece move's two directions once [bench 42073055] [speed +2.4%]
- *(search)* Return the score the search saw, not the window edge [bench 41396291] [speed -0.9%] [elo not measured]
- *(search)* Remember the fail low nodes too [bench 39394488] [speed -5.6%] [elo not measured]
- *(search)* Let quiescence use the table it was already paying for [bench 36130893] [speed -4.3%] [elo +33 ±19 (sprt [0, 10] passed, 742 games, 5+0.05, vs fc19c6a)]
- *(search)* Hand the sort its keys instead of a closure [bench 36130893] [speed +2.6%]
- *(board)* Compact the evasion list in one pass over it [bench 35561814] [speed +1.6%]
- *(eval)* Index the piece square tables instead of matching [bench 17657158] [speed +1.8%]
- *(board)* Index the piece boards instead of matching [bench 17657158] [speed +2.6%]
- *(board)* Answer what stands on a square with a load, not a walk [bench 20182103] [speed +7.9%]
- *(eval)* Read the material values from a table, not a match [bench 20182103] [speed +0.1%]

### Refactor

- *(search)* Keep the limit check's slow path cold
- *(zobrist)* Spell Zobrist the way Zobrist spelled it
- *(eval)* Name the piece square tables after what they hold
- *(board)* Name the attack masks under construction
- *(magic)* Name the blocker masks under construction
- *(magic)* Build both sliders from one set of tables [bench 42847751]
- *(search)* Give the search a SearchConfig, and name the reference one [bench 42847751]
- *(board)* Parse the move number once and name the starting position [bench 42847751]
- *(search)* Build and count a stored entry in one place [bench 42847751]
- *(board)* Remove two dead conversions and right the debug print [bench 42847751]
- *(uci)* Read a command's parameters off its words
- *(search)* Move the transposition table to its own module [bench 42073055]
- *(search)* Give the table verbs for what the search means [bench 42073055]
- *(search)* Give a search its limits as one value [bench 42073055]
- *(board)* Stop carrying what nothing uses [bench 42073055]
- *(board)* Build the attack masks at compile time [bench 42073055]
- *(board)* Give both colours one castling rule [bench 42073055]
- *(board)* Keep only the surface something uses [bench 42073055]
- *(board)* Ask the board for the evasions [bench 42073055]
- *(board)* Say when the fifty move counter has run out [bench 42073055]
- *(board)* Build the mailbox at compile time [bench 36130893]
- *(board)* Walk the rays for the squares between [bench 36130893]
- *(magic)* Build the slider tables at compile time [bench 36130893]
- *(search)* Keep one ordering key buffer instead of filling one per sort [bench 36130893]
- *(search)* Carry the draw taint with the score [bench 35561814]
- *(search)* Gather the move ordering into one module [bench 17657158]
- *(board)* Recompute the square array off the boards, not the squares [bench 20182103]
- *(uci)* Say the board through the writer, not past it
- *(uci)* Read a command as its first word, in one place
- *(uci)* Assemble the session's threads in one place
- *(eval)* Give the evaluation a module of its own [bench 20182103]
- *(search)* Arm everything that stops an iteration in one place [bench 20182103]

### Documentation

- *(search)* Say what the private entry leaves room for
- *(uci)* Say where the info line's elapsed time comes from
- *(search)* Say who owns the table's generation across searches
- *(uci)* Record what a refused move does not tell the interface
- *(search)* Record the repetition rule that measurement rejected

### Development

- *(docs)* Say what the board relies on and correct what drifted
- *(docs)* License the engine under GPL-3.0-or-later
- *(ci)* Check the shell scripts out with unix line endings
- *(ci)* Skip the shell script tests on windows
- *(bench)* Read criterion's output as utf-8 whatever the console says
- *(bench)* Add the bench, a fixed suite searched to a fixed depth
- *(docs)* Say which duplication is load bearing
- *(ci)* Require a bench on engine commits and a speed on perf commits
- *(bench)* Print the bench and speed trailers from scripts
- *(ci)* Count every commit's stated bench, and run the hooks in ci
- *(release)* Print the elo trailer with a match, and the trailers in the changelog
- *(docs)* Say which trailers a commit carries and how to produce them
- *(docs)* Split the readme up and say what AI wrote
- *(docs)* Say how to work here, and what has already been measured
- *(bench)* Pin the reference search's counts apart from the default's
- *(bench)* Read the starting position from the engine
- *(bench)* Measure speed the way a perf commit does, and retire criterion
- *(search)* Drop the tests a stronger neighbour already proves
- *(uci)* Test the depth clamp rather than the capture under its name
- *(bench)* Say each thing once in the bench and trailer check tests
- *(bench)* Leave fmt and cargo check to the Rust workflow
- *(docs)* Say what the test suite's time goes on now
- *(bench)* Tell the sides of a speed measurement apart by side
- *(release)* Name the sprt verdict in the Elo trailer
- *(release)* Put docs scoped commits under Development, as the notes say
- *(ci)* Hold the four scope lists to each other
- *(ci)* Let a run on master finish when another lands behind it
- *(docker)* Publish the image on release only
- *(bench)* Build a commit from an export, in one script
- *(ci)* Build the sides of a match and of the speed job from exports
- *(bench)* Stamp an export with now and give it a target directory of its own
- *(uci)* Fuzz the parameter parser with properties
- *(bench)* Take a table size and a taint policy, and say the move
- *(uci)* Assert the clock a go command sets reaches the search
- *(search)* Drop the weaker of two tests with one setup
- *(search)* Let the compiler check the table's layout
- *(uci)* Drop the parameter tests the properties already cover
- *(ci)* Read a match result once
- *(board)* Count the perft positions the way a game plays them
- *(ci)* Report time to depth when the node counts differ
- *(ci)* Pin the actions to commit shas
- *(ci)* Fail on an advisory against a dependency
- *(release)* Attest what a release publishes
- *(docs)* Bring the roadmap's order up to date with what measurement said
- *(docs)* Lead the readme with what the engine is
- *(board)* Generate plausible fens and check what parses [bench 20099718]
- *(tactics)* Gate on a tactical suite, and report coverage [bench 20099718]
- *(release)* Build the x86-64 archives at three cpu levels
- *(workspace)* Call the engine crate arche-core [bench 20182103]
- *(deps)* Bump actions/attest-build-provenance in the actions group
- *(workspace)* State the licence in every source file [bench 20182103]
- *(uci)* Drive the shipped binary through real sessions
- *(ci)* Raise the gauntlet to where the engine now plays
- *(bench)* Compare the fastest rounds, and say when a change is no claim
- *(bench)* Give the speed job nine rounds
- *(docs)* Add an architecture overview
- *(docs)* Say how to write here
- *(docs)* Update the documentation to match the code
- *(workspace)* Say in the manifests what each crate is and where it lives
- *(ci)* Name the script tests job for what it runs
- *(search)* Prove the quiescence reach at depth five, not eight

## [0.3.10] - 2026-08-22

### Bug Fixes

- *(board)* Record the move history in a ring
- *(board)* Reject a position the search cannot survive
- *(board)* Reject a fen with more than eight files on a rank
- *(search)* Refuse a transposition score that came from a draw
- *(search)* Quiesce the leaves of shallow searches too
- *(search)* Show quiescence the promotions that capture nothing
- *(search)* Evade checks in quiescence instead of standing pat
- *(board)* Tighten the bitboard index asserts to reject 64
- *(search)* Store the root entry past the depth contest
- *(uci)* Report nodes for the whole search rather than one iteration

### Performance

- *(search)* Try the table's move before generating any
- *(search)* Halve the default transposition table to 256MB
- *(search)* Answer checks from make_move instead of probing for them
- *(search)* Find the evasion targets once per node, spare perft the checkers

### Refactor

- *(uci)* Move protocol printing out of the library
- *(engine)* Shape the Engine trait around its caller and delete the fossils
- *(board)* Tidy the board, misc and play modules
- *(search)* Tidy the search module
- *(magic)* Generate moves and captures from one function
- *(engine)* Delete the cleared key workaround the refusal retires
- *(uci)* Route output through a writer so the protocol is testable

### Documentation

- *(docs)* Say what the debug test run checks that release cannot

### Development

- *(board)* Assert the en passant square is one a pawn can take
- *(deps)* Replace lazy_static with the standard library
- *(search)* Measure how much the table depends on the path taken
- *(search)* Pin that draw taint is recorded and never trusted
- *(release)* Survive a repository with no release tag yet
- *(ci)* Cover the helper scripts and gate them in ci
- *(ci)* Stop a strength match as soon as the answer is in
- *(board)* Walk random lines checking state and unmake
- *(board)* Share the fens and the move lookup, table the macros
- *(search)* Adopt the shared fens and drop the test_ prefixes
- *(uci)* Assert the replies the interface actually sees

## [0.3.9] - 2026-08-04

### Features

- *(search)* Take a draw on the first repetition rather than the third

### Bug Fixes

- *(uci)* Honour movetime and budget the increment safely
- *(uci)* Clear the transposition table on ucinewgame
- *(uci)* Report bad input instead of dying on it
- *(eval)* Turn the piece square tables the right way up
- *(search)* Rebuild the principal variation by replaying it
- *(zorbrist)* Only hash en passant when a pawn can take there

### Performance

- Let the piece square and zorbrist lookups inline
- *(board)* Iterate bitboards in place instead of collecting them
- *(search)* Index the hash table without dividing
- *(eval)* Keep the piece square sum incrementally
- *(magic)* Use precomputed magic numbers instead of searching for them
- *(search)* Narrow the depth and ply kept in the hash table
- *(search)* Score in sixteen bits so more of the table fits
- *(eval)* Build the piece square tables at compile time
- *(zorbrist)* Build the keys at compile time
- *(eval)* Store the piece square tables in sixteen bits
- *(magic)* Only look for a capture where there can be one

### Refactor

- *(magic)* Draw magic candidates from the same splitmix as the keys
- *(search)* Return the outcome from the root instead of probing the table

### Documentation

- *(docs)* Bring the readme todo list up to date
- *(magic)* Correct what the magic search costs and why

### Development

- *(bench)* Stop timing the transposition table clear
- *(release)* Separate engine changes from development in the changelog
- *(release)* Run the strength match on demand with a game count
- *(docker)* Publish the image from the release and quote it in the notes
- *(lint)* Check the shell scripts with shellcheck
- *(release)* Stop a rerun adding a second copy of each notes line
- *(deps)* Update clap and stop dependabot proposing bad action bumps
- *(board)* Add the perft positions that catch the awkward cases
- *(search)* Pin how many nodes the search visits
- *(search)* Stop every test allocating half a gigabyte
- *(bench)* Measure both sides of a pull request on one runner
- *(bench)* Post the comparison to the pull request again
- *(release)* Let the strength match name both sides
- *(board)* Assert the eval counters against a recompute in debug
- *(bench)* Place the engine on the ccrl scale at each release
- *(bench)* Share what the two match workflows have in common
- *(bench)* Let the rating estimate outlive a failed strength match
- *(bench)* Count one unfinished game as one rather than as 1 games
- *(board)* Check the position key against a recomputed one
- *(release)* Give a candidate a changelog section it can be released from

## [0.3.8] - 2026-08-01

### Bug Fixes

- Report the engine author with an id line
- Search the root position even when it has repeated
- Stop long games from running past the end of the history
- Point the sanity check at a fastchess tag that exists
- Stop the cargo-release install being cached away
- Write the changelog from the last full release

### Documentation

- Document building, strength and the development workflow

### Features

- Bundle the engine and an opening book into a lichess-bot image

### Miscellaneous Tasks

- Update workflow actions, cache builds and check formatting
- Modernise the pre-commit hooks
- Build and publish the lichess-bot docker image
- Remove the iai benchmark
- Replace the disabled benchmark workflow
- Group the dependabot updates

### Performance

- Stop the search benchmark timing a 500MB memset

### Styling

- Fix the clippy warnings across the workspace

### Ci

- Add a short match against the previous release
- Add clippy, debug and msrv jobs to the test workflow
- Add a workflow to cut a release from the actions tab
- Open a pull request to release rather than pushing to master

## [0.3.7] - 2026-07-26

### Bug Fixes

- Fix position key generation and transposition table bugs
- Use depth and key when replacing hash table entries

### Miscellaneous Tasks

- Bump bumpalo from 3.11.0 to 3.12.0
- Bump bumpalo from 3.11.0 to 3.12.0 in /basic_engine
- Update to Rust 2024 edition and latest dependency versions
- Fix release tooling config for current cargo-release and git-cliff

### Testing

- Add tests for hash table replacement and cache reuse

## [0.3.6] - 2022-09-29

### Bug Fixes

- Reinitialize selective depth on call to search
- Fix inverted calculation of least valuable attacker score
- Ensure quiescence nodes are never used for pv
- Don't overwrite exact hash table entries with non-exact evals
- Resolve hash collisions by comparing to original key

### Miscellaneous Tasks

- Various minor lint fixes
- Stop including release candidate tags in changelog

### Performance

- Calculate negative score when sorting instead of sort then reverse
- Modify move ordering score when destination is attacked
- Use depth in hash table replacement strategy

### Refactor

- Clean up syntax used for bitboard mutations
- Implement Not operator for Color enum to simplify some match blocks

### Testing

- Add test for hash key random uniqueness

## [0.3.5] - 2022-09-25

### Bug Fixes

- Display engine author and name on separate id lines
- Add missing increment for fifty move rule
- Improve calculation of move time

### Features

- Include selective search depth in uci info output
- Increase selective search depth for positions where in check

### Miscellaneous Tasks

- Don't include release commits in changelog

### Performance

- Optimize check for repeated positions

### Refactor

- Move bitboard trait to standalone module

### Testing

- Refactor benchmarks to use shared test positions
- Add basic iai benchmark for alpha beta

## [0.3.4] - 2022-09-20

### Bug Fixes

- Try clearing cache key for moves made
- Fix off by one error for white checkmate in calculations

### Documentation

- Add brief description of project purpose to README

### Miscellaneous Tasks

- Add checksum to release created in CI
- Update pretty_assertions to fix security warning
- Disable criterion compare CI step until it is fixed
- Release 0.3.4

### Performance

- Use bitmask to avoid checking empty squares during evaluation
- Increase maximum depth for quiescence search to prevent horizon effects

### Refactor

- Use array instead of vector for magic bits

## [0.3.3] - 2022-09-17

### Bug Fixes

- Try re-ordering draw check to prevent draws in winning positions
- Slightly increase score for 5th rank pawns
- Add template for cargo-release commit messages

### Documentation

- Add basic usage to readme

### Miscellaneous Tasks

- Add CI job to compare benchmarks on pull requests
- Release 0.3.3

### Performance

- Use small vec instead to reduce allocations in move generation

### Refactor

- Clean up some tests by using a macro

### Styling

- Minor lint fixes based on clippy output
- Add pre-commit config and associated initial fixes

### Testing

- Fix transposition table shortcutting alpha-beta benchmarks

## [0.3.2] - 2022-09-17

### Bug Fixes

- Bug in evaluation causing non-symmetric scores
- Incorrect calculation of moves until checkmate
- Incorrect calculation of hash table size

## [0.3.0] - 2022-09-16

### Miscellaneous Tasks

- Create CI configuration for test & release automation

### Testing

- Fix up proptest regressions file

## [0.2.4] - 2022-09-16

### Features

- Implement basic transposition table

### Refactor

- Minor cleanup and optimization

## [0.2.0] - 2022-09-16

### Features

- Add basic version piece value tables
- Change move generation to use magic bitboards

## [0.1.2] - 2022-09-16

### Performance

- Change hash table implementation

## [0.1.1] - 2022-09-16

### Features

- Implement quiescence search extension to alpha beta

### Miscellaneous Tasks

- Add configuration for cargo-release

<!-- generated by git-cliff -->
