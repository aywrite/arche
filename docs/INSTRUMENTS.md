# Instruments

Five measurements ask what the engine gave up rather than how large a tree it
walked. Four are of the search: three are arguments of their own, `residuals`,
`cutoffs` and `reductions`, and the fourth is the bench's `audit` word. The
fifth, `terms`, is of the evaluation. Each has a section here saying what it
answers and how to read what it prints, and the last section is the offline
harness under `scripts/` that fits and scores the weights `terms` states.

[DEVELOPMENT.md](DEVELOPMENT.md) has the bench itself, along with the build,
the tests and everything else a change needs before it is committed. Nothing
in this file is needed for that.

Three of the five answer by asking the reference, `SearchConfig::reference()`,
which is alpha-beta with every shortcut off. DEVELOPMENT.md's bench section
says where the default parts company with it.

## What the shortcuts cost in accuracy

The bench says how much of the tree a shortcut removes. It cannot say how
often the shortcut was wrong to remove it, and that is the question
`arche residuals` answers:

```
target/release/arche residuals [depth] [every <n>] [cap <n>] [epd <file>] [taint refuse|trust|skip|rule50]
```

It searches the same suite the bench does unless `epd` names another file,
samples the nodes reverse
futility and the null move pass answered, and then asks
`SearchConfig::reference()` what each of those positions is really worth.
Those are the two of the default's shortcuts that answer a whole
node, which is what leaves a reference something to be asked about. The
delta margin and the losing capture skip pass over a move in quiescence
rather than answering a node, and what trusting the late move reduction's
scout costs, like what late move pruning's skip writes off, is the
reduction ledger's question further down.

What the run is read for is the crossing. A shortcut returns a lower bound
and claims it clears beta, so a claim well above what the position is worth
is still a sound fail high as long as the reference agrees the node fails
high; what is unsound is a reference answer below the beta that was cleared,
because the node was cut off and should not have been. The crossing is
`reference < beta`, strictly, since an answer equal to beta is a fail high
the shortcut was entitled to. Beside it the residual, the reference's answer
less the claim, says how large the errors the crossings come from run.

The second label is the overstatement, `claimed > reference`, strictly: the
value the shortcut handed up was more than the position is worth. It is
distinct from the crossing. A shortcut can clear beta rightly and still
overstate, and the cost then falls on the parent rather than at the node: a
child's claim arrives at or below the parent's alpha and never raises it,
but it becomes the parent's fail-soft best when no move does better, and the
ceiling the parent stores is then one too low. The two labels are counted
independently and a row can carry either without the other.

Each sample is a row of `kind depth window halfmove beta eval_beta claimed
reference delta crossed overstated fen`, whitespace separated with the fen
last so a row parses left to right. The window is `zw` or `open`, read from
alpha and beta at the node: the fuller pv, cut and all classification needs
the node's outcome, which a sample taken at a cutoff cannot know. In practice
principal variation search puts every child after a node's first inside a
zero width window, so the column reads `zw` on nearly every row, and an
`open` row is a node still inside its window's first move, or inside the
re-search a zero width fail high asked for. The halfmove column is the fifty
move counter, which travels in the fen too and is pulled out so rows filter
on it without a fen being parsed.
Nothing is written to a file; redirection is the file mechanism here as
everywhere else in the tooling.

