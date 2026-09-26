# Instruments

These measurements ask what the engine gave up rather than how large a tree it
walked. `residuals`, `cutoffs`, `reductions` and `effort` are arguments of their
own and measure the search, as does the bench's `audit` word. `terms` measures
the evaluation, and the last section is the offline harness under `scripts/`
that fits and scores the weights `terms` states.

[DEVELOPMENT.md](DEVELOPMENT.md) has the bench itself and everything else a
change needs before it is committed. Nothing in this file is needed for that.

`residuals` and `reductions` label what they sample by asking the reference,
`SearchConfig::reference()`, which is alpha-beta with every shortcut off, and
`terms` uses the reference's quiescence. DEVELOPMENT.md says where the default
parts company with it.

Every setting below is optional and takes its default when the line leaves it
out. A setting named with nothing after it is refused by name, as `effort 4
cap` is refused with `cap: no value`, and so is one named twice: a run takes
minutes, and one started at a default nobody typed answers a question that was
not asked.

All of these print to standard output; redirect it to keep a run. The four
arguments and `terms` print a row per sample, whitespace separated with the fen
last so a row parses left to right, under a header that states what the run
used, so it can be rerun from what it printed.

## What the shortcuts cost in accuracy

The bench says how much of the tree a shortcut removes. It cannot say how often
the shortcut was wrong to remove it, and that is what `arche residuals`
answers:

```
target/release/arche residuals [depth] [every <n>] [cap <n>] [epd <file>] [taint refuse|trust|skip|rule50]
```

It searches the bench's suite, or the one `epd` names, samples the nodes
reverse futility and the null move pass answered, and then asks the reference
what each of those positions is really worth. Those are the two shortcuts that
answer a whole node, which is what leaves the reference something to be asked.
The quiescence skips pass over a move rather than answering a node, and what
the late move reduction and pruning write off is the reduction ledger's
question further down.

The run is read for the crossing. A shortcut returns a lower bound and claims
it clears beta, so a claim well above what the position is worth is still a
sound fail high as long as the reference agrees the node fails high. What is
unsound is a reference answer below the beta that was cleared. The crossing is
`reference < beta`, strictly, since an answer equal to beta is a fail high the
shortcut was entitled to. The residual beside it, the reference's answer less
the claim, says how large the errors run.

The second label is the overstatement, `claimed > reference`, strictly: the
value handed up was more than the position is worth. A shortcut can clear beta
rightly and still overstate, and the cost then falls on the parent: the child's
claim never raises the parent's alpha, but it becomes the parent's fail-soft
best when no move does better, and the ceiling the parent stores is then too
low. The two labels are counted independently.

Each row is `kind depth window halfmove beta eval_beta claimed reference delta
crossed overstated fen`. The window is `zw` or `open`, read from alpha and beta
at the node (the pv, cut and all classification needs the node's outcome, which
a sample taken at a cutoff cannot know). Both shortcuts are refused at an open
window, so every row reads `zw`; the column stays so that a change to the
exemption shows in it. The halfmove column is the fifty move counter, pulled
out of the fen so rows filter on it without parsing one.

One kind is not a shortcut. A `shadow_futility` row is a reverse futility
candidate: a node where every gate but the margin test passed and the
evaluation stood at or above beta, recorded whether or not the test fired. The
fired rows alone cannot price a tighter margin, because every one of them stood
a whole margin above beta. A shadow row claims the same `eval - 100 * depth`
the live kind does, so on a candidate the margin declined the claim sits below
beta, and the crossing says whether a margin firing there would have been
wrong. A node answered from the table never reaches the margin test, so the
population is conditioned on a table miss, for the shadow as for the live kind.

The run ends with a line for each kind at each depth: the count, the crossings
and their rate, the overstatements, the mate references, then the minimum,
median, ninetieth, ninety-ninth and maximum of the deltas. It is not pooled
over depths, because the margin a shortcut risks grows with the depth left and
the depths are reached in wildly different numbers, so a pooled rate is the
shallowest depth's.

A mate reference is counted in the `mates` column and left out of the
percentiles: its delta is not a number of pawns, and the replay counts mate
distance from its own root. The labels still hold, since a mate score is above
or below every eval, which is all comparing it with beta or the claim asks.

The rate is a hash and not a counter. A node is sampled when
`position_key ^ salt(kind) ^ depth * odd` falls in the first `1/n` of the
range, so `every 200` takes about one node in two hundred, and takes the same
nodes whatever order the search reached them in. A counter would pick nodes by
when they were visited, and a change to the tree would then move the
membership in ways that read as a shift in the distribution. Two runs of the
same command print the same rows. Rows from before hash sampling landed sample
different nodes and are not a baseline for rows from after it.

