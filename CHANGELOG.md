# Changelog

All notable changes to this project will be documented in this file.

## [0.4.9] - 2026-10-09

### Features

- *(search)* Weigh entry age against depth in transposition table replacement [elo +4 ±9 (sprt [-10, 0] passed, 2500 games, 30+0.3, vs 31eb33b)] [bench 5965973]
- *(search)* Raise the transposition table's age weight in endgames [elo +10 ±8 (sprt [0, 10] passed, 3000 games, 30+0.3, vs ec2abac)] [bench 5965973]
- *(eval)* Add a tempo term for the side to move, tapered from 10 to 8 [bench 5312244] [elo +10 ±7 (sprt [0, 10] passed, 5500 games, 10+0.1, vs ec2abac)]
- *(search)* Open the aspiration window at 15 rather than 30 [bench 5355551] [elo +13 ±9 (sprt [0, 10] passed, 3400 games, 10+0.1, vs ec2abac)]
- *(uci)* Scale the soft time line by the best move's share of the root's nodes [bench 5355551] [elo +11 ±9 (sprt [0, 10] passed, 3000 games, 10+0.1, vs 36ecabc)]
- *(search)* Add singular extensions from the table move's floor [bench 9939754] [elo +44 ±22 (sprt [0, 10] passed, 500 games, 10+0.1, vs 621ecab)]

### Performance

- *(eval)* Keep the pair term as the sum and difference of the two perspectives [bench 5965973] [speed +0.0% (bench nps, 95% interval -1.4% to +1.6%, 60 interleaved rounds over shuffled layouts vs adfd966)]
- *(eval)* Hold the pawn structure beside the shelter in the shelter table's entry [bench 5965973] [speed +1.6% (bench nps, 95% interval +0.1% to +3.1%, 60 interleaved rounds over shuffled layouts vs d133ba1)]
- *(eval)* Read material from the piece's row as one white relative sum [bench 5965973] [speed +1.0% (bench nps, 95% interval -0.8% to +2.6%, 60 interleaved rounds over shuffled layouts vs 6b214ea)]
- *(search)* Store the static evaluation in the transposition table entry [bench 5965973] [speed +0.7% (bench nps, 95% interval -1.2% to +2.5%, 60 interleaved rounds over shuffled layouts vs a620e4b)]
- *(board)* Switch make and unmake to copy-make on a stack of positions [bench 5355551] [speed +2.6% (bench nps, 95% interval -0.0% to +5.4%, 60 interleaved rounds over shuffled layouts vs 621ecab)]
- *(board)* Guard the king step test with the kept king square [bench 5355551] [speed +0.4% (bench nps, 95% interval -1.7% to +2.6%, 60 interleaved rounds over shuffled layouts vs 324a485)]

### Refactor

- *(search)* Hold a node's facts and its answer in one value the reductions read [bench 5965973]
- *(search)* Ask for a child search with the loop's decision [bench 5965973]
- *(search)* Answer quiescence and the root through one fail soft value [bench 5965973]
- *(board)* Name the square behind a pawn and the pawn attack masks once [bench 5965973]
- *(eval)* Fold each term through weigh directly and pin the arithmetic once [bench 5965973]
- *(uci)* Remove duplicated branches and dead match arms
- *(search)* Remove duplicated code from the table, the gate and the ledger [bench 5965973]
- *(board)* Read a file's letter and index off its discriminant [bench 5965973]
- *(board)* Keep DerefMut on the board to tests and fix stale make comments [bench 9939754]
- *(eval)* Read the capped phase through the accumulator's accessor [bench 9939754]
- *(search)* Shorten the aspiration width comment and fix the forced arm's doc [bench 9939754]
- *(uci)* Rewrap the soft line's node share doc and state its lower bound plainly

### Documentation

- *(uci)* Record that moving the soft time bound past 45% measured nothing

### Development

- *(ci)* Give the mache pins a Dependabot pull request of their own
- *(bench)* Add a forced decision instrument that inverts one shortcut at a time [bench 5965973] [elo not measured]
- *(board)* Walk the perft cases through one helper for the three generators
- *(uci)* Table the reader tests and share the session fixtures
- *(search)* Merge the repeated table seeding, replay and fifty move tests [bench 5965973]
- *(bench)* Share the instruments' position parsing, header and share helpers
- *(bench)* Share the recorders' arming fixture and the terms tests' hand count helpers
- *(ci)* Offer the table size on the Strength workflow's dispatch tab
- *(docs)* Record the v0.4.7 to v0.4.9 rejections in the roadmap's closed list
- *(deps)* Bump taiki-e/install-action
- *(docs)* Correct the release's stale roadmap entries and record five closed arms
- *(bench)* Build the forced instrument's full kind set from its kinds
- *(bench)* Check that each forced row's search differs from the default
- *(uci)* Test the root node counts the soft time line reads
- *(board)* Check that a detached board keeps the keys from before its root
- *(search)* Write the root window tests in terms of the aspiration width
- *(eval)* Price the tempo block in the tuner's bounds test