One kind is not a shortcut. A `shadow_futility` row is a reverse futility
candidate: a node where every gate but the margin test passed and the
evaluation stood at or above beta, recorded whether or not the test fired.
The fired rows alone cannot price a tighter margin, because every one of
them stood a whole margin above beta, so the region a tighter margin would
newly fire on is empty in them. A shadow row claims the same
`eval - 100 * depth` the live kind records, and the two therefore agree on
a node that fired (a pair only a run at `every 1` can show: the salts keep
the kinds' kept sets apart at coarser rates). On a candidate the margin
declined, the claim sits below the beta beside it, and the crossing then
says whether a margin firing there would have been wrong. Nothing was
handed up on a shadow row, so its overstatement reads the same way: what a
margin firing there would have claimed too much. The candidates
are the ones the margin test reads: a node answered from the table never
reaches it, so the population is conditioned on a table miss, for the
shadow exactly as for the live kind.

The run ends with a line for each kind at each depth: the count, the
crossings and their rate, the overstatements, the mate references, then the
minimum, median, ninetieth, ninety-ninth and maximum of the deltas. By depth
and not pooled over the depths, because the margin a shortcut risks grows
with the depth left and the depths are reached in wildly different numbers,
so a pooled rate is the shallowest depth's rate wearing every depth's name.

A mate reference is counted in the `mates` column and left out of the
percentiles. Its delta is not a number of pawns, and the mate distance the
replay reports counts from the replay's own root while the claim's counts
from the root of the recorded search, so the two are not measured from the
same place. The two labels survive all of that and are what such a row is
read for: a mate score is above every eval or below every eval, which is
all that comparing it with beta, or with the claim, asks. So a claim above
a mated reference counts as an overstatement and a claim below a mating one
does not.

The rate is a hash and not a counter. A node is sampled when
`position_key ^ salt(kind) ^ depth * odd` falls in the first `1/n` of the
range, so `every 200` takes about one node in two hundred and takes the same
nodes whatever order the search reached them in. A counter over the stream
picks nodes by when they were visited, and a change to the tree then moves
the membership in ways that read as a shift in the distribution. Nothing is
drawn: two runs of the same command still print the same rows.

The header states `events`, every node the shortcuts answered, beside
`records`, the ones the run kept. That is the denominator, and a crossing
rate cannot be read without it: a zero over four hundred records is not the
same statement as a zero over four hundred thousand events, and at a rate
low enough to finish in minutes the two counts run three orders apart. A
rate of zero at a low sampling rate is not a rate of zero. Raise the rate,
or read the events count and say how small a rate the run could have seen.

The buffer holds ten thousand samples unless `cap <n>` asks for another,
and the header says when it had to drop some. A cap off the default is
stated there too, so a run can be rerun from what it printed. Past the cap
it keeps the smallest keys rather than the first arrivals, which is a
uniform draw from the whole run
and is the same draw whichever order the run met the nodes in. That holds
for the set of keys and not for what sits behind a repeated one: a deepening
search revisits a position at a depth under a kind, so keys tie, and the
samples behind a tie differ in their beta and their window because they are
the node's first answer and its second. Which member of a tied group
survives the cap is whichever the heap surfaces, so a run offering the same
events in another order can keep the other member.

`epd <file>` searches a suite of its own instead of the bench's, in the
format `bench.epd` is written in, and the header names the file the way it
names a cap off the default. That is what lets a threshold be chosen on one
set of positions and read back on another: a margin fitted on the bench's
eighteen and then reported as an improvement to the bench is circular, and
a held-out file is the answer. `arche-core/tactics.epd` is three hundred
positions the bench does not hold. A file that will not open, holds no
position, or holds one the board will not take is refused rather than
searched. The reduction ledger takes the same word, for the same reason;
the cutoff census does not, because a census describes a tree rather than
choosing a number off it.

The kinds share the one buffer. Keys are uniform whatever the kind, so past
the cap each kind keeps a share in proportion to its volume, and the shadow
kind's volume is the largest by construction; a calibration run that wants
its live strata whole raises the cap rather than reasoning from a crowded
one. The events count in the header counts offers, so a fired reverse
futility node contributes twice, once live and once shadow.

The recording and the replay never overlap. A reference search run inside the
measured one would store reference entries in the table the measured search
is reading and change the play being measured, so the replay waits until the
suite is finished and runs on an engine and a table of its own. That table is
cleared before every sample, so no sample's reference answer is read from
another sample's entries.

The known limitation is in the fen. It carries the fifty move counter and not
the path, so the replay cannot see a repetition that needs moves made before
the node, and the reference value is the reference's answer to the position
as a diagram. That is the simplification every epd suite makes, and it means
a residual from a position deep in a shuffle is read with more care than one
from a middlegame. The taint policy differs across the two phases as well:
the recording runs whatever the command was given, `rule50` by default,
while the reference refuses every tainted cutoff, so a crossing on a row with
a high halfmove count can be the two policies disagreeing about a draw rather
than the shortcut being wrong.

Rows from before the hash sampling landed do not compare with rows from
after it. The two runs sample different nodes, so a distribution from one is
not a baseline for the other; rerun the command rather than reading an old
report next to a new one.

The command is an argument and not a uci command. Like the bench it is a
measurement rather than a move, and unlike the bench nothing about a live
session wants it: it searches the suite twice over and takes minutes at the
depths worth running it at.

## Which move cuts a node off

The bench says how much of the tree the move ordering saves. It cannot say
which moves are doing the saving, and that is what `arche cutoffs` records:

```
target/release/arche cutoffs [depth] [every <n>] [cap <n>]
```

It searches the same suite the bench does, under the default configuration,
and samples the full width nodes as they answer: one event per node, taken
at the two places a node returns out of the move loop, the cutoff and the
loop running out. Both outcomes are recorded at the same rate on purpose.
A cutoff censors every move ordered after it, which is what makes a raw
history count biased (a move ordered early gets chances a move ordered late
never does), and a stream of cut nodes alone would reproduce exactly the
censoring the census exists to measure. Quiescence and the root are out of
scope: quiescence cuts on capture order, and the root searches every move.

Each event is a row of `depth window check outcome generated searched index
class history history_max scored tt eval_beta cost reduced fen`, whitespace
separated with the fen last so a row parses left to right. A `cut` row
names the cutting move's place among the searched moves (`index`, 0 for a
table move searched first), its `class` (`table`, `capture`, `promotion`,
`killer` or `quiet`, material first, so a capture that is also a killer is
a capture), the history table's score for it (`history`, quiet moves only),
and whether its answer came through the reduced scout (`reduced`); a `held`
row prints `-` in those four columns rather than moving the others.
`generated` and `searched` are what the list held and what the loop made:
the legal count is unknowable without making every move, so the censored
count is the reader's subtraction, and a node its table move cut before
anything was generated says `generated 0`. `history_max` is the largest
history score among the generated quiets, the denominator `history` is read
against, since the raw number ages. `scored` says whether the staged
ordering ever scored the quiet band at this node, or the front answered
first. `tt` is what the probe gave the node: `miss`, `move`, or
`score_only` for a hit whose move was not playable here. `eval_beta` is the
static evaluation less beta, computed at record time for kept events alone;
the column is exact rather than a cache read, and evaluating only the
sampled nodes is what keeps an eval away from the nodes the measured search
never evaluated. `cost` is the nodes spent under the node, the counter at
its answer less the counter at its entry.