The header states `events`, every node the shortcuts answered, beside
`records`, the ones kept. A crossing rate cannot be read without that
denominator: a zero over four hundred records is not a zero over four hundred
thousand events. Raise the rate, or say from the events how small a rate the
run could have seen.

The buffer holds ten thousand samples unless `cap <n>` asks for another, and
the header says when it had to drop some. Past the cap it keeps the smallest
keys rather than the first arrivals, which is a uniform draw from the whole run
and the same draw in any order. That holds for the set of keys and not for
what sits behind a repeated one: a deepening search revisits a position at a
depth, so keys tie, and which member of a tied group survives the cap depends
on the order. The kinds share the buffer, so past the cap each keeps a share in
proportion to its volume, and the shadow kind's is the largest; a run that
wants the live kinds whole raises the cap. The events count counts offers, so a
fired reverse futility node counts twice, once live and once shadow.

`epd <file>` searches a suite of its own instead of the bench's, in the format
`bench.epd` is written in, and the header names it. That is what lets a
threshold be chosen on one set of positions and read back on another: a margin
fitted on the bench's eighteen and reported as an improvement to the bench is
circular. `arche-core/tactics.epd` and `arche-core/strategy.epd` are positions
the bench does not hold. A file that will not open, holds no position, or holds
one the board will not take is refused. The reduction ledger, `effort` and
`terms` take the same word on the same terms.

The replay waits until the suite is finished and runs on an engine and a table
of its own, cleared before every sample, because a reference search inside the
measured one would store entries in the table the measured search reads.

The fen carries the fifty move counter and not the path, so the replay cannot
see a repetition that needs moves made before the node: the reference value is
the answer to the position as a diagram, and a residual from deep in a shuffle
is read with care. The taint policy differs between the phases too: the
recording runs whatever it was given, `rule50` by default, while the reference
refuses every tainted cutoff, so a crossing on a row with a high halfmove count
can be the two policies disagreeing about a draw.

The command is an argument and not a uci command: nothing about a live session
wants it, and it takes minutes at the depths worth running it at.

## Which move cuts a node off

The bench says how much of the tree the move ordering saves. It cannot say
which moves are doing the saving, and that is what `arche cutoffs` records:

```
target/release/arche cutoffs [depth] [every <n>] [cap <n>]
```

It searches the bench's suite under the default configuration and samples the
full width nodes as they answer: one event per node, at the cutoff or at the
loop running out. Both outcomes are recorded at the same rate on purpose. A
cutoff censors every move ordered after it, which is what biases a raw history
count, and a stream of cut nodes alone would reproduce that censoring.
Quiescence and the root are out of scope: quiescence cuts on capture order, and
the root searches every move.

Each row is `depth window check outcome generated searched index class history
history_max scored tt eval_beta cost reduced fen`. A `cut` row names the
cutting move's place among the searched moves (`index`, 0 for a table move
searched first), its `class` (`table`, `capture`, `promotion`, `killer` or
`quiet`, material first, so a capture that is also a killer is a capture), the
history table's score for it (`history`, quiet moves only and signed, since a
move tried more often than it cuts sits under zero), and whether its answer came
through the reduced scout (`reduced`). A `held` row prints `-` in those four
columns.

`generated` and `searched` are what the list held and what the loop made; the
censored count is the reader's subtraction, and a node its table move cut
before anything was generated says `generated 0`. `history_max` is the largest
history score among the generated quiets, clamped at zero, which is what
`history` is read against; it is 0 when every one of them is marked down.
`scored` says whether the staged ordering ever scored the quiet band at this
node. `tt` is what the probe gave: `miss`, `move`, or `score_only` for a hit
whose move was not playable here. `eval_beta` is the static evaluation less
beta, computed at record time for kept events alone, so the measured search
never evaluates a node it would not have. `cost` is the nodes spent under the
node.

The sampling is the residuals mechanism with a salt of its own, and the header
states `events` beside `records` for the same reason. Every full width node the
move loop answers is an event, so the stream is far denser than the shortcut
sampler's and the deepest nodes are the rarest in it; a run that wants them
lowers `every` or raises `cap`.

The run ends with a line per depth: the records, the cut rate, the share of
cuts at index 0, at 1 to 3 and past 3, the mean moves searched at cut and at
held nodes, the class shares, and the share of cut nodes whose quiet band was
never scored. The rows are the product; the summary is a sanity read.