## [0.4.8] - 2026-10-04

### Features

- *(uci)* Add a debug log of every line read and said
- *(search)* Widen the late move pruning band to -5457 [bench 5769245]
- *(eval)* Report how the tuner's L-BFGS stopped [bench 5769245]
- *(eval)* Refit the pair term's factors at a weaker ridge of 3e-7 [bench 5966188] [elo +10 ±7 (6000 games, 10+0.1, vs d073151)]

### Bug Fixes

- *(uci)* Answer an isready read behind a queued go infinite
- *(search)* Score a stalemate at the quiescence horizon as a draw [bench 6369864] [elo +3 ±8 (sprt [-10, 0] passed, 3000 games, 10+0.1, vs e01fb9a)]
- *(board)* Stop the swap walk at its last slot rather than write past it [bench 5769245]
- *(board)* Read a move name's promotion piece whatever its case [bench 5769245]
- *(uci)* Drop a line that is not utf-8 rather than leave the session
- *(uci)* Search a depth of zero or less as depth one
- *(uci)* Refuse a debug log path that is not a regular file
- *(board)* Try a promotion or an en passant capture from the table before generating [elo -4 ±10 (sprt [-10, 0] inconclusive, 2000 games, 10+0.1, vs ffdb1f0)] [bench 5965973]

### Performance

- *(eval)* Weigh mobility and the king attack zone at each piece in the shared walk [bench 6369864] [speed +0.4% (bench nps, 95% interval -0.8% to +1.6%, 60 interleaved rounds over shuffled layouts vs d1beb6b)]
- *(board)* Read the static exchange evaluation from a table of attacker counts [bench 6369864] [speed +2.1% (bench nps, 95% interval +0.8% to +3.2%, 60 interleaved rounds over shuffled layouts vs d1beb6b)]
- *(board)* Read a piece's key, piece square value and factors from one row per square [bench 6369864] [speed +2.3% (bench nps, 95% interval +0.5% to +4.0%, 60 interleaved rounds over shuffled layouts vs 9d71072)]
- *(board)* Test a king step's legality before making the move [bench 6369864] [speed +0.9% (bench nps, 95% interval -0.7% to +2.6%, 60 interleaved rounds over shuffled layouts vs 4a05e83)]
- *(board)* Restore the pawn key and accumulator by copy in the unmake [bench 6369864] [speed +0.2% (bench nps, 95% interval -2.1% to +2.5%, 60 interleaved rounds over shuffled layouts vs 9f0d56f)]
- *(search)* Generate captures ahead of quiet moves so the ordering keys only the captures [bench 5769245] [speed +5.4% (bench nps, 95% interval +3.5% to +7.2%, 60 interleaved rounds over shuffled layouts vs cc865ba)]
- *(search)* Key the quiet moves four at a time with SSE and pick the least by a vector minimum [bench 5769245] [speed +0.3% (bench nps, 95% interval -1.3% to +2.1%, 60 interleaved rounds over shuffled layouts vs fe513cb)]
- *(search)* Probe a transposition table bucket with one SSE2 key compare and a generation mask [bench 5966188] [speed +0.6% (bench nps, 95% interval -0.9% to +2.0%, 60 interleaved rounds over shuffled layouts vs f22a1fe)]
- *(search)* Hoist the node checks of late move reductions and of futility and move count pruning out of the move loop [bench 5966188] [speed -0.8% (bench nps, 95% interval -2.2% to +0.5%, 60 interleaved rounds over shuffled layouts vs 44c7159)]
- *(search)* Step over the pruned quiet moves in one jump rather than one place at a time [bench 5966188] [speed +0.7% (bench nps, 95% interval -0.8% to +2.0%, 60 interleaved rounds over shuffled layouts vs 92829a8)]

### Refactor

- *(search)* Count the transposition table's cutoffs and stores on the searcher [bench 6407589]
- *(board)* Read the swap table's piece values off the swap's own [bench 5966188]
- *(uci)* Dispatch the binary's commands from one array of instruments
- *(search)* Move the search's tests into a file of their own [bench 5966188]
- *(search)* Key the killers by comparing indexes rather than lending their ranks to the history [bench 5966188]
- *(search)* Carry the node's evaluation memo as a Score [bench 5966188]
- *(search)* Name the four states of the root bounds [bench 5966188]
- *(search)* Split alpha_beta into named steps and put each instrument behind one call [bench 5966188]
- *(board)* Shrink the history ring from 1,024 plies to 256 [bench 5965973]
- *(search)* Remove the deep reduction's model threshold and its switch [bench 5965973]