The sampling is the residuals command's mechanism with a salt of its own: a
hash gate over the position key and the depth at an `every <n>` rate, in
front of a capped reservoir that keeps the smallest keys of the whole run.
Two runs print the same rows, and a change that reorders the tree without
changing what is in it samples the same nodes. The header states `events`
beside `records` for the residuals header's reason: the rows are a share of
the events, and a share cannot be read without its denominator. Every full
width node the move loop answers is an event, so the stream runs far denser
than the shortcut sampler's, and the deepest nodes are the rarest in it; a
run that wants them well sampled lowers `every` or raises `cap` rather than
reasoning from a handful of rows.

The run ends with a line per depth: the records, the cut rate, the share of
cuts at index 0, at 1 to 3 and past 3, the mean moves searched at cut nodes
and at held nodes, the class shares, and the share of cut nodes whose quiet
band was never scored. The rows are the product; the summary is a sanity
read.

Recording changes nothing. The census is armed only by the command, an
engine without one searches exactly the tree it searched before there was
a census at all, and an armed engine's node counts equal a disarmed one's
position by position, which
`recording_leaves_the_measured_search_where_it_was` in
`arche-core/src/census.rs` asserts of the armed engines themselves. The
pinned node counts cover the disarmed default and the reference.

## What trusting a reduced scout costs

The census says which moves cut. It cannot say what the late move reduction
buried, and that is what `arche reductions` measures:

```
target/release/arche reductions [depth] [every <n>] [cap <n>] [epd <file>]
```

It searches the same suite the bench does, under the default configuration,
and samples the reduced scouts as they answer: one event per sampled scout,
taken where the scout's answer comes back. A scout that fails low is
trusted, and the move it answered for is never searched at the depth the
node has. A scout that fails high has earned the full depth, so its cost is
the scout it wasted rather than a wrong answer, and it is never replayed;
the fail highs stay in the stream at the same rate all the same, because
they are the denominator a reduction policy's propensities are read
against.

The label on a fail low comes from a replay, run after the suite on the
residuals replay's terms exactly: the reference, with a table of its own
cleared before every sample and no clock. The fen on a row is the position
the reduced move left, so its side to move is the side the move was played
against. The replay searches it to the node's depth less one, which is the
depth the move was denied, over the full window, and the answer is negated
to the reducing node's side before it is read. Strictly above the alpha
the scout was read against is `harmful`: the full search would have raised
alpha on a move the scout wrote off. Anything else is `harmless`. The fen
carries the fifty move counter and not the path, with everything the
residuals section says that costs.