Recording changes nothing: an armed engine's node counts equal a disarmed one's
position by position, which `recording_leaves_the_measured_search_where_it_was`
in `arche-core/src/census.rs` asserts. The residuals, reductions and effort
recorders carry a test of the same name.

## What trusting a reduced scout costs

The census says which moves cut. It cannot say what the late move reduction
buried, and that is what `arche reductions` measures:

```
target/release/arche reductions [depth] [every <n>] [cap <n>] [epd <file>]
```

It searches the suite under the default configuration and samples the reduced
scouts as they answer. A scout that fails low is trusted, and its move is never
searched at the node's depth. A scout that fails high has earned the full
depth, so its cost is the wasted scout rather than a wrong answer, and it is
never replayed; the fail highs stay in the stream at the same rate because they
are the denominator a reduction policy is read against.

A fail low is labelled by a replay under the reference, on the residuals
replay's terms. The fen on a row is the position the reduced move left, so its
side to move is the side the move was played against. The replay searches it to
the node's depth less one (the depth the move was denied) over the full window,
and negates the answer to the reducing node's side. Strictly above the alpha
the scout was read against is `harmful`: the full search would have raised
alpha on a move the scout wrote off. Anything else is `harmless`.

Each row is `depth window index searched generated history history_max killer
tt eval_beta alpha_gap alpha scout cost reference label reduction fen`. `depth`
is the reducing node's, its check extension included. `index`, `searched`,
`generated`, `history`, `history_max` and `tt` are the census's columns, read
at the decision; `history_max` is read once at the node's first gated or staged
move and held, as the gate holds it. `killer` says whether the move stood in a
killer slot. `eval_beta` and `alpha_gap` are the node's static evaluation
against its two bounds, computed at record time for kept events alone. `alpha`
is the bound the scout was asked about, `scout` is `low`, `high` or `skipped`,
and `cost` is the nodes the scout spent. A fail high prints `-` in the
`reference` and `label` columns.

`skipped` is the pruning rules'. A move the attention model prices in its
deadest band at depth four and up, and a quiet at depth one to three that
either shallow rule drops, are never scouted, so a sampled skip is recorded
where the loop passes it over, with a cost and a reduction of zero, and
replayed as a fail low would be. Its `searched` count stands one past the
index, as a scouted row's does, because that is the model's feature (ledgers
printed before 25 September 2026 have the index there instead). The recorder
makes and unmakes the skipped move around the record, and a move that turns
out illegal is not recorded, because the skip denied it nothing.

A skipped row at depth one to three is a shallow rule's and one at four or more
is the model's, since the model decides from four and both shallow rules stop at
three. The ledger does not say which shallow rule took a row, and the two
overlap on the same moves, so an ablation (`effort`) is what separates them.

A depth one row is labelled against a deeper search than the skip denied.
`Event::replay_depth` in `arche-core/src/reduction.rs` floors `depth - 1` at
one, because `residual::reference_answer` has no depth zero form, so quiescence
is not an answer it can give. Rows at depths two and three are labelled against
the search actually denied. So read the three shallow rates one at a time and
do not pool them. Which way the extra ply biases the depth one rate has not
been measured.

The sampling is the census's. Only a late quiet at a node deep enough to reduce
offers a scout, so the stream is sparser than the census's, and every fail low
kept costs a reference search in the replay. The attention model was fitted on
a bench run, so a threshold chosen off these rows is read back on
`arche-core/tactics.epd` or `arche-core/strategy.epd` rather than on the bench.

The run ends with a line per depth: the scouts (skipped rows are counted apart,
so the fail low share keeps its denominator), the skipped count, the fail low
share, the replayed count, the harmful count and rate, and the harmful rate
split by index band (4 to 7, 8 to 15, 16 and past) and by history fraction
(zero, under a tenth, under half, half and up, then a fifth cell for a move the
table has marked down, printed last so older lines read the same in their first
four). A cell under thirty replayed rows prints its counts instead of a rate,
and the line's own rate prints `-`: a percentage over a handful of rows reads
as a finding and is noise.

## What a rule frees, and where the freed effort goes

The three above each describe one tree. A saving is a difference between two,
so none of their rows can carry one, and that is what `arche effort` does:

```
target/release/arche effort [depth] [every <n>] [cap <n>] [epd <file>] [off <switch>[,<switch>]] [budget <n>]
```