### Documentation

- *(eval)* Restate mobility's cost against the 5% budget at 8f9dafe
- *(uci)* Shorten the comments on the debug log, the isready fix and the games suite
- *(board)* Shorten the comments on the swap table, the kept stack and the king step [bench 5769245]
- *(search)* Shorten the comments on the SSE quiet keying and the stalemate test [bench 5769245]
- *(board)* Say why the move generator's writes are sound whatever the list held [bench 5966188]

### Development

- *(search)* Pin how quiescence scores a stalemate at the horizon
- *(bench)* Add a games suite searched to a node budget beside the bench
- *(docs)* Correct the documentation the changes since 0.4.7 left behind
- *(uci)* Hold a stop read while idle to being counted like any other
- *(ci)* Test the quiet ordering's scalar forms on the baseline x86-64 target
- *(search)* Check in debug builds that the killers' lent ranks are put back [bench 5966188]
- *(board)* Check the static exchange's fast exits against the walk in debug builds [bench 5966188]
- *(deps)* Bump the mache actions from v0.5.0 to v0.7.1
- *(ci)* Name the 40/15 scale through mache rather than rewriting its line
- *(workspace)* Remove the nix cross-compilation shell
- *(ci)* Count the bench's instructions along master in one run
- *(bench)* Add a verdict to the instruction count against a 0.7% band
- *(deps)* Bump smallvec from 1.16.1 to 1.16.2 in the cargo group
- *(deps)* Bump taiki-e/install-action

## [0.4.7] - 2026-09-27

### Features

- *(eval)* Add a factorization machine term, off at rank 0 [bench 6810240]
- *(eval)* Refit every weight but material jointly on 59,049 games [bench 6375981]
- *(eval)* Turn on the factorization machine at rank 16 with its fitted table [bench 6173942] [elo +95 ±19 (1000 games, 10+0.1, vs 8acbbc0)]
- *(search)* Refit the late move pruning model on game positions [bench 6407589] [elo +0 ±7 (sprt [-10, 0] passed, 4500 games, 10+0.1, vs e77fd54)]

### Bug Fixes

- *(uci)* Match option names ignoring case
- *(search)* Print the searched count the gate read on the ledger's skipped rows [bench 6900228]
- *(board)* Say why a move in a position line cannot be played [bench 6900228]
- *(uci)* Refuse a setting keyword typed without its value
- *(search)* Refuse reverse futility and the null move at every open window [elo +2 ±9 (sprt [-10, 0] passed, 3000 games, 10+0.1, vs ec3d213)] [bench 6810240]
- *(uci)* Count stops so a stop read early reaches its own search

### Performance

- *(search)* Skip SEE for the quiescence captures the delta test drops [bench 6900228] [speed +1.8% (bench nps, 95% interval +0.4% to +3.5%, 15 interleaved rounds vs bfc5143)]
- *(search)* Guard mate distance pruning on a mated alpha or a mating beta [bench 6900228] [speed -1.4% (bench nps, 95% interval -3.3% to +0.8%, 15 interleaved rounds vs c94b6a1)]

### Refactor

- *(search)* Skip the table's move by its place rather than comparing every move [bench 6900228]
- *(search)* Derive the quiet run from the counts the ordering made [bench 6900228] [elo +13 ±15 (sprt [-10, 0] passed, 1000 games, 10+0.1, vs d379bc8f)]
- *(board)* Answer gives_check from a per node table of checking squares [bench 6900228]
- *(search)* Order the quiets lazily, sorting only what the loop reads [bench 6900228]
- *(magic)* Take the probe's bounds check off with a power of two mask [bench 6900228]
- *(magic)* Build the magic tables without the long const evaluation warning [bench 6900228]
- *(search)* Give mate distance pruning a function of its own [bench 6900228]

### Documentation

- *(search)* Record what the mate distance pruning match measured
- *(search)* Record the game batch for the skip at depth three
- *(search)* Correct three stale comments on the table, the quiet ordering and the late move count [bench 6900228]
- *(search)* Shorten the comments on pruning, ordering, the table and the limits [bench 6810240]
- *(eval)* Shorten the comments on the board, move generation and evaluation [bench 6810240]
- *(uci)* Shorten the comments on the protocol, the session and time control
- *(search)* Shorten the comments in the search [bench 6810240]
- *(eval)* Shorten the pair term's and the refit's comments to the sweep's standard [bench 6407589]

### Development