Each event is a row of `depth window index searched generated history
history_max killer tt eval_beta alpha_gap alpha scout cost reference label
reduction fen`, whitespace separated with the fen last so a row parses left
to right. `depth` is the reducing node's, its check extension included.
`index`, `searched`, `generated`, `history` and `history_max` are the
census's columns, read at the decision; `killer` says whether the move
stood in a killer slot, and `tt` is the census's three-state. Every
reduced move is quiet, so `history` is never priced by a class instead.
`eval_beta` and `alpha_gap` are the node's own static evaluation against
its two bounds, computed at record time for kept events alone by stepping
the move back and replaying it, for the census's reason: an eval forced at
every scout to fill a column is not the engine being measured. `alpha` is
the bound the scout was asked about, `scout` is `low`, `high` or
`skipped`, and `cost` is the nodes the scout spent. A fail high prints
`-` in the `reference` and `label` columns rather than moving the others.

The third outcome word is late move pruning's. A move the model prices
in its deadest band is never scouted at all, so a sampled skip is
recorded where the loop passes it over: the same features, a cost of
zero, a reduction of zero, and a `searched` count that equals the index
rather than standing one past it, because the move is not among the
searched. The replay treats a skipped row as it treats a fail low,
since what was denied is the same full depth search. The search never
makes a skipped move, so its legality is unknown at the decision;
the recorder makes and unmakes it around the record alone, and a move
that turns out illegal is not recorded, because the skip denied it
nothing.

The sampling is the census's, salt and header and all. What differs is
the density. Only a late quiet move at a node deep enough to reduce
offers a scout, so the stream runs sparser than the census's rather than
denser, and every fail low kept costs a reference search in the replay.
A run that wants one stratum whole lowers `every` and pays for it in
replays.

`epd <file>` searches a suite of its own instead of the bench's, on the
residuals argument's terms exactly, and the header names the file the way
it names a cap off the default. A threshold chosen off these rows and then
reported as an improvement to the same positions has checked nothing, and
the ledger's own thresholds are the case in point: the attention model was
fitted on a bench run, so a candidate for either of its operating points is
read on `arche-core/tactics.epd` or `arche-core/strategy.epd` rather than
on the eighteen. A file that will not open, holds no position, or holds one
the board will not take is refused rather than searched.

The run ends with a line per depth: the scouts (the skipped rows are
counted apart, so the fail low share keeps its denominator), the skipped
count, the fail low share, the replayed count, the harmful count and
rate, and the harmful rate split by
index band (4 to 7, 8 to 15, 16 and past) and by history fraction (zero,
under a tenth, under half, half and up), which are the cells a reduction
policy would be fit on. A cell under thirty replayed rows prints its
counts in place of a rate, and the line's own rate holds to the same rule:
a percentage over a handful of rows reads as a finding and is noise.

Recording changes nothing, on the census's terms and held to them the
same way: `recording_leaves_the_measured_search_where_it_was` in
`arche-core/src/reduction.rs` searches each of its positions twice, once
with the ledger armed and once without, and holds the two counts equal.

## What a position's evaluation is made of

The three instruments above measure the search. This one measures the
evaluation, and it is the engine's half of the tuner:

```
target/release/arche terms [epd <file>]
```

It searches nothing to a depth, so it takes no depth. It reads the bench's
suite, or the one named, keeps the positions that are quiet, and prints what
each one's evaluation is made of.

The evaluation is material plus a tapered piece square score, and it is
linear in the numbers those are read from. So a position's score is a dot
product of the position against the weights, and a row is the position's half
of it: for every weight the position touches, the integer that weight is
multiplied by. The weights are a flat vector of 774, in this order: the 384
midgame table entries, the 384 endgame ones in the same order, then the six
material values. So a square's two weights are 384 apart. A slot's entry is a
square as black sees it, because black is the colour that reads the tables as
they are written.

The vector was 518 until a knight, a bishop, a rook and a queen were given an
endgame table of their own, since each of the four had handed one array to
both ends of the taper. Rows printed by an engine from before that, and any
vector fitted against them, are refused rather than read: every slot they name
exists in the layout that replaced them, so reading them would put the numbers
on the wrong weights.

