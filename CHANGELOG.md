# Changelog

All notable changes to this project will be documented in this file.

## [0.3.9-rc.2] - 2026-08-04

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

### Refactor

- *(magic)* Draw magic candidates from the same splitmix as the keys

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