It searches the suite twice, once under `SearchConfig::default()` (the
candidate, `on`) and once with the switches `off` names turned off (the
baseline), and joins the two runs by the node. The join works because the
sampling key is a function of the node and nothing about the run: both sides
record under one lane, so a key one side holds and the other does not is a
fact about the trees rather than the buffers.

`off` names a switch from `SearchConfig::SWITCHES` in `engine.rs`, and a run
naming anything else is refused and told what a switch may be. A field the
table leaves out fails `turning_every_switch_off_gives_the_reference`.
`taint` is not among them: it is a policy with four values, and `residuals` takes it.

`off` may name two switches joined by a comma, `off
late_move_count,quiet_futility`, and the baseline then has both off. Read
against the two singles and the null, a pair says whether two rules' savings
multiply, as rules on unrelated parts of the tree would, or whether the pair
frees more or less than that. A keyword given twice is refused, so a pair is
one word; the same switch twice and a third switch are refused too. Where one
switch of a pair is only ever asked under the other (`adaptive_null_move` under
`null_move`; `deep_reductions`, `late_move_pruning`, `reduction_table` and
`deep_index_rule` under `late_move_reductions`; `deep_index_rule` under
`deep_reductions`), the pair searches as many nodes as the outer single,
position by position, which a test holds.

**`off` absent means both sides are the default**, which the header says as
`off none`. That run is the null, and it is the one to take first (see the end
of this section).

Each joined key is one of three outcomes. `both` is a node in both trees, so
the difference in what sat under it is effort the rule moved. `only_off` is a
node only the baseline reached, so what sat under it is effort the rule removed
outright. `only_on` is a node only the candidate reached, so what sits under it
is effort the rule created, which is the population nothing else here reads.

Each row is `depth outcome visits_on visits_off cuts_on cuts_off cost_on
cost_off delta fen`. `visits` is how many times that side's move loop answered
this position at this depth over the whole deepening; a rule that changes how
often a node is re-reached changes it, and that change is itself reallocation.
`cuts` is how many of those visits ended in a cutoff. `cost` is the nodes spent
under the node over that side's visits, quiescence included. `delta` is
`cost_on - cost_off`. No column prints `-`: the absent side of an `only_` row
spent nothing, and a 0 lets the column be summed. The fen is the candidate's
where it has one, and the key covers neither the fifty move counter nor the move
number, so the two sides at one key can carry different ones. The events are
offered where the census offers them, so quiescence and the root are out of
scope here too.

The run ends with a line per depth and a line per position. **The depth line's
`nodes on` and `nodes off` are exact and not sampled**: every offered event is
counted on each side, so they read the same at any `every`, and the sampled rows
are for attribution. The cost columns on that line do not add down the depths,
because a depth three node's cost holds its depth one descendants'; the node
columns do. The position line is exact too, and its node counts are each side's
whole search, quiescence included, so at the bench's depth and configuration
the candidate's equals the bench position by position; that line is where a
switch's whole tree delta is read. The per depth tallies count full width nodes
alone, about a fifth of the whole at the bench's depth, so they are checked
against each other and never against the bench.

`budget <n>` holds both sides to a node count as well as to the depth, and
they stop at whichever comes first. A side the budget binds spends exactly it
(except below the cost of depth one, which always runs to its end), so the
speed channel is held out and the position line reports the depth each
side finished (`reached`) and the move each chose (`best`). An iteration the
budget cut short still answers with a move that beat its alpha, so `best` is
what that side would play while its score is a floor rather than a value. That
is an offline reading of what a node budget buys, at the cost of two searches
rather than a match.

Two guards sit on the join. **The trim**: a reservoir keeps the smallest keys
it is offered, so if one side overflows and the other does not, a key kept on
one and dropped on the other would read as `only_on` or `only_off` and the
buffer would manufacture the finding. After both runs the smaller of the two
sides' retained bounds is taken and every row at or above it is dropped from
both, which the header states as `bound` and `trimmed`; a run where neither side
overflowed says `trimmed 0`. **The collision guard**: two positions can agree on
a 64 bit key, and a key whose visits disagree about the node is counted as
`collisions` and dropped.

What it cannot see: a change that moves no node reads as `both` with a zero
delta everywhere, and is priced by instructions and the clock instead. A rule
with no switch has to be given one first. A rule that moves effort inside
quiescence moves the cost columns without rows of its own. And a node count is
not a time: the quiet futility margin was 0.50% of the bench by count at
`b9325ae` and less than that by work.