- *(ci)* Point the match sections at mache where they repeat its readme
- *(deps)* Bump taiki-e/install-action in the actions group
- *(release)* Move the gauntlet up to bracket 2800 and add Weiss 1.0 and Stash 25.0
- *(bench)* Move the effort switch table onto the search configuration
- *(release)* Correct the 40/15 ladder's comparison with the blitz panel
- *(search)* Hold the reverse futility margin above its mate boundary with a test that fails [bench 6900228]
- *(bench)* Let the effort instrument turn two switches off at once
- *(bench)* Report speed as a paired Hodges-Lehmann estimate with a 95% interval
- *(bench)* Run a speed round again when the machine was loaded for it
- *(bench)* Count the bench's instructions under cachegrind beside the speed
- *(bench)* Discard a warmup bench per side before the speed rounds
- *(bench)* Add --cpu to pin the speed benches to chosen cpus
- *(bench)* Name the runner in the speed comment and turn the round rerun off
- *(bench)* Show the mean of each side's faster half in the speed report
- *(bench)* Run each speed round on a code layout of its own
- *(bench)* Measure the speed job over forty shuffled layouts
- *(ci)* Shorten the comments in the workflows and match scripts
- *(bench)* Shorten the comments on the instruments
- *(docs)* Shorten the documentation and correct what had drifted
- *(search)* Hold the beta mate exemptions with an ordinary alpha [bench 6810240]
- *(search)* Give the ledger's skipped fixtures the searched count it prints [bench 6810240]
- *(search)* Drop the switch guard and tighten three search tests [bench 6810240]
- *(uci)* Refuse an unreadable effort setting and drop a repeated test
- *(ci)* Close four gaps in the workflow and script tests
- *(ci)* Report the in-game nodes a second in the strength summary
- *(eval)* Run the factorization machine at rank 8 under a test feature
- *(ci)* Test the evaluation's pair term at its test rank
- *(eval)* Correct what the pair term's tests and comments say about it [bench 6407589]
- *(docs)* Describe the pair term where the documentation still missed it
- *(release)* Move the blitz and 40/15 gauntlets up to bracket 2870 and 2800

## [0.4.6] - 2026-09-23

Corrections to this section, written by hand after it was generated. The
generated lines below are left as they were published.

- Two entries here print `[bench 48354803]` and their trees pin 47836191:
  "Count the iteration a budget gave up in the nodes a search reports"
  (`3298289`) and "Add an effort instrument that differences two
  configurations by the node" (`b9325ae`). Both were written on quiet
  futility pruning, which counts 48354803, and rebased onto the late move
  count before they landed. Neither moves a pin, so their bench is the
  late move count's 47836191.

### Features

- *(search)* Add an adaptive null move reduction that grows with depth and the eval's margin over beta [bench 50127050] [elo +27 ±16 (sprt [0, 10] passed, 1000 games, 10+0.1, vs 54d85b96)]
- *(search)* Decide the deep reduction by depth and move index [bench 50263773] [elo +2 ±9 (sprt [-10, 0] passed, 3500 games, 10+0.1, vs ac53360)]
- *(search)* Add quiet futility pruning at depths one to three [bench 48354803] [elo +21 ±13 (sprt [0, 10] passed, 1500 games, 10+0.1, vs 92922deb)]
- *(search)* Add a late move count at depths one to three [bench 47836191] [elo +37 ±16 (sprt [0, 10] passed, 1000 games, 10+0.1, vs bdf0befa)]
- *(search)* Add mate distance pruning [bench 2153533]

### Bug Fixes

- *(search)* Count the iteration a budget gave up in the nodes a search reports [bench 48354803]
- *(search)* Fall back to a smaller transposition table at startup [bench 47836191]
- *(search)* Measure an aborted search's time over the nodes it reports [bench 47836191]
- *(uci)* Write a final info line with every node the search spent

### Documentation

- *(search)* Record the attention model's objective as measured and rejected
- *(search)* Correct what the late move count's line is pinned to [bench 47836191]

### Development

- *(deps)* Bump the actions group across 1 directory with 4 updates
- *(bench)* Hold a term by name rather than by a flag of its own
- *(deps)* Bump the mache actions from v0.3.0 to v0.4.0
- *(bench)* Add an effort instrument that differences two configurations by the node [bench 48354803]
- *(bench)* Read the effort test's outcomes above the depths the rule decides
- *(deps)* Bump the mache actions from v0.4.0 to v0.5.0
- *(docs)* Correct the quiet futility figure and four counts the new rules staled
- *(release)* Add a second calibration gauntlet against the ccrl 40/15 list
- *(bench)* Let the effort instrument ablate the late move count [bench 47836191]
- *(ci)* Chain up to four sprt batches in one strength run
- *(ci)* Read the ladder's numbers off resolve rather than off the inputs
- *(bench)* Raise the bench depth from nine to eleven [bench 6900228]

## [0.4.5] - 2026-09-19

### Features