The line after the header is `weights 774 <w0> <w1> ...`, the vector itself as
the live tables hold it, so that nothing reading these rows transcribes
psqt.rs. A transcription is the same failure as a reimplemented evaluation and
quieter: a table copied out and left behind fits weights against a position it
scores differently from the engine, and nothing says so.

Each row after that is `id eval phase n slot:coefficient... fen`, whitespace
separated. Both ends of it can hold spaces: a fen is six fields, and an id is
whatever the epd put in the quotes, which in the bench's own suite is "ruy
lopez" and in the strategic suite "7th Rank.001". An epd line that names no id
is called by its own fen, so an id can be six fields itself. So a row is read
from the end whose width is fixed. The fen is the last six fields, the
coefficients are the run of `slot:coefficient` in front of it, and what is left
before the three numbers is the id. `n` is printed so the two ends can be held
against each other rather than one of them trusted. The coefficients are in
the side to move's frame, so the row's own arithmetic is the evaluation with
nothing further to do:

```
eval = mat . w_mat + trunc((psqt . w_psqt) / 24)
```

Three things in that line are load bearing, and each is a way to be wrong by a
centipawn. The divide truncates toward zero, where python's `//` floors, and
on a negative numerator that does not divide evenly the two differ. The
material is added outside the divide rather than scaled into it:
`trunc((24 * 1 + -5) / 24)` is 0 where `1 + trunc(-5 / 24)` is 1. And the
phase is capped at 24 before the coefficients are written, because promotions
can leave more on the board than the opening had. Each has a test of its own
in `arche-core/src/tune.rs`.

Nothing outside the engine is told how to evaluate a position, which is why
the argument exists at all. A second implementation of the evaluation in
another language diverges quietly: one wrong by a little still produces
plausible weights, and nothing says when. So `reconstruct` folds a row back
against the live tables and has to give what `eval` gives, exactly.
`a_positions_terms_reconstruct_its_evaluation` asks that over the shared fens,
the bench's suite and the strategic suite, which is 1522 positions of three
different shapes; the run asserts it on every row it prints and panics rather
than dropping one, so a corpus cannot hold a row the engine disagrees with.

A position is kept when three things hold. The side to move is not in check,
since a checked position's static evaluation is not a thing to fit and
quiescence treats it differently anyway. A capture search comes back at the
static evaluation, so the side to move has nothing to win by capturing. And
the same holds after a pass, so the opponent has nothing to win either. The
pass is what makes the test two sided: a one sided test keeps the position
where the side to move is about to lose a hanging queen, and labels an
evaluation that misses it with the result of a game that did not.

The capture search is the reference's, with every shortcut off, for the reason
the residual replay uses the reference. The default's quiescence has the delta
margin and the losing capture skip on, so it passes over captures it prices as
hopeless, and those skips are guesses. A corpus whose quietness was decided by
a guess would carry the guess into every weight fitted on it, and would move
when the guess moved.

The header states what the run turned away beside what it kept:

```
terms positions 18 in_check 1 unsettled 8 kept 9
```

which is the recorders' rule that a share cannot be read without its
denominator. If the yield ever leaves too few positions to fit 768 weights,
dropping the pass is the fallback, and the header is what makes that a
decision rather than a discovery.

`epd <file>` reads a suite of its own instead of the bench's, on the residuals
argument's terms, and the header names the file. A file that will not open,
holds no position, or holds one the board will not take is refused rather than
read.

The argument arms no reservoir and searches no suite, so there is no
recording-neutrality claim to make: it cannot move a node count, because
nothing about it runs inside a measured search.

## The loss of a weight vector

The rows above are the input to `scripts/tune.py`, which scores a weight
vector against the games the positions came from and fits a new one:

```
python3 scripts/tune.py loss --terms rows.txt --corpus corpus.epd
python3 scripts/tune.py fit --terms rows.txt --corpus corpus.epd --out fit.json
```

`scripts/build_corpus.py` builds the corpus from archived strength-run pgns,
carrying each position's game, the run and the round that game was played in,
the result from the side to move's point of view, and how many times the games
reached it:

```
python3 scripts/build_corpus.py runs/*/games.pgn --out corpus.epd
```