**Take the null run first.** `effort 9` with no `off` searches the same
configuration twice, so every row must read `both` with a `delta` of 0, and the
two sides' node counts must be equal at every depth. Anything else is the
instrument and not the tree, and no reading is worth quoting until that run is
clean.

`effort.rs` holds its recording to the census's test, and
`recording_changes_nothing_under_the_baseline_configuration_either` asks the
same of a side with switches off, since this is the one instrument that
searches under a configuration its caller chose. There is no pinned count for
a configuration with a switch off, and there should not be one, since it would
need rewriting whenever the search gained a rule.

## What a position's evaluation is made of

This one measures the evaluation, and it is the engine's half of the tuner:

```
target/release/arche terms [epd <file>]
```

It searches nothing to a depth, so it takes no depth. It reads the bench's
suite, or the one named, keeps the positions that are quiet, and prints what
each one's evaluation is made of.

The evaluation is material plus a tapered piece square score plus the tapered
leaf terms, which are linear in the numbers they are read from, plus an
untapered pair term, which is not. So a position's score is a dot product of
the position against the weights plus that term, and a row is the position's
half of the dot product and the term's score: for every weight the position
touches, the integer that weight is multiplied by. The weights are a flat
vector: the 384 midgame table entries, the 384 endgame ones in the same order,
the six material values, then each leaf term's midgame weights followed by its
endgame weights. A slot's table entry is a square as black sees it, because
black reads the tables as they are written.

What each leaf term counts is in its own file under `arche-core/src/eval/`.
Every count is carried as white's less black's, in the side to move's frame.
Two details matter to a reader of the rows. The king attack count is taken
over the real occupancy with nothing subtracted, so a square two pieces attack
counts twice. And the rows carry every term's coefficients whatever its
weights hold, which is how a fit prices a term before any of its weights is
worth anything.

The line after the header states the layout, so that what reads these rows
holds no copy of it:

```
layout midgame 384 endgame 384 material 6 mobility 4 shelter 7 pawn_structure 8 king_attack 4
```

The first three are runs of slots. The names after them are the leaf terms in
vector order, each with the counts it is measured in per side and per half of
the taper, so it takes twice that in slots, midgame half first. The line is
spelled off `eval::TERMS`, the list the engine lays its slots out from, so a
term added there is named here without a second edit. `scripts/tune.py` reads
its slots from this line and refuses a run naming a term it has no bounds for.
A run with no layout line was printed by an engine older than the line, whose
slots would be read against the wrong weights, and is refused too.

The line after that is `weights <n> <w0> <w1> ...`, the vector as the live
tables hold it, so that nothing reading these rows transcribes psqt.rs.

Each row after that is `id eval phase n slot:coefficient... fen`. While the
pair term is on, a `factors <rank> <scale>` line follows the weights and each
row carries the term's score, from the side to move, as a fourth number after
`n`. Both ends can
hold spaces: a fen is six fields, and an id is whatever the epd put in the
quotes ("ruy lopez", "7th Rank.001"), or the fen itself when the epd names none.
So a row is read from the end whose width is fixed: the fen is the last six
fields, the coefficients are the run of `slot:coefficient` in front of it, and
what is left before the numbers is the id. `n` is printed so the two ends
can be checked against each other. The coefficients are in the side to move's
frame, so the row's own arithmetic is the evaluation:

```
eval = mat . w_mat + trunc((psqt . w_psqt + mobility . w_mobility
                            + shelter . w_shelter + pawns . w_pawns
                            + king_attack . w_king_attack) / 24)
```

Four things in that line are each a way to be wrong by a centipawn. Every leaf
term is inside the divide beside the piece square half, so the numerator is
truncated once. The divide truncates toward zero, where python's `//` floors.
The material is added outside the divide: `trunc((24 * 1 + -5) / 24)` is 0
where `1 + trunc(-5 / 24)` is 1. And the phase is capped at 24 before the
coefficients are written, because promotions can leave more on the board than
the opening had. Each has a test in `arche-core/src/tune.rs`.

Nothing outside the engine is told how to evaluate a position, which is why the
argument exists: a second implementation of the evaluation diverges quietly and
still produces plausible weights. So `reconstruct` folds a row back against the
live tables and has to give what `eval` gives, exactly.
`a_positions_terms_reconstruct_its_evaluation` asks that over the shared test
positions, the bench's suite and the strategic suite, and the run asserts it on
every row it prints and panics rather than dropping one.