- *(eval)* Count the enemy pieces bearing on the king's ring, at zero weight [bench 3868987] [elo not measured]
- *(eval)* Fit the king attack zone weights on the archive [bench 4102609] [elo +35 ±16 (sprt [0, 10] passed, 1000 games, 10+0.1, vs 4b1b80f)]
- *(search)* Add aspiration windows at the root [bench 52298065] [elo +19 ±11 (sprt [0, 10] passed, 2000 games, 10+0.1, vs dcc9688)]
- *(search)* Add a reduction table by depth and move count [bench 51236454] [elo +19 ±11 (sprt [0, 10] passed, 2000 games, 10+0.1, vs 5902681)]

### Performance

- *(eval)* Take the king attack counts in the mobility walk [bench 4102609] [speed +5.7% (bench nps, 21 interleaved rounds vs 02432c9, spread 12.2%)]
- *(search)* Skip the stack sort for a list with no key and rotate one with a single key [bench 4102609] [speed +0.4% (bench nps, 7 interleaved rounds vs 6f32cbc, spread 4.1%)]
- *(board)* Probe one slider line for legality where the move vacated a line through the king [bench 4102609] [speed +0.3% (bench nps, 7 interleaved rounds vs 0df6a31, spread 5.9%)]
- *(search)* Build the late move node only where the node admits a reduction [bench 4102609] [speed +1.2% (bench nps, 7 interleaved rounds vs b10e2c0, spread 6.3%)]
- *(search)* Put the transposition table on huge pages [bench 52298065] [speed -1.1% (bench nps, 5 interleaved rounds vs 273c5eb, spread 3.6%)]

### Refactor

- *(uci)* Read an instrument's suite in one place
- *(search)* Move the late move reductions and pruning into one module [bench 4102609] [elo not measured]
- *(search)* Make the principal variation exemption explicit [bench 4102609]
- *(search)* Rename Edges to RootBounds and pin how the bits travel [bench 4102609]
- *(board)* Derive the move number from the ply rather than keeping it [bench 52404553]
- *(eval)* Fold every leaf term through one weigh [bench 52404553]
- *(board)* Read recompute_checkers off the attacker helpers [bench 52404553]
- *(board)* Hold the castle rights as four bits [bench 52404553]
- *(search)* Share the sampling key, the share and the reference replay across the recorders [bench 52404553]
- *(uci)* Read the depth and the taint word in one place each
- *(uci)* Read every spin option through one reader from its row
- *(eval)* Give the shelter and the pawn structure one direct mapped cache type [bench 52298065] [elo not measured]

### Documentation

- *(search)* Correct the claim that fail low nodes are not stored
- *(search)* Shorten the comments in the search, the generator and the suites [bench 4102609]
- *(eval)* Shorten the comments in the evaluation and the tuner [bench 4102609]
- *(board)* Shorten the comments in the board and make/unmake [bench 4102609]
- *(uci)* Shorten the comments in the protocol crate and its tests
- *(search)* Record a third sort shape and the stored static evaluation as measured and priced
- *(uci)* Give the terms row's formula every leaf term and the vector its width
- *(search)* Give the taint policy result its games and its control
- *(eval)* Correct what the whole key costs a cache entry
- *(uci)* Say which spin values are clamped and which are refused

### Development

- *(search)* Store and probe a mate score at a nonzero ply
- *(tactics)* Add a floor under the suite and a list of accepted losses
- *(uci)* Check an instrument takes the sampling rate it was given
- *(docs)* Answer the mobility budget bullet and record two rejected arms
- *(ci)* Pin the three tools that prepare a release
- *(uci)* Pin what a terms argument takes and how it refuses
- *(uci)* Name a suite the bench does not use when checking one is read
- *(bench)* Split the attention fit by group and let a feature be dropped [bench 3868987]
- *(ci)* Refuse to resolve a ref under a trigger that carries secrets
- *(ci)* Shorten the comments in the workflows, the scripts and the manifests
- *(board)* Add the three evasion shapes to the perft edge cases
- *(ci)* Call the match orchestration out of mache rather than inlining it
- *(deps)* Bump smallvec from 1.16.0 to 1.16.1 in the cargo group
- *(bench)* Raise the bench depth from 7 to 9 [bench 52404553]
- *(release)* Move the gauntlet up to bracket 2700 and add Blunder, Inanis and Zahak
- *(deps)* Bump the mache actions from v0.2.0 to v0.3.0
- *(docs)* Correct the speed report example and eight stale claims
- *(ci)* Date the coverage estimate and pin the v2 figure to its commit
- *(docker)* Name every option the engine advertises and raise Move Overhead

## [0.4.4] - 2026-09-14

### Features