The book is dropped off the front of every game, so the corpus starts where the
book stops, and a game that ended in anything but play is dropped whole. The
known caveat is that these are the engine's own games, so the positions it
never reaches are unlabelled and its mistakes are labelled as if they were
normal play. Mixing in positions from stronger engines' games would answer a
different question, and a loss change measured on a corpus of two sources
cannot be attributed to either.

The objective is occurrence weighted. A unique position carries the weight of
how many times the corpus reached it, so the loss is over the distribution the
engine runs on rather than the one deduplication leaves behind. Every loss,
interval and share the run prints reads the count, and the header names a phase
bucket's positions and its appearances separately, so which of the two a figure
was taken over is on the page rather than assumed.

The split is by game, in three groups, and the two games that played one
opening with the colours reversed go together. A game is named by the sha256
of its movetext and a pair by the sha256 of its two games' keys sorted and
joined, and the first byte of the pair's key modulo five says where the pair
goes: nought, one and two train, three is the selection group and four is
calibration. A game with no partner is a pair of one and its pair key is its
own. Both keys are the movetext's and nothing else's, so a re-extraction of the
same archive puts every game back where it was and nothing has to be written
down outside the pgn. Split by the game alone, the two halves of an opening
land in one group eleven times in twenty five, which is the chance two keys
agree under shares of three fifths, a fifth and a fifth.

The game is the unit because the label is: every position of a game carries
that game's result, and consecutive positions are one move apart, so a row held
out while its neighbours are trained on is a row whose answer the fit has
already been shown. Splitting on the position instead hides that rather than
preventing it.

Grouping by the game loses one property a split keyed on the position has for
free, which is that rows sharing a position land together, so
`build_corpus.py` gives it back: a position two games reached belongs to the
group of the lower key, and its result and its count are taken from that
group's games alone. The appearances in other groups are dropped rather than
merged. A corpus that repeats a position across games anyway is refused rather
than fitted around.

The ridge is chosen on the selection group and the loss is reported there. The
calibration group is not read at all. What it is for is a coverage claim made
on it once the weights are final, and a group that has already been read cannot
carry one, so it is assigned from the first run rather than carved out when it
is wanted. That is not a convention a reader has to keep in mind: the
calibration rows are not in the matrices `loss`, `cv` and `fit` score, so none
of the three can reach a calibration row. Deleting them from the corpus file
changes nothing any of the three prints, and
`the_calibration_group_is_not_read_by_a_fit` says so by running the same fit
twice.

The seal holds on the rows and on the labels. No command reads a sealed row,
and no sealed game's result reaches a label a fit sees, because a position is
labelled by its own group and by nothing else. What that costs is the
appearances in the other groups: they are dropped, so a position common enough
to be reached by games in two groups carries fewer appearances than the corpus
gave it. No position loses every appearance, since the group that owns it is
the group of a game that reached it.

The run is read off the `manifest.txt` a strength run keeps beside its
`games.pgn`, as the run id and the shard, and off the directory's name where
there is no manifest; the round is the pgn's own `Round` header. Neither is
part of the key, because neither is a property of the play. They are on the
row so that a source can be excluded or weighted after extraction, and so that
the two games that played one opening with the colours reversed can be told
apart from two openings.

The one door into the sealed group is `final`:

```
python3 scripts/tune.py final --terms rows.txt --corpus corpus.epd --weights fit.json --log final.log
```

It takes a vector already quantized to the integers that would ship, refuses
one that is not, and before it reads a sealed row it appends a line to the log
naming the corpus, the sealed games, the extraction and the vector by checksum,
the scaling constant and the group's size. A corpus the log names is refused,
and so is one whose sealed games the log names under another corpus: the
checksums are over the file and over the sealed pair keys, so neither a new
filename nor a re-extraction with a run appended reopens the same games. What
it prints is the frozen vector against the shipped one on the sealed rows, both
losses at real and at integer weights, the paired difference with its interval
clustered on the game, the loss by phase bucket, and the residual quantiles,
signed and absolute, weighted by appearances. The quantiles are an empirical
diagnostic and the line says so. A coverage claim needs its sampling unit,
score, exchangeability, quantile rule and target named before the group is
opened, which no command can do for the person making it. A vector revised
after the reading needs a sealed group of games the corpus did not hold.