A position is kept when three things hold. The side to move is not in check. A
capture search comes back at the static evaluation, so the side to move has
nothing to win by capturing. And the same holds after a pass, so the opponent
has nothing to win either. The pass makes the test two sided: a one sided test
keeps a position where the side to move is about to lose a hanging queen, and
labels an evaluation that misses it with the result of a game that did not.
The capture search is the reference's, because the default's quiescence skips
captures it prices as hopeless, and a corpus whose quietness was decided by a
guess would carry the guess into every weight fitted on it.

The header states what the run turned away beside what it kept:

```
terms positions 18 in_check 1 unsettled 8 drawn 0 kept 9
```

`drawn` counts the positions whose material cannot mate, which the evaluation
answers with a hard zero rather than a sum over the weights, so every weight
vector scores them the same and they are turned away. `tune.py` refuses a
header without that count, because an extraction by an older engine holds those
rows. If the yield ever leaves too few positions to fit the weights, dropping
the pass is the fallback, and the header is what makes that a decision.

The argument arms no reservoir and runs no measured search, so it cannot move a
node count.

## The loss of a weight vector

The rows above are the input to `scripts/tune.py`, which scores a weight vector
against the games the positions came from and fits a new one. Its docstring,
and those of `scripts/build_corpus.py` and `scripts/groups.py`, carry the
reasoning behind the rules below.

```
python3 scripts/tune.py loss --terms rows.txt --corpus corpus.epd
python3 scripts/tune.py fit --terms rows.txt --corpus corpus.epd --out fit.json
```

`scripts/build_corpus.py` builds the corpus from archived strength-run pgns: one
row per unique post-book position, carrying its game, the run and round the game
was played in, the result from the side to move's point of view, and how many
times the games reached it. `scripts/harvest_games.py` downloads the strength
runs' game artifacts into an archive and rebuilds the corpus from the whole of
it:

```
python3 scripts/build_corpus.py runs/*/games.pgn --out corpus.epd
python3 scripts/harvest_games.py --archive runs --out corpus.epd
```