- *(eval)* Add a pawn structure term behind a pawn hash [bench 3882989]
- *(eval)* Name the tuner's sealed games rather than drawing them from the key [bench 3882989]
- *(eval)* Fit the sixteen pawn structure weights to our own games [bench 4066438] [elo +57 ±25 (sprt [0, 10] passed, 500 games, 10+0.1, vs 062fd3c259ddf12a7aead948c00a9be3a634e857)]
- *(eval)* Add an insufficient material rule returning zero [bench 4089373] [elo +8 ±12 (sprt [-10, 0] passed, 1500 games, 10+0.1, vs acf076d)]
- *(uci)* Budget a twentieth of the clock when no move count is given [elo +58 ±17 (sprt [0, 10] passed, 1000 games, 10+0.1, vs 561ad40)]
- *(eval)* Hold the pawn structure weights through a fit [bench 4089373]
- *(eval)* Refit the eight mobility weights on the whole archive [bench 3868987] [elo +76 ±25 (sprt [0, 10] passed, 500 games, 10+0.1, vs 2df11cb5b92afbea0561e5b34073230c262a220d)]
- *(uci)* Add a Move Overhead option in place of the constant fifty

### Bug Fixes

- *(search)* Answer a move at a root whose fifty move counter has expired [bench 4089373]
- *(eval)* Tell a seal drawn on the wrong archive from a pair with no rows [bench 4089373]
- *(search)* Hold a line of check extensions to the ply rail [bench 3868987]

### Refactor

- *(eval)* Move each leaf term into a module of its own under eval/ [bench 3868987]

### Documentation

- *(eval)* Take a file the reader cannot open out of the mobility comment [bench 3868987]

### Development

- *(workspace)* Build for x86-64-v2 by default
- *(release)* Ship the v2 build under the plain name
- *(ci)* Have the book table say what commit its books come from
- *(ci)* Take the match tools from the mache action instead of the tree
- *(ci)* Point the match documentation at the tool that reads the games
- *(docs)* Bring the readme, the roadmap and the map up to date with the code [bench 3868987]
- *(bench)* Let the named seal reach the labelling, not only the fit
- *(ci)* Read the published image's version, and pin the last loose action
- *(docs)* Correct four comments that describe a tree the code has left [bench 3868987]
- *(tactics)* Check a bm names a move the generator offers
- *(ci)* Put the Cinnamon paragraph back over the Cinnamon block
- *(tactics)* Write the strategy fixture as bytes so windows matches the pin

## [0.4.3] - 2026-09-12

### Features

- *(uci)* Let the reductions argument search a suite of its own [bench 4162584]
- *(eval)* Fit the twelve piece square tables to our own games [bench 4070288] [elo +54 ±13 (sprt [0, 10] passed, 2000 games, 10+0.1, vs d17622f)]
- *(eval)* Add the mobility term with its eight weights at zero [bench 4070288]
- *(eval)* Fit the eight mobility weights to our own games [bench 4053745] [elo +12 ±8 (sprt [0, 10] passed, 4000 games, 10+0.1, vs 96bad35)]
- *(eval)* Add a basic king safety term from pawn masks [bench 4053745]
- *(search)* Rank the quiet moves by a cutoff rate [bench 4020251] [elo +22 ±13 (sprt [0, 10] passed, 1500 games, 10+0.1, vs 96bad352)]
- *(eval)* Add the pawn storm to the king safety term [bench 4020251]
- *(eval)* Fit the fourteen king safety weights to our own games [bench 3882989] [elo +44 ±15 (sprt [0, 10] passed, 1500 games, 10+0.1, vs ccd1804)]

### Performance

- *(eval)* Count mobility only where a weight is not zero [speed +5.7% (bench nps, 41 interleaved rounds vs 54a6ceb, spread 20.1%)] [bench 4053745]
- *(eval)* Cache the king shelter by the pawn key and the two king squares [speed +4.9% (bench nps, 21 interleaved rounds vs 114fc59, spread 11.1%)] [bench 3882989]
- *(board)* Generate the evasions rather than filter them [bench 3882989] [speed +5.4% (bench nps, 21 interleaved rounds vs acf076d, spread 20.5%)]
- *(search)* Spare the swap two walks it always takes [bench 3882989] [speed -1.0% (bench nps, 21 interleaved rounds vs baf7a97, spread 14.6%)]

### Refactor

- *(eval)* Give four pieces an endgame table of their own [bench 4162584]

### Documentation

- *(search)* Say what the pruning threshold covers under its own weights
- *(search)* Re-measure the figures beside the reverse futility margin
- *(eval)* Record what the sealed games say about the king safety fit

### Development