`build_corpus.py`'s counters open with `runs`, the archives the games came
from, carry `pairs` and `unpaired`, the rounds that made a pair of more than
one game and the games that stood alone, and end with `repeated`, the
positions more than one game reached, `straddled` and `dropped_appearances`, which are how many of
those were reached from more than one group and how many appearances that cost,
and `same_key`, the games whose movetext another game already had. The last is
the only place a game the archive holds twice shows up: it is one game's
evidence counted twice, and every other number in the run reads it as two.

The loss is Texel's, the mean squared error between the game result and a
logistic of the evaluation, with log loss printed beside it. The two are
different scoring rules and if they disagree about a candidate that is worth
seeing, which is the only reason both are printed. The scaling constant K is
fitted once on the training games at the shipped weights and held there,
because K and the overall scale of the weights are one degree of freedom and
the scale is not free: `REVERSE_FUTILITY_MARGIN` at 100 a ply, `DELTA_MARGIN`
at 200 and the ledger's `eval_beta` column all read the evaluation on the
assumption that a pawn is about a hundred.

Nothing in the script knows how to evaluate a position. It folds each row's
coefficients back against the weights the run printed and has to get the
integer the row says the engine got, which it checks on every row read, so a
file it cannot rebuild stops the run rather than being fitted around.

What makes the number worth having is that scoring a weight vector over
hundreds of thousands of positions is one matrix-vector product. A hundred
thousand rows are read, joined and scored in a couple of seconds, and a
candidate evaluation term would be one appended column whose held-out loss can
be read before a line of engine code exists for it. That is a triage instrument
and not a verdict.

A meaningful move in the number is one larger than its own interval. Two weight
vectors are scored on the same selection positions, so the difference in
per-position squared error is a paired sample; the run prints its mean with a
standard error and marks a difference that sits inside its own interval. There
is no loss-to-elo mapping here and the run does not print one. Its job is to
rank candidates and to reject the ones that cannot help. The sprt says elo.

The interval is taken over the games and not over the positions. A game's
hundred odd rows share a result and differ by a move, so they move together,
and counting them as a hundred independent draws counts one game's evidence a
hundred times. Both figures are printed: the standard error over the games, the
naive one over the positions, and the design factor between them, so what
treating the positions as independent would have claimed is on the page rather
than described.

Comparing two ways of fitting is `cv`:

```
python3 scripts/tune.py cv --terms rows.txt --corpus corpus.epd
```

Five folds assigned by the second byte of the game key, each refitting on four
fifths of the games and scored on the fifth, so every row is scored once and by
a fit that never read its game. K is fitted per fold on that fold's training
games. The second byte and not the first, because the first is what put the
game in its group and folding on it would leave two folds empty. What it folds
is the training and selection games, and the sealed group is not among them
because it is not in the corpus `cv` is handed. `fit` does not choose its ridge
here: the selection group is what a fifth of the games was set aside for, and
the wider question this answers is whether a way of fitting is worth anything
at all over the games the run may read. The line naming the best penalty says
which of the two it is best over, so a reader of a `cv` log cannot paste it
into `fit --penalties` as the ridge the fit would have picked.

Three more figures are printed because a loss on its own hides what a fit
did. The loss is stratified by the three phase buckets, so a fit that improves
the endings by hurting the middlegame is visible rather than averaged away. The
per-slot support counts say how many rows each weight is fitted on, so a weight
the corpus barely constrains says so before it ships. And each vector's table
scale is printed beside a K refitted for that vector alone, which is the
diagnostic for the one thing the fit can do that is not an improvement: with K
held, a fit given enough licence spends the loss on growing the tables rather
than on their shape, and a vector whose loss only falls at its own K bought
scale.

The scale is a ratio of root mean squares over the table half of the vector,
so it means one thing inside a layout and nothing across two. That half held
512 entries before a knight, a bishop, a rook and a queen were given an
endgame table and holds 768 after, and 256 of the 768 were exact copies of
their midgame twins until the fit that made them differ. A scale of 1.0 at
774 slots and a scale of 1.0 at 518 are not the same statement, and the same
goes for the boardful the bound is checked against. Figures from fits at
different layouts are quoted with the layout beside them or not quoted
together.