Run the harvest after every arm. A games artifact lives ninety days (the
`retention-days` on the strength runs' upload), and one that expires before it
is harvested takes games that cannot be played again. The run prints when the
next artifact expires. Running it twice downloads nothing the second time: an
artifact is held once its directory carries a `.harvested` marker, written
after the download, so an interrupted fetch is taken again. It takes the
strength runs and nothing else. A calibrate game is against another engine, and
a corpus of two sources could not attribute a loss change to either. The
archive and the corpus are gitignored, since at a release's scale they are
hundreds of megabytes.

The rebuild reads the whole archive every time rather than appending, because
which group a repeated position belongs to depends on every game in it. So the
archive is what must not be lost; the epd is minutes of arithmetic away.

The book's plies are dropped from every game and a game that ended in anything
but play is dropped whole. The known caveat is that these are the engine's own
games: positions it never reaches are unlabelled and its mistakes are labelled
as normal play.

The objective is occurrence weighted: a unique position carries the weight of
how many times the corpus reached it. Every loss, interval and share reads the
count, and the header names a phase bucket's positions and its appearances
separately.

The split is by game, in three groups, and the two games that played one
opening with the colours reversed go together. A game is keyed by the sha256 of
its movetext and a pair by the sha256 of its two games' keys, and the first byte
of the pair key modulo five places it: three fifths train, a fifth is the
selection group, and a fifth is sealed for calibration. Both keys depend on the
movetext alone, so a re-extraction puts every game back where it was. A split
on the position would put a row's neighbours, a move away and carrying the same
label, in the training set. A position two games reached belongs to the group
of the lower key and is labelled from that group's games alone; its
appearances in other groups are dropped rather than merged.

The ridge is chosen on the selection group and the loss is reported there. The
sealed rows are not in the matrices `loss`, `cv`, `fit` and `curve` score, so
none of them can reach one, and `the_calibration_group_is_not_read_by_a_fit`
says so by running the same fit twice. What that costs is the appearances
dropped from the other groups, so a position reached in two groups carries
fewer appearances than the corpus gave it.

The run is read off the `manifest.txt` beside a strength run's `games.pgn`, as
the run id and shard (with the batch between them where the run chained
batches), or off the directory's name; the round is the pgn's `Round` header.
Neither is part of the key. They are on the row so a source can be excluded or
weighted after extraction.

The one door into the sealed group is `final`:

```
python3 scripts/tune.py final --terms rows.txt --corpus corpus.epd --weights fit.json --log final.log
```

It takes a vector already quantized to the integers that would ship, and
before it reads a sealed row it appends a line to the log naming the corpus,
the sealed games, the extraction and the vector by checksum. A corpus the log
names is refused, and so is one whose sealed games the log names under another
corpus, so neither a new filename nor a re-extraction reopens the same games.
It prints the frozen vector against the shipped one on the sealed rows (both
losses at real and integer weights, the paired difference with its interval
clustered on the game, the loss by phase bucket) and the residual quantiles,
which are an empirical diagnostic and say so. A coverage claim needs its
sampling unit, score, exchangeability, quantile rule and target named before
the group is opened. A vector revised after the reading needs sealed games the
corpus did not hold.

`--sealed <file>` names the sealed group by pair key, one to a line, instead of
drawing it from the keys, which seal the same fifth every time. That is how a
run says the games that settle a revised vector are ones played since. Both
halves of the tuner take it and both must be given the same file, or the
corpus is labelled by one group and fitted holding out another. `final` still
refuses a second reading of the same sealed games.

Whether the corpus is big enough is `curve`:

```
python3 scripts/tune.py curve --terms rows.txt --corpus corpus.epd --out curve.json
```

It refits at an eighth, a quarter, a half, three quarters and the whole of the
training pairs, five draws by pair at each size below the whole, and reads
every fit on the same selection group with the fit's own ridge, weighting and
scaling constant. The summary gives, per size, the draws fitted and refused,
the mean, least and most loss, and the mean interval. A curve flat between a
half and the whole says more games will not lower the held-out loss at this
parameter count; one still climbing says they would. The json keeps every fit
with the pairs it drew, so a run can be replayed, and `learning_curve`'s
docstring has the reasoning.

`build_corpus.py`'s counters open with `runs`, carry `pairs` and `unpaired`,
and end with `repeated` (positions more than one game reached), `straddled` and
`dropped_appearances` (how many of those were reached from more than one group
and what that cost) and `same_key`, the games whose movetext another game
already had. `same_key` is the only place a game archived twice shows up, and
every other number reads it as two games. It is not expected to be zero, since
two games can be played move for move the same; a game archived twice climbs
with the artifacts and a repeated game climbs with the games. A full harvest
read `same_key 10` over 23,175 games (53a66dc) with no run archived twice, and
what those ten are was not established.

The loss is Texel's, the mean squared error between the game result and a
logistic of the evaluation, with log loss printed beside it; if the two
scoring rules disagree about a candidate that is worth seeing. The scaling
constant K is fitted once on the training games at the shipped weights and held
there, because K and the scale of the weights are one degree of freedom and
the scale is not free: `REVERSE_FUTILITY_MARGIN`, `DELTA_MARGIN` and the
ledger's `eval_beta` column all assume a pawn is about a hundred.

Scoring a vector over the corpus is one matrix-vector product, so a candidate
evaluation term is one appended column whose held-out loss can be read before
there is engine code for it. That is triage and not a verdict. A difference
that matters is one larger than its own interval: two vectors are scored on the
same selection positions, so the difference in per-position error is paired,
and the run prints its mean with a standard error and marks one that sits inside
its interval. The interval is taken over games rather than positions, since a
game's rows share a result and move together; the run prints both standard
errors and the design factor between them. There is no loss-to-elo mapping. The
sprt says elo.

Comparing two ways of fitting is `cv`:

```
python3 scripts/tune.py cv --terms rows.txt --corpus corpus.epd
```

Five folds by the second byte of the game key (the first placed the game in its
group, so folding on it would leave two folds empty), each refitting on four
fifths of the training and selection games with its own K and scored on the
fifth. The sealed group is not in what `cv` reads. It answers whether a way of
fitting is worth anything, not which ridge `fit` should use: the line naming
the best penalty says which games it is best over, so it is not pasted into
`fit --penalties` as the ridge the fit would have chosen.

Three more figures are printed, because a loss alone hides what a fit did. The
loss by phase bucket shows a fit that improves the endings by hurting the
middlegame. The per-slot support counts say how many rows each weight is fitted
on. And each vector's table scale is printed beside a K refitted for that vector
alone: with K held, a fit given enough licence can spend the loss on growing
the tables rather than their shape, and a vector whose loss only falls at its
own K bought scale. The scale is a ratio of root mean squares over the table
half of the vector, so it means one thing inside a layout and nothing across
two; figures from fits at different layouts are quoted with the layout beside
them or not together.

The fit is ridge toward the shipped weights rather than toward zero. So a
re-tune leaves alone the one direction the corpus cannot see (a constant added
to both king tables, which cancels between the colours) and holds the slots
with no support, the pawn tables' back ranks, at their zeroes. The ridge is
chosen from a grid on the selection games, each penalty fitted on the training
games alone, and a fit whose tables have grown past what the packed halves can
carry is refused whatever it scores. The selection loss beside the chosen vector
is therefore the fit's own best case; the sealed group is where an honest
interval comes from.

`MATERIAL` is held by default, because the delta margin in quiescence reads it,
so moving it changes the tree for a reason unrelated to the evaluation's
accuracy; `--free-material` lets it move. `--hold tables` holds the table
entries, and `--hold <term>` any leaf term the layout line names, given once
for each term held; a name the line does not carry is refused before the rows
are read. A hold freezes both halves of the term's taper.

A fit of a new term holds everything older: the shelter was fitted under
`--hold tables --hold mobility`, the pawn structure under those and `--hold
shelter`, and the king attack zone under those and `--hold pawn_structure`. A
refit of an older term on a grown corpus holds every other term instead, so a
mobility refit is `--hold tables --hold shelter --hold pawn_structure --hold
king_attack`. The holds follow from the one term the run means to move.

## What the table's key signature costs

An entry keeps thirty two bits of the position key rather than all sixty four,
so two positions can agree on the bits the table compares and the search reads
one as the other. The entry's comment puts that at about one probe in a
thousand million. `audit` counts it:

```
target/release/arche bench hash 1 audit
```

It keeps the full key of every entry beside the entries and prints two lines of
whole-suite totals after the table. At e67d711, at the bench's depth on a one
megabyte table:

```
signature audit: probes 4446265, hits 921512, comparisons 11416161, false accepts 0 (0.003 expected), false accept cutoffs 0, aliased evictions 0
narrow signature: 16 bit accepts 162 (174.194 expected), 24 bit accepts 3 (0.678 expected), 28 bit accepts 0 (0.040 expected)
```

The counts move with any change to the tree or to what the table keeps, so read
the shape and not the digits.

The word turns the shadow keys on for the tables the bench builds and nothing
else, so a session that plays games never allocates them. A false accept hands
back what it would have handed back unaudited. An audited table starts empty,
because an entry stored before the audit has no key beside it.

Probes, hits, comparisons and false accepts count keyed lookups; aliased
evictions count stores. A comparison is one live entry a probe really compared
its signature against whose full key belonged to another position, so each is
one chance in two to the signature's width, and every expectation is drawn from
that total. An aliased eviction is a store that landed in a slot the signature
said was its own and replaced another position's entry. A store the depth
contest turned away after comparing itself with a foreign entry is a related
cost but evicted nothing, so it is not counted.

The thirty two bit count cannot say anything on its own: a run of this size
expects a few thousandths of a false accept, so a zero is what a working
instrument and a dead one both print. The narrow line is the check. It counts
the comparisons a narrower signature would have accepted and this one refused,
at sixteen, twenty four and twenty eight bits. Sixteen is the same rate scaled
by sixty five thousand, so a count near its expectation says the rate really
does scale by two to the minus the width on this workload, and the thirty two
bit expectation can then be believed where its observation cannot.

Twenty four and twenty eight are the widths left if four or eight bits went to
other metadata, which is what they answer. Neither is measurable at the sizes
the command runs at: their expectations are under one accept, so a zero or a
three is the run being too short to have an opinion, and what the reader takes
from them is the expectation. The widths are cumulative (an entry agreeing on
twenty four bits agrees on sixteen and counts under both), so each count is read
against its own expectation and the three are never added.
`the_counts_fall_as_the_width_rises` in `arche-core/src/transposition.rs` checks
the counts fall as the width rises. The narrow figures are counted and never
acted on: a search running one of these widths would have stopped its scan at
the first entry it accepted, which is a different tree.

Read the narrow line off a small table. An entry is sixteen bytes, so a
megabyte holds sixty five thousand, and the bench's default sixteen megabytes a
million. The larger positions store more entries than a one megabyte table has
slots, so it fills and its comparisons run past ten million. The default table
fills on no position: the largest store is kiwipete's 402,233, and the audited
default bench reads 30 sixteen bit accepts against 18.367 expected. The command
does not sweep sizes itself.

The audit costs eight bytes an entry, half the table's own size again, and
refuses to run rather than run unaudited if the memory is not there. An audited
run and a plain one search the same tree, which the node counts show.