- *(bench)* Add a recorder module the three recorders share [bench 4162584]
- *(ci)* Play a panel of engines rather than one lineage
- *(docs)* Record that the reduction floor guards cheap nodes
- *(docs)* Record that the wider pruning band bought no strength
- *(ci)* Let a run play a second opening book
- *(ci)* Let build_at.sh be told how to build and what to copy
- *(ci)* Take the name the calibrated engine plays under as an input
- *(ci)* Take what a match is played on as inputs
- *(ci)* Take the release note marker as an input
- *(docs)* Move the instrument reference out of DEVELOPMENT.md
- *(ci)* Make the four match tools a package
- *(ci)* Declare the test pins in the package rather than beside it
- *(ci)* Give the match tools a json mode
- *(bench)* Print what a position's evaluation is made of
- *(eval)* Pin the piece square tables on shape rather than on numbers
- *(bench)* Add the evaluation tuner and its loss harness
- *(bench)* Split the tuner's corpus into three groups by game
- *(bench)* Count the tuner's phase shares by appearance
- *(docs)* Say that the tuner splits by game and seals a fifth
- *(bench)* Label a tuner position from its own group alone
- *(docs)* Take the run results out of the instrument reference
- *(bench)* Read a terms row whose id holds a space
- *(bench)* Keep a terms row whose id opens with the header word
- *(deps)* Bump smallvec from 1.15.2 to 1.16.0 in the cargo group
- *(bench)* Carry each game's run and round on its corpus row
- *(bench)* Split the tuner's groups by the opening pair
- *(bench)* Add a final command that scores the sealed group once
- *(bench)* Add a learning curve to the tuner
- *(ci)* Pin how long a run's games are kept
- *(bench)* Harvest the strength runs' games into the corpus
- *(ci)* Put the match report in the log as well as the summary
- *(docs)* Say that a title names the thing it changed
- *(docs)* Replace the em dashes with the punctuation the house rule asks for [bench 4053745]
- *(docs)* Bring three claims up to date with the code [bench 4053745]
- *(ci)* Print the gauntlet rating margin as a 95% interval
- *(release)* Print the whole Speed trailer in the changelog
- *(release)* Read the stated benches against their pins before a release
- *(ci)* Hold an Elo trailer to a base that still resolves
- *(docs)* Date the 2332 gauntlet and state one game count
- *(bench)* Add the attention model's fitting script
- *(bench)* Hold the mobility weights through a fit of a later term
- *(tactics)* Say what the strategic suite's total can and cannot resolve
- *(release)* Play fifty games against each rung of the gauntlet
- *(release)* Take the gauntlet's bottom rung off and add one above the top
- *(release)* Swap the bottom stash for a fifth lineage at the top
- *(deps)* Bump taiki-e/install-action in the actions group

## [0.4.2] - 2026-09-07

Corrections to this section, written by hand after it was generated. The
generated lines below are left as they were published.

- The `[speed ...]` figures in this section and in the ones before it were
  printed without the spread they were measured against. 18 of the 22
  `Speed:` trailers between 0.3.10 and this release sit inside their own
  spread, which by the rule in `docs/DEVELOPMENT.md` makes them
  measurements stating no claim. Three of the 22 (`66aa260`, `594366a`
  and `911eaeb`) were measured across trees counting different benches,
  where a rate is not a like for like comparison either. The trailers in
  `git log` carry the spread, and sections from 0.5.0 on print it.
- Two entries here print `[bench 4395471]` and their trees pin 4162584:
  "Let the residuals argument search a suite of its own" (`ac47935`) and
  "Record that the reverse futility margin is where it should be"
  (`a0bf845`). Both were rebased after the bench workflow built them, and
  the trees they landed on count 4162584. `scripts/check_bench_pins.py`
  reads a message against its own pins and the release runs it from 0.5.0
  on, with these two on its acknowledgement list.
- Three entries here print `vs master` in their `[elo ...]`, which names
  whatever master was on the day the trailer was written rather than what
  was played. The bases, recovered from the Strength runs:
  - "Order captures by static exchange evaluation" (`dcdaab0`) was
    measured against `6bcd2440`, candidate `fb56979d`, in run 33964436951.
  - "Add a model gated two ply reduction" (`634083f`) has three runs
    behind it. Run 33964444015 played 840 games at [0, 10] for +10 ±19
    against `fb56979d`. Run 33964436951 played 840 at [-10, 0], which is
    the run above. Run 34029955963 played the 2,000 at [-10, 0] the
    trailer carries, candidate `2d40e50f`, against `50a54c7f`.
  - "Add late move pruning under the model gate" (`2057f39`) was measured
    against `634083f`, candidate `1b9dc346`, in run 34098727843.

  Run 33964436951 kept no artifacts, expired or otherwise. It ran before
  the workflow began keeping a manifest, so its bases come from its job
  logs, which are held for ninety days as the artifacts are. From 0.5.0 on
  `scripts/check_trailers.py` refuses a base that is not a commit or a
  release tag.

### Features