The fit itself is ridge toward the shipped weights rather than toward zero.
That makes a re-tune literally what it does; it leaves alone the one direction
the corpus cannot see, which is a constant added to both king tables and
cancelling between the colours; and it holds the slots with no support at all,
the two back ranks of the pawn tables, at exactly the zeroes they already are.
The ridge strength is chosen from a grid on the selection games, each penalty
fitted on the training games alone, and a fit whose tables have grown past what
the packed halves can carry is refused whatever it scores. What that costs is
that the selection loss printed beside the chosen vector is the fit's own best
case, since it is the number the grid was ranked on. The sealed group is where
an honest interval on a final vector comes from, which is what it is being kept
for. `MATERIAL` is held for a first fit, because
`eval::material` is read by the delta margin in quiescence, so moving it
changes which captures quiescence skips, which changes the tree for a reason
that has nothing to do with the evaluation's accuracy. `--free-material` lets
it move, for the run that reports what holding it cost.

## What the table's key signature costs

An entry keeps thirty two bits of the position key rather than all sixty
four, so two positions can agree on the bits the table compares and the
search reads one of them as the other. The entry's comment puts that at about
one probe in a thousand million. `audit` replaces the guess with a count:

```
target/release/arche bench 7 hash 1 audit
```

It keeps the full key of every entry beside the entries and prints two lines
of whole-suite totals after the table:

```
signature audit: probes 1173888, hits 241830, comparisons 2141471, false accepts 0 (0.000 expected), false accept cutoffs 0, aliased evictions 0
narrow signature: 16 bit accepts 36 (32.676 expected), 24 bit accepts 0 (0.127 expected), 28 bit accepts 0 (0.007 expected)
```

The word turns the shadow keys on for the tables the bench builds and for
nothing else, so a session that plays games never allocates them. Detection
changes nothing: a probe that was a false accept hands back what it would
have handed back unaudited. An audited table starts empty, because an entry
stored before the audit began has no key on the side and every probe would
read it as a stranger's.

The two lines have two denominators. Probes, hits, comparisons and false
accepts count keyed lookups; aliased evictions count stores. A comparison is
one live entry a probe compared its slice against, whose full key turned out
to belong to another position. Only the entries that probe really looked at
are counted, so each comparison is one chance in two to the signature's width
and every expectation on either line is drawn from the total.

An aliased eviction is a store that landed in a slot the slice said was its
own and replaced another position's entry there. Landed stores only. A store
the depth contest turns away after comparing itself against a foreign entry's
depth is a related cost, since it compared against the wrong position and its
own result went unstored, but nothing was evicted and it is not in the
figure.

The thirty two bit observation cannot say anything on its own. A run of this
size expects half a thousandth of a false accept, so a zero is what a
working instrument and a dead one both print. That is what the narrow line is
for. It counts the comparisons a narrower signature would have accepted and
this one refused, at sixteen bits, twenty four and twenty eight. Sixteen is
the same rate scaled by sixty five thousand, so the figure is a few dozen
instead of about zero, and a count sitting on its expectation says the rate
really does scale by two to the minus the width on this workload. The thirty
two bit expectation beside it can then be believed where its observation
cannot.

Twenty four and twenty eight are the widths the signature would be left with
if four or eight of its bits went to some other piece of metadata, which is
the question they answer: what reclaiming those bits would cost the table.
Neither is measurable at the sizes the command runs at. At the bench's depth
twenty four expects an eighth of an accept and twenty eight a hundred and
fiftieth, and at depth eight twenty four expects about three quarters. A zero on either is
the run being too short to have an opinion rather than a bit budget with room
in it, so what the reader takes from those two is the expectation and not the
count. The count bounds it, and nothing at these sizes can do more.

The widths are cumulative by construction. An entry whose low twenty four
bits agree agrees on sixteen as well, and is counted under both, so each
figure is read against its own expectation and the three are never added
together. The counts therefore fall as the width rises, which
`the_counts_fall_as_the_width_rises` in `arche-core/src/transposition.rs`
checks on a probe sequence built to make them fall.

The narrow figures are counted and never acted on: a search really running
one of these widths would have stopped its scan at the first entry it
accepted, which is a different tree.

Read the narrow line off a small table. A table of one or two megabytes at
the bench's depth is full by the end of the suite, so its buckets hold four
live entries and its comparisons run to millions; at the default sixteen the
suite fills under a tenth of the table, the comparisons are an order down and
the narrow count is small enough to be noise. The command does not sweep
sizes itself.

The audit costs eight bytes an entry, half the table's own size again, and
refuses to run rather than run unaudited if there is not the memory for them.
An audited run and a plain one search the same tree, which the node counts
say.