- *(search)* Record every reverse futility candidate, fired or not [bench 8182450]
- *(search)* Add principal variation search [bench 7184673]
- *(search)* Skip hopeless captures in quiescence [bench 6708286] [elo +50 ±24 (sprt [0, 10] passed, 530 games, 10+0.1, vs 24ef995)]
- *(search)* Add static exchange evaluation [bench 6708286]
- *(search)* Order captures by static exchange evaluation [bench 6146070] [elo +12 ±18 (sprt [-10, 0] inconclusive, 840 games, 10+0.1, vs master)]
- *(search)* Skip losing captures in quiescence [bench 5684750] [elo +24 ±19 (sprt [-10, 0] passed, 746 games, 10+0.1, vs 6e6fdec)]
- *(search)* Reduce late quiet moves by one ply [bench 4395471] [elo +44 ±22 (sprt [0, 10] passed, 648 games, 10+0.1, vs 899197c)]
- *(board)* Tell whether a move gives check without making it [bench 4395471]
- *(search)* Add a model gated two ply reduction [bench 4186515] [elo +11 ±12 (sprt [-10, 0] passed, 2000 games, 10+0.1, vs master)]
- *(search)* Add late move pruning under the model gate [bench 4162584] [elo +18 ±11 (sprt [0, 10] passed, 2000 games, 10+0.1, vs master)]
- *(uci)* Let the residuals argument search a suite of its own [bench 4395471]

### Bug Fixes

- *(board)* Drop a castle right or en passant square without its pieces [bench 4395471]
- *(uci)* Take startpos, fen and moves as whole words
- *(uci)* Refuse an argument word nobody recognises, and spell them once

### Performance

- *(search)* Score the quiet moves only when the search reaches them [bench 6146018] [speed +3.4%]
- *(board)* Mask the castling rights, step the repetition walk in twos [bench 4395471] [speed +3.5%]
- *(search)* Sort the keys and put the moves in order once [bench 4395471] [speed +2.1%]
- *(search)* Sort only the keys the move ordering scored [bench 4395471] [speed +6.4%]
- *(search)* Mask the squares the history table is read by [bench 4395471] [speed -0.9%]

### Refactor

- *(search)* Leave the swap's gain array uninitialised [bench 5684750]
- *(board)* Make Board's active_color, line_ply and key pub(crate) [bench 4395471]
- *(board)* Add try_make and try_undo for callers outside the crate [bench 4395471]
- *(magic)* Move the mailbox and the MAGIC static into magic.rs [bench 4395471]
- *(uci)* Read a go line once into a Go value
- *(uci)* Answer the three research commands through one path
- *(board)* Write the castling rights out from a table [bench 4395471]
- *(uci)* Move the instruments out of the protocol module

### Documentation

- *(board)* Say where the mailbox lives and how big it is
- *(search)* Say what Limits::starting_at is for

### Development

- *(bench)* Let the residuals command set the reservoir cap
- *(bench)* Score the terminal positions the replay reaches
- *(docs)* Correct three verdicts in the experiments ledger
- *(docs)* Record that on-demand move picking measured slower
- *(bench)* Count the rows whose claim overstates the reference
- *(docs)* Record the tuning that measured slower
- *(ci)* Count how the games in a match ended
- *(ci)* Keep the games and a manifest from every strength run
- *(bench)* Record which move cuts a sampled node off [bench 4395471]
- *(deps)* Bump the actions group with 2 updates
- *(docs)* Record the check exemption verdict
- *(ci)* Pool the shards of a match into one estimate
- *(ci)* Give each shard of a match its own slice of the book
- *(ci)* Split the strength match across shards
- *(bench)* Label each sampled reduced scout's fail low [bench 4395471]
- *(magic)* Import the test module's names in one use
- *(ci)* Play the calibrate rungs at the same time
- *(workspace)* Drop or fix the checks that were already being made
- *(workspace)* Clean up the docs, the comments and the test scaffolding [bench 4395471]
- *(docs)* Record the tuning this round measured slower
- *(workspace)* Hold the unsafe to the two crates that need it
- *(workspace)* Look for the licence notice everywhere a source file is
- *(ci)* Read a match's pairs as a sequential test
- *(ci)* Play an sprt in batches across the shards
- *(docs)* Correct and tidy the documentation
- *(ci)* Read the elo figure and its interval off the same pairs
- *(ci)* Carry an sprt's pairs between batches rather than its ratio
- *(tactics)* Convert the Strategic Test Suite to epd
- *(tactics)* Score the strategic suite beside the tactical one
- *(ci)* State the whole test in an sprt's trailer
- *(ci)* Drop the script tests that restate what another pins
- *(board)* Name the positions the tests repeat
- *(search)* Put the move ordering's repeated cases in tables
- *(uci)* Read the go line and the option cases from tables
- *(docs)* Record that the reverse futility margin is where it should be [bench 4395471]

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
