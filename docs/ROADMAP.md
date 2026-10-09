# Roadmap

What the engine does not do yet, and what it does badly enough to be worth writing down.
See [DEVELOPMENT.md](DEVELOPMENT.md) for how to measure whether one of these helped, and
[INSTRUMENTS.md](INSTRUMENTS.md) for the commands the entries below quote.

## Not implemented yet

Each of these arrives with a `Bench:` trailer, and with an `Elo:` trailer from
an SPRT when it changes how the engine plays. Roughly in the order they look
worth doing.

- the rest of the late move reductions. How far a late quiet is scouted back is read
  off a table by the node's depth and the move's index, which took +19 ±11 over 2,000
  games at 10+0.1. Reducing the losing captures and reading the history table for the
  eligibility are what remain, each measured on its own
- draw knowledge in the evaluation. The material signatures that cannot mate
  read zero. What remains is a scale factor on the endgame half for the near
  drawn endings that rule does not catch: opposite coloured bishops with pawns,
  and a pawnless minor piece advantage, neither of which the signatures reach
- the rest of evaluation: the rest of king safety, the rest of pawn structure, and special
  cases such as the bishop pair and open files. Mobility has been fitted three times. The
  first fit read 1,812 games and rounded six of the eight weights to zero; the second read
  twenty four times as many games and left no weight at zero; the third moved it with
  every other weight but material, on 59,049 games in September 2026. The king shelter
  follows the storm three ranks and no further, and a storm pawn blocked by one of ours
  counts the same as a free one. The king attack zone is priced per square, where an
  attack by several pieces is usually taken as worth more than the sum of its parts, and
  reads neither safe checks nor the ring's defenders. Its first fit (d3dcc53) trained on
  32,516 of the archive's 50,677 games with every other term held, and it was refitted
  with every other weight but material on 59,049 in September 2026; the first fit's sealed
  group read -0.000143 against a standard error of 0.000059 and the selection group
  -0.000395 against 0.000052, about 3.2 standard errors apart, which by the rule the fit
  was registered with means the selection group overstated it. On the sealed games the
  boards with thirteen or more pieces left read slightly worse. In the pawn structure the
  seventh rank is the one to distrust: a passed pawn there prices below one on the sixth
  at both ends of the taper at every ridge on the grid, likely from the quiet filter,
  since a position with a passer one square from queening is rarely settled unless the
  pawn is blockaded or falling. Thirteen of the strategic suite's fifteen themes rose when
  that term was fitted; AKPC fell by 112 and 7th Rank by 217. The term leaves out
  everything that reads a square rather than a file: whether the square in front of a
  passer is occupied or attacked, how far each king stands from the promotion square,
  candidate, connected and backward pawns, pawn islands, and the rule of the square. The
  first two are the valuable ones, and neither can sit behind a key over the pawns. The
  tuner is built ([INSTRUMENTS.md](INSTRUMENTS.md)), so a candidate term is one appended
  column whose held-out loss can be read before there is engine code for it. Fitting on
  our own games does buy strength: four fits each passed an sprt bounded [0, 10] at
  10+0.1, the twelve tables at +54 ±13 over 2,000 games, the first mobility fit at +12 ±8
  over 4,000, the mobility refit at +76 ±25 over 500 and the pawn structure weights at
  +57 ±25 over 500, each against the baseline its own `Elo:` trailer names. The joint
  refit of every weight but material passed the same test at +36 ±16 over 1,000 games as
  5d34488 against ec3d213, on a search older than the one it landed on, so 8027c08 carries
  no trailer. A held-out loss cannot choose between two fits of one term: it favoured the
  first mobility fit while covering zero, and the games ranked the two
- the rest of the uci protocol
  - the only options advertised are `Hash`, the `Clear Hash` button, a `Threads` fixed at
    one, `Move Overhead` and `Debug Log File`, so everything else an interface might set,
    `Ponder` among them, is refused rather than acted on
  - `ponderhit`, `debug` and `register` are not handled, so pondering is still out of reach
    even though `stop` is answered now
- read an opening book in the engine, only the lichess-bot image has one at the moment and it is
  lichess-bot that reads it rather than the engine
- winboard

## Known limitations

- the strategic suite's total cannot be read against zero. It discriminates between
  vectors of the same size and not between a term and its absence; the figures are in
  [DEVELOPMENT.md](DEVELOPMENT.md). Three themes lose under any shelter term at all:
  Recapturing takes 90.7% of its points already and can only regress, and Square Vacancy
  and Advancement of a/b/c pawns lose under every arbitrary vector tried
- the fitted term makes the engine keep the pawns in front of its own king at home, and
  one graded theme says that is wrong. The first fit's midgame cover weights were +10 and
  +21, and they did what they said: over the strategic suite the engine advanced a pawn on
  its king's file or a neighbour 100 times where it advanced 123 before, and 167 times
  with the weights negated. AKPC grades such a push as the best move in 79 of its 100
  positions, and the engine played one in 22 of them against 31 before. The joint refit
  puts the weights at +19 and +19, and none of these counts has been read again since.
  That is the one place the suite and the term disagree about chess rather than about
  noise, and the games that judged the term were played on its first fit and again, moved
  with every other weight, in the joint refit's sprt, so what is unresolved is whether
  declining those pushes is right in positions the games under-sample rather than whether
  it costs elo overall
- the king safety weights are fitted on a corpus that is mostly not the middlegame the
  term is about. 66.4% of the 2026-09-12 corpus's appearances have six or fewer pieces
  left on the board and 6.0% have thirteen or more of the fourteen, so the midgame half
  of the taper, which is the half king safety is for, rests on the smallest of the three
  phase buckets. It shows in the answer: two of the storm's three weights came out
  positive, so an enemy pawn one rank in front of a king scores in favour of the side it
  stands in front of. That count is also the thinnest supported in the corpus, carrying a
  coefficient in 4.65% of the rows against 47.83% for the near cover, because a king
  usually takes such a pawn and the position is then not quiet. The held-out loss puts the
  fourteen at 5.6 standard errors better than zero. Whether the term measures king safety
  is a separate question, and a corpus with middlegames in it is what would answer it
- the sealed fifth of the 2026-09-12 corpus is spent. It was opened once, on 2026-09-12
  after the games had accepted the king safety vector, and read -0.000537 against a
  standard error of 0.000118 over its 5,772 games, which is 0.68 standard errors from the
  selection group's -0.000649. The next fit wanting an honest held-out interval wants
  games this corpus never saw. The access log is not in this repository
- an evaluation term is allowed 5% of the search. Mobility and the king attack zone share
  one walk, so the rule has two readings. Under callgrind over the full `arche bench` at
  8f9dafe, with the walk out of line and a walk of each term alone beside it, the shared
  walk is about 10.6% of the tree search. Taking mobility out and keeping the ring would
  save 4.36%, and taking the ring out 3.39%, so each is under the 5% as a marginal, and
  the remaining 2.9% is charged to neither. A walk of mobility alone is about 7.3%, over
  it. The 13.5% once recorded here was mobility's own walk at `378c148` on `bench 5` and
  does not compare. The 5% is a rule of thumb that nothing enforces, and games at this
  scale do not check it: lazy mobility took 6.05% off the search to a fixed depth and
  none of its three margins resolved a gain in 6,000 games (below). The pair term costs
  4.4% of the bench's nodes a second, under the 5%, and 7.6% of the nodes a second in
  the games that measured it at +95 ±19 (6ce33d3), over it
- on 1,812 games a held-out loss could not resolve a fit of the piece square tables one
  way or the other, so an sprt decided the re-tune. Measured 2026-09-10 over those games,
  100,726 quiet positions across 1,807 of them: the weights then shipped score 0.093561 on
  the selection games and the fit psqt.rs held until the joint refit beats them by 0.000618
  against a standard error of 0.000627, which is inside its own interval. The sealed third
  of the games, opened once after the vector was frozen, reads the same fit 0.001754 better
  against 0.000674, which is outside it. The two readings differ by 1.23 standard errors,
  so they are one corpus disagreeing with itself rather than two findings, and the games
  settled it at +54 ±13 over 2,000 at 10+0.1. The corpus is the engine's own play, so the
  positions it never reaches are unlabelled, and that is the ceiling on what any fit of it
  can say. The 2026-09-12 corpus holds sixteen times the games and did resolve a fit, at
  5.6 standard errors, but of fourteen weights rather than 768, so it says the corpus was
  small for that question as well as the question hard. The tables were refitted jointly
  with every other weight but material on 59,049 games in September 2026, which five fold
  cross validation with whole pairs held out reads 0.000832 better than the weights they
  replaced, against a standard error of 0.000040
- the harness said otherwise until 2026-09-10, and why is worth keeping. It split the
  corpus on the fen, and 1,805 of the corpus's 1,809 games had rows on both sides: 53.1% of
  the held-out rows had the position a ply away, from the same game and carrying the same
  label, sitting in the training set. Read that way a fit of the same tables came to
  -0.005831 at 27.8 standard errors, which is what the leak was worth. Any number quoted
  from a tuner run made before that date is a number on a split that held nothing out
- a transposition score that came from a repetition or fifty move draw is trusted, except
  within four plies of the fifty move horizon, where every cutoff is refused. The search can
  therefore read a draw down a path that could not reach it; the policies were played against
  each other and trusting such scores won, at +48 ±23 over 308 games at 5+0.05, so the
  error is carried knowingly, measured by the graph history counters, and the refusing search
  remains as the reference. `taint refuse` restores the refusal alone, on top of whatever
  else the default does; the full reference has no command line spelling
- a fen is only validated as far as what the search cannot survive, a king a side and the side
  not to move being out of check. A position which is illegal in other ways, such as one with
  nine pawns or a pawn on the back rank, is accepted and played from. The castle rights and
  the en passant square are the exception, because the generator reads those fields rather
  than the pieces and make_move would corrupt the board playing what they license. A right
  whose king or rook is not standing on its square is dropped, and so is a square that is not
  on the rank a double push crosses, is occupied, has no pawn placed to take there, or has no
  enemy pawn behind it
- nothing validates the twenty `unsafe` operations, thirteen in `board.rs`, four in
  `transposition.rs` (the `madvise`, the bucket's SSE load and the two unchecked bucket
  reads), and three blocks of SSE code in `ordering.rs`. `unsafe_op_in_unsafe_fn` is denied
  in `arche-core/Cargo.toml`, so every one of them sits in a block carrying a `SAFETY`
  note, and the `arche` crate forbids unsafe outright. That is the half a compiler can
  check. The other half is Miri, which needs nightly. Each of the generator's six sites,
  and the static exchange gain array's `assume_init`, are ones where a slip is undefined
  behaviour rather than a wrong answer: the captures written through a cursor into the move
  list before `set_len`, and the quiet moves written unchecked into a buffer that holds
  only because no side has more than 63 pieces besides its king, of at most 27 quiet moves
  each. So are the position stack's four: reading a slot, shared and mutable, that no parse
  or make has written (a detached board cannot take a move back past the one slot it
  writes); the copy of one slot into another, whose indices must fall inside the stack; and
  the detached board, which writes one slot of an uninitialised board. The ordering's SSE
  blocks are too: a load or a store past the keys, the moves or the history table reads or
  writes memory the array does not own, and so is the table's: an index past the length
  reads past its end, and the aligned load faults on an address that is not aligned. A slip
  in the `madvise` is a refused call or a huge page flag on memory the table does not own,
  not undefined behaviour. The exposure is carried knowingly until a scheduled Miri run
  reports on it
- a bench tree size measured before mate distance pruning cannot be read against one
  measured after it. `bratko kopec 1` and `wac 4` are both forced mates, proved at depth
  five, and without the pruning every iteration after that proved them again over a tree
  growing four and a half times a ply. At the bench's depth the two were 45,692,972 of
  47,836,191 nodes, 95.5%, so a percentage of "the bench tree" from before is a
  percentage of those two and little else. An entry below that gives a figure over
  sixteen positions is already clear of them; one that says "the bench tree" is not. At
  the depth of eleven the same change bought, the suite's total is the latest `Bench:`
  trailer, and no single position is more than about a fifth of it
- mate distance pruning landed without a strength result that settled. Two runs played
  3,500 games at 10+0.1 against `464d3cc`. The first was an sprt of [0, 10] and failed at
  its third batch at -11 ±12 over 1,500 games, which only says the games did not favour
  ten elo over nothing. The second asked [-5, 0] and carried the first's pairs in. It
  played all four of its batches and reached neither bound, ending at a log likelihood
  ratio of -1.81 against ±2.94. Over all 1,750 pairs, read as a fixed sample, the
  difference is -8.3 with a 95% interval of -16.2 to -0.5. That interval only just
  excludes zero, and not robustly: the first run stopped on a failure bound and the second
  was chosen after it. So the change likely costs a little, and that it costs anything is
  not established. The last batch was +1. Carrying the test on wants `prior_pairs`
  109,440,722,384,95, and halving the interval wants about ten thousand further games, so
  it is not cheap to settle. It was landed for what the bullet above describes rather
  than for strength
- the rate a match reports for a side that prunes mates is not that side's speed. Both
  runs put the candidate near 0.95 times the baseline's rate, and that is composition.
  The nodes the pruning removes run at 6,841,289 nps against 2,723,277 for the other
  sixteen bench positions, 2.51 times cheaper, and pricing the missing 7.7% of nodes at
  that discount predicts the observed time and rate ratios to within 0.003. The cost a
  node really carries was 0.642% of its instructions under callgrind, or 0.457% behind the
  `is_mate` guard c7730f1 shipped, which is worth well under an elo; 488dae0 replaced that
  guard with a one sided test, and the rule is now 667,863 of 242,410,349 instructions
  (0.28%) at depth five. A reader who takes that rate column for a slowdown will go
  looking for five percent that is not there, which has happened once already

## Measured and rejected

Ideas that look right on paper and have already been tried. Do not propose one
of these again without saying what is different this time.

- Carrying the moving piece in `Play`, to save the `get_piece_index` walks in
  make, unmake and MVV-LVA. Implemented correctly, node counts identical, and
  4-6% slower: the struct grows from six bytes to seven, which takes the inline
  `MoveList` from 384 bytes to 448. It would only pay bit-packed into the spare
  bits of `from` and `to`, which is a different change to measure.
- A `MoveList` inline capacity other than 64. Thirty-two and forty-eight are
  4.2% and 3.4% slower, ninety-six and 128 marginally slower. The list is a
  stack local at every recursion frame, so inline bytes multiply by depth and
  trade against a 48KB L1D. Spilling is already negligible; there is nothing
  there to fix.
- Holding the move lists in one arena indexed by ply rather than a `SmallVec` at
  every recursion frame, so that consecutive plies pack against each other instead
  of sitting a 392 byte slot apart for the 54 bytes a nine move list uses. Not
  implemented: callgrind bounds what it could win first, and the bound is small.
  The search misses L1D on 0.66% of its data references and 96% of those are
  served by L2, so every data cache stall together is at most 11% of the run and
  every write miss, the transposition table's own included, at most 5%. The move
  lists are a minority of that. The entry above is the same finding from the other
  side: neither term of that curve is large, which is why no inline capacity wins
  much either.
- Requiring the game's own two prior occurrences before a pre-root repetition
  scores as a draw, the line Stockfish draws. Lost -22 ±18 over 446 games at
  5+0.05 (sprt [0, 10] failed, PR #107): an eval this simple is better off
  taking every draw the history makes available than re-fighting positions it
  half-understands, and the stricter rule spends depth keeping alive games it
  then loses. Tapered evaluation has since landed and this was not re-measured
  against it; worth re-asking once king safety lands too.
- A correction history: a table of the running error between the static
  evaluation and the search results that followed it, keyed by the pawn
  structure, nudging the evaluation the two shortcut gates read. Two SPRT runs
  at 5+0.05 against master both ended inconclusive at their caps (sprt [0, 10],
  branch search/correction-history): +12 ±11 over 1,980 games, then +1 ±12 over
  another 1,980. Two stopped runs do not pool into one interval, and neither
  recorded its pairs, so a re-run of this arm starts the test again. The
  mechanism was live and the tree 2.2%
  smaller with the tactical suite unmoved, so the games say the corrections
  were nearly free rather than nearly right. One suspect is on record: the
  correction's ±31 centipawn clamp is twice the fifteen the reverse futility
  margin was sized to keep clear of a mate it can miss, so the table may spend
  its gains inside the margin's own headroom. A re-ask starts from the shadow
  sampler's records (the raw evaluation's clearance of beta, the correction
  offset, the depth and the reference outcome), chooses a corrected margin on
  held-out positions before any games, and measures correcting the leaf and
  correcting the gates separately before together. The pawn key the arm was
  built on landed on its own.
- Correcting the evaluation reverse futility reads by a table entry's score and
  bound (a stored floor above the evaluation raised it before the margin was
  measured). Inconclusive at the game cap, +6 ±11 over 1,980 games at 5+0.05
  (sprt [0, 10], branch search/tt-refined-eval), with four tactical positions
  lost for a 5.3% smaller bench tree. The refinement pruned in the right
  direction, but it bought no measured strength and the tactical losses at
  that margin are not acceptable. It is not a sound bound either: a stored
  score is a bound at the depth it was searched to, not at the greater depth
  the gate is asked about. Worth re-asking once the reverse futility margin is
  recalibrated against corrected estimates rather than the raw evaluation's
  error, which the residual harness exists to do. Refining the null move gate
  the same way was rejected inside the same arm: it grew the tree and changed
  nothing the tactical suite could see.
- Moving the reverse futility pruning margin away from a hundred centipawns a ply. The
  shadow lane prices a margin with no game played: a rule with margin `m` fires
  on a candidate row exactly when the evaluation's clearance of beta is at least
  `m` times the depth, and whether the reference came back under beta does not
  depend on `m`, so one run scores every margin over the same rows.
  `residuals 7 every 1 cap 400000` keeps all 347,946 events of the bench's tree,
  and at the shipped margin the shadow rows reproduce the live gate depth by
  depth, 143,917 firings and 29 crossings; that agreement is what says the
  offline reading is the live one. The margin turns out not to be spare. Its
  pooled crossing rate of 0.02% belongs to the three quarters of the candidates
  that clear beta by more than five pawns a ply and would fire at any margin at
  all. What a tightening buys is the band beside the boundary, and that band is
  the dear one. Over the three hundred held-out positions of `tactics.epd`, the
  band from a hundred down to ninety five crosses at 0.47% and the band from a
  hundred down to ninety nine at 1.39%, against 0.04% over everything already
  firing. Two of the ten crossings those five points buy are forced mates
  against the side that would have cut off, one at depth four with the
  evaluation standing 399 above beta, which is a centipawn of margin between the
  shortcut and a lost position. The bench's own eighteen positions put the same
  band at 0.10% and its halves cannot resolve it at all, so the size of this
  number is read off the corpus and not off the bench. The two suites agree.
  Nine margins from ninety one to ninety nine move the bench between 0.6% and
  1.9% smaller with no order to the sizes, and the tactical count over the same
  nine runs 227 to 229, unordered as well: 227 at ninety nine and 229 at ninety
  five. A count at any one of them is the tree being reshuffled rather than the
  search answering better. Under ninety one the depth four mate in two goes.
  Games were played afterwards, at 10+0.1 under sprt [0, 10]. Ninety five, on
  the evaluation before the joint refit, read -12 ±23 over 500 games (95% -35
  to +12, LLR -1.19) and was stopped after one batch by its registered futility
  rule, so it is unresolved rather than rejected. On the refitted evaluation the
  margin's answers cross 1.72 times as often, and the depth four mate in two
  holds down to a margin of 84. A hundred and twenty read -3 ±10 over 2,500
  games (95% -12 to +7) and failed at an LLR of -3.05, which closes that
  widening. A probe of 1,000 games a point with no sprt read sixty at +9.7 ±16
  and two hundred at -4.5 ±16, and sixty sits under the mate boundary. A
  hundred and twenty five, played together with a tempo term for the side to
  move, read +0 ±8 over 4,000 games and failed at -3.19. None of these points
  resolves from zero, so nothing played supports a narrower margin or a wider
  one, and a hundred stays. Re-ask with the correction history, which is what
  would move the clearance the rows are read by.
- The delta margin in quiescence, measured on its own. It landed in one pair
  with principal variation search, and the pair's +50 ±24 over 530 games at
  10+0.1 (sprt [0, 10] passed, PR #171) sits on the margin's commit. Turning
  the margin off against that master gave +7 ±17 over 840 games at 10+0.1
  (sprt [-10, 0] inconclusive at the time cap, final LLR 1.47, branch
  ablate/delta-off): the games could not see the margin at all, and the pair's
  gain is principal variation search's. The margin stays on its switch because
  it costs nothing measurable. Taking it out again on the refitted evaluation,
  which moves the matching margin only from 200 to 215, read -13 ±21 over 500
  games at 10+0.1 (sprt [-10, 0], LLR -0.73) and was stopped after one batch by
  its registered futility rule, so the rule stays. The lesson is one guess
  per run, unless two parts cannot be measured apart.
- Exempting quiet moves that give check from the late move reduction. Lost
  -18 ±18 over 860 games at 10+0.1 (sprt [0, 10] stopped at the time cap with
  the likelihood ratio at -2.72, a fraction from accepting H0, PR #185). The
  exemption regained seven tactical positions at fixed depth, WAC.118 among
  them, and still lost the games: every checking quiet searched whole grew the
  bench tree 2.6%, and at this control the depth buys more than the accuracy.
  The board's gives-check test the arm was built on is exact, oracle-tested
  and free when nothing calls it, and landed on its own. Re-ask only as a
  narrower guess: a model that exempts the checking quiets whose reduction is
  measured harmful, or an exemption near the leaves alone, each its own arm.
- Exempting every open window node from the late move reduction and the
  shallow rules as well as the shortcuts, beside the root's beta. Lost
  -23 ±13 over 1,400 games at 10+0.1 (sprt [-10, 0] accepted H0 at an LLR of
  -3.95, branch commit 8ae6023). The bench tree grew 10.6% and the suites
  gained a tactical position and 3,121 strategic points, which the games did
  not pay for. The shortcuts alone are refused at an open window instead.
  Re-ask only as a reduction by one less at open windows rather than none.
- Ranking the quiet moves by cutoffs per node spent, in place of the history
  table's cutoff score. Lost -53 ±26 over 420 games at 5+0.05 (sprt [0, 10]
  accepted H0, branch research/cost-aware-ordering) with the bench tree 1.8%
  larger. A one-term comparison placed the blame: the cost divisor alone grew
  the tree, while the rest of the change (smoothed plain counts in place of the
  depth squared bonus) shrank it slightly. A try's cost spans four orders of
  magnitude and mostly reports the depth the try ran at, so the ratio ranks by
  depth noise. Worth re-asking only with the cost normalised by depth; the
  smoothed counts ranking on its own is a separate small candidate.
- Lowering the deep reduction's depth floor, so that the model gate reaches
  the depth three nodes where most of the reduced scouts are. The population
  is real and the value is not. `reductions 7 every 1 cap 200000` keeps all
  75,091 of the bench's scouts, and 50,394 of them stand at depth three, 67%,
  with depths three and four together 89%; `reductions 7 every 50 cap 400000
  epd arche-core/strategy.epd` puts depth three at 68% of 228,033 over the
  held-out positions. The floor is `DEEP_REDUCTION + 2`, so none of that two
  thirds is reachable by the deeper scout or by the skip. But a depth three
  scout is a depth one search, and against a warm table it is mostly one node
  answered from it: all 50,394 of them together cost 88,259 nodes of a
  4,162,584 node tree, 2.1%, a mean of 1.8 nodes each. Counting scouts is the
  wrong denominator: what prices a reduction is the nodes it removes. Measured
  against master at `ce8b662`, with the two halves of the floor asked apart,
  the deeper scout at depth three is quiescence, and it grows the bench tree by 0.17%
  while the strategic suite falls 813 points (92502 to 91689) and the
  tactical count rises two (221 to 223). The skip at depth three leaves the
  suites where they were (221, and 8 points down) and saves 0.70% at depth
  seven, 0.43% at eight, 0.13% at nine and -0.04% at ten: what it
  takes off the shallowest nodes is given back as the tree around them
  widens, so nothing of it is left at the depths a game reaches. Both
  together are 1.18% of the bench tree, 223 tactical and 91625 strategic.
  The skip alone at depth three was later played against `57ec3c6`: -11 ±21
  over 500 games at 10+0.1, an sprt of [0, 10] stopped after that batch
  because its estimate was under zero. Re-ask only with a reason the depth
  three nodes have become expensive, which is a change to what a depth one
  search costs rather than a change to the reduction.
- Widening the late move pruning band, so that a late quiet the attention
  model prices at or under -6000 is skipped where the threshold stood at
  -7954. These scores are on the weights fitted on 2026-09-06, which the
  refit below replaced; the refit's thresholds are not points on this scale.
  The offline reading was favourable and the games could not see it.
  What prices the move is the rate in the band the wider threshold newly
  reaches, since the moves already skipped are skipped either way and the
  moves the reduced scout writes off are written off either way. Over the
  fifteen hundred held-out positions of `strategy.epd` that band hands back
  0.114% of 18,442 rows for a full search (95% upper bound 0.163%), against
  the 0.16% at depth four, 0.21% at five and 0.46% at six that the reduction
  already gets wrong, so -6000 is the last point on the grid under all
  three. The tree came out 1.68% smaller at depth seven, and 14.6% smaller
  at depth nine over the sixteen bench positions the skip can reach.
  Two sprt batches at 10+0.1 against master at `ce8b662` (sprt [0, 10]) then
  put it at nothing: -13 ±24 over 500 games with the likelihood ratio at
  -1.18, and +1 ±17 over a further 1,000 at -0.47. The first batch's -13
  sits inside its own interval and the second contradicted it, so the number
  to read is the 1,500 games together, which are centred near zero. The
  batches ran from different seeds and neither carried the other's pairs in,
  so their ratios were added by hand, which is weaker than a run that carries
  them, and -1.65 against a -2.94 bound falls short of accepting H0. The
  tactical count went 221 to 220 and the strategic total 92502 to 92456, both
  small enough to be the tree reshuffled. The band reading was sound about
  what the skip costs in accuracy and says nothing about what the nodes it
  saves are worth, which at this control is nothing a game can see. Re-ask
  only at a control long enough for 1.7% of the tree to show, or with the
  skip moved to where it takes more than that. On the refit's scale cc865ba
  later widened the band from -5932 to -5457 as a non-regression pass (+17
  ±15 over 1,000 games, sprt [-10, 0]), not a measured gain; -4308 read -17
  ±22 over 500.
- Fitting the attention model against the two points the gates read, rather
  than by log loss over every row of the reduction ledger. The thirteen
  integers come from a logistic regression over the whole ledger, while the
  search reads the ranking at two pinned coverages, so a direct search for
  the vector that lowers the attention rate inside the skipped region and
  the deeper-scouted band is a different objective and the one the gate
  wants. (The deeper scout has since moved to an index rule, so only the
  skip's point is left.) Measured on 2026-09-19 at `54d85b9` over the 1,428 game roots at
  depth 8 (`reductions 8 every 4`), with the features, the corpus, the split
  by source game and the search policy held fixed so the objective was the
  only variable. On the half no fit saw, at coverage pinned to the live
  thresholds', it lowers the two rates' mean 1.145 times against the shipped
  vector and 1.121 times against a logistic refit of the same rows (95%
  [1.070, 1.174]); the refit on its own reads 1.022 with an interval
  covering one, so refitting the current objective buys nothing that ledger
  can read at either gate. **Nine tenths of the gain is wasted scouts.** At
  the deep reduction the fail highs fall 1.529 times (95% [1.420, 1.668])
  while the harmful fail lows, the moves written off wrongly, move 0.995
  times (95% [0.962, 1.030]); decomposed on the quantity the bar read, 90.7%
  of the gain is those fail highs and 11.5% is late move pruning's own
  region, where every attention row is a harmful fail low. So about a ninth
  of it is a real reduction in errors, at the gate whose rate that corpus
  resolves worst. The label these weights are fitted on adds a wasted scout
  to a wrong answer, and an objective over it moves mostly the cheaper one,
  because that is where the rows are. Two grouped refits and this search
  have now produced no vector worth a match. Re-ask with a feature the model
  does not have, or with the label split into the two costs, not with
  another fit. A fit was asked again on 2026-09-26 for a different reason:
  every earlier fit dropped the rows the model skips, and none was fitted
  on the skip's own rows or on the tree the skip now runs in. Over the
  skip's rows of 75,024 game positions, skipped ones included, the refit
  skips 1.230 times fewer attention rows at the same coverage on the pairs
  it did not see, and plays even with the old weights over 4,500 games
  (+0 ±7, sprt [-10, 0] passed). It ships as a model fitted to its own gate
  at no measured cost, not as a gain.
- Prefetching a child's transposition slot straight after `make_move`, 6.7%
  slower over six interleaved rounds. The prefetch sits immediately before the
  recursive call and the child probes the table almost first, so there is no
  latency to hide and all that is added is an index multiply on every made
  move. Inside `make_move`, after the key is finalised, is the only placement
  that could pay, and it needs the key folded up front first.
- Picking the next best move on demand inside the tree instead of sorting the
  whole list, so a node that cuts off early never orders the moves it never
  reaches. Node counts identical, 8.6% slower in nps between medians over five
  interleaved rounds with a spread near 3%, and a variant that left quiescence
  sorting whole was still 6.1% slower. The premise does not hold here: a node
  with a table move searches it before generating anything, so the cheap
  cutoffs never sorted at all, and what does reach ordering mostly consumes
  its list, where a selection per pick does about twice the compares the one
  sort did. It also carries a row of scored keys per ply, and the two entries
  at the top of this list already say what growing the move path's working set
  costs. The waste it aimed at is real (half the quiet keys at depth seven were
  computed at nodes that cut off before a quiet move was tried); what lost was
  paying a selection per move tried, and scoring the quiets in one pass when
  the search reaches them took the saving instead, 2.7% fewer instructions per
  node. A narrower form landed later in 280ee93: the quiet run picks up to four
  moves by selection and sorts the rest, and at depths one to three sorts only
  the moves the shallow rules leave, 3.9% fewer instructions in the search over
  the full bench and 1.2% more at depth five.
- Masking a `u8` square down to six bits where it indexes a table of sixty
  four, so that the bounds check comes off the load. The answer is per site
  and measured rather than a rule. It pays on the board's square array and
  on the read the move sort makes of the history table, and both are
  written that way. It is worse on the zobrist table, 1.2% more
  instructions when it was first measured and 0.6% more on the tree as it
  stands, and worse on the castling tables, 0.4% more. On the magic tables
  it is worth nothing either way, though the one index there is checked
  against four tables, which reads as the compiler already sharing those
  checks. There is little in it at any site: the check a mask takes away is
  cheap and always predicted, and the and it puts in sits in the address
  computation, so the measurement is the whole answer. The history table is
  the entry to read this one by. It was measured at 0.8% more and rejected
  here, and on the same two indexes it is now 0.6% less, having been asked
  again after the loop around the read was rewritten. A mask that lost once
  is worth re-measuring when the code holding it moves, the way
  `inline(always)` moved on `Quiet::bonus` below. Only the quiet keying's
  reads carry it (`Quiet::bonus` and `key_quiets`, SSE and scalar). The
  write in `cutoff` and the census read are cold and keep their check,
  which is the better failure for a square that cannot be out of range: a
  check panics where a mask reads a different square.
- Keeping the swap's attacker set across a static exchange and adding only
  the sliders each capture opens, rather than finding the attackers again
  from the board. The set it builds is the same one, since taking a piece
  off the board can only open a line onto the square and never close one,
  so the set carried forward and masked by what still stands is what a
  fresh lookup returns. Node counts identical and 0.2% more instructions.
  The swap is too short to pay for it: under two captures past the first
  on average, so the set is found again once or twice, and the two slider
  lookups that costs each time are most of the lookup it replaces. Hoisting
  the steppers alone does pay, and is what the swap now does: the pawns,
  knights and kings bearing on the square do not depend on the occupancy at
  all, so they are found once and the sliders are still looked up fresh.
  That is 0.12% and carries none of the accumulation this entry rejected.
- Two other shapes for putting the sorted moves back, both slower than the
  runs the sort now copies. Walking the passed-over places one bit at a
  time into a destination slice cut to the popcount, so that the write has
  no bounds check, is 1.3% more instructions than walking them into the
  whole list. Copying a run element by element instead of with
  `copy_from_slice` is 2.1% more: a `Play` is six bytes, an awkward width
  to move one of, and a run averages eight of them, which memcpy does in
  one go. A third shape, measured once the no key and one key lists had
  been taken out of the sort: lifting only the keyed moves out to the
  buffer and closing the runs up inside the list with `copy_within`, the
  runs that move down in list order and the runs that move up after them
  in reverse, so the whole list is never copied out and back. 0.92% more
  instructions, node counts identical. The two straight copies are cheap
  per byte; lifting the keyed moves one at a time and keeping the runs
  that move up back for a second pass cost more than they saved. Skipping
  only the run copies that land where they stood is exact and worth
  0.05%, too little to carry.
- `#[inline(always)]` on the shelter count, `eval::shelter::counts_of`, which
  moved 127 instructions of 3.4 billion over the bench and is not carried,
  although the function is read twice at every leaf and quiescence node, where
  the attribute has paid elsewhere. It is small enough that llvm inlines it
  unasked.
- `#[inline(always)]` on `square_attacked`, 3.7% more instructions, and on
  `Quiet::bonus`, 3.1% more. Both are called from inside a loop the register
  allocator then runs short in, and forcing them in is what tips it. The
  bonus is the case worth keeping in mind, because the answer moved: that
  3.1% was measured where the key function around it was itself always
  inlined, and where the scoring loop calls it directly instead the
  attribute pays and the code carries it. So the answer belongs to the call
  site rather than to the function, and nothing about the shape of either
  says which way it will go. Measured where it stands, the attribute does
  pay on `move_piece`, `undo_move`, `search_child`, `ordering_key`, the
  table's probe and the lookup behind it.
- Comparing a `Play` as its six bytes, the way `CastlePermissions` compares
  its four. 0.9% more instructions. The derive reads a field and branches,
  which is the right shape here: the killers are asked about every quiet
  move in a list and a move that differs usually differs in the from square.
- Reading the moving piece off the square array at the top of `make_move`,
  so that the pawn board is not consulted as well for the fifty move reset.
  0.2% more instructions: carrying the value across the block costs more
  than the load it saves.
- Sharing the slider attack sets between the mobility count and the move generator at the
  same node, so a slider of the side to move is probed once instead of twice. Not built:
  the whole saving is bounded at 0.385% of the run and writing the sets costs 0.781%,
  because the stand pat cuts three quarters of quiescence evaluations before they generate
  anything at all.
- The late move gate scoring nodes the shortcuts had already scored. A count over the bench
  found it scoring 884 such nodes through the uncached `eval::eval`; `shortcuts` now hands
  back the evaluation it read and the move loop seeds the decision's memo with it. On the
  bench at `54d85b9` the gate then asked for an evaluation at 2,515,532 decisions and used
  the uncached door 671 times, against 23,717,724 evaluations over the run; the tree has
  moved since and these have not been taken again.
- Lazy mobility, leaving the term out at the quiescence stand pat when the rest of the
  score already clears beta by a margin. Built and played at three margins, 6,000 games at
  10+0.1 under sprt [0, 10]: +1 ±8 over 4,000 games at a margin of a hundred, and nothing
  the other two could see either. The mechanism does work, skipping the term at 71% of
  evaluations for 6.05% off the run to a fixed depth, and no game can tell. Branches
  `eval/lazy-mobility`, `eval/lazy-mobility-200` and `eval/lazy-mobility-50` hold the code,
  and the pairs were recorded, so any of the three tests resumes rather than restarts.
  Re-read at 8f9dafe, where the walk is shared with the king attack zone, a margin of a
  hundred fires at 22.3% of stand pats and could save at most 0.63% of the search before
  its own cost, and a cut exactly where the full score cuts at most 1.19%.
- Beginning a deepening later in the clock share than 45%. The 45% was placed from the
  time through one depth over the time through the next, which had a median of 0.29 on a
  tree with no pruning but the transposition table. Read again at b26b9f0 over 80 self-play
  games at 10+0.1, leaving out the depths after a mate score, the median is 0.51 for the
  iterations that end at the shares where the bound decides, and an iteration begun at 45%
  finishes inside the budget two times in three. The rule the 45% was placed by puts the
  line near two thirds on that ratio. The games did not follow it. Against 9d09647, 55% lost
  -16 ±15 over 1,000 games (sprt [0, 10] failed, LLR -3.31, branch tune/soft-bound-55) and
  65% read -3 ±11 over 2,000 (inconclusive at four batches, LLR -2.57, branch
  tune/soft-bound-65), so the ratio says the old reasoning is stale and not that the line is
  in the wrong place. The 65% test resumes with `prior_pairs` 77,236,380,242,65. Re-ask at a
  longer control, or beside a change to `ASSUMED_MOVES_TO_GO`, which was measured with 45%
  in place.
- Declining the reverse futility cuts the pair term marks as riskiest. On 4.0 million fresh
  cuts from `arche residuals`, a model reading the pair term put 1,078 of 5,674 crossings in
  its riskiest 5%, against 929 for depth, phase and slack alone, +149 (95% +92 to +232).
  Risk fell as the pair term grew. Played as a guard declining that 5%, it lost -9 ±11 over
  2,000 games at 10+0.1 (sprt [0, 10] failed at LLR -4.41) at master's node rate. Two
  variations then screened negative offline, a license for near misses and the same model
  on null move cuts, so the direction is closed and the constant margin stays.
- Moving the quiet futility margin or the null move pruning evaluation unit after the joint
  refit. Quiet futility at ninety centipawns a ply in place of a hundred read -10 ±21 over
  500 games at 10+0.1 (95% -30 to +11, LLR -1.31) and was stopped after one batch by its
  registered futility rule. The ninety was read off the refitted evaluation's error at the
  rule's nodes (84 at search depth seven, 95 at nine), and the bench tree's 10.7% fall did
  not reach the games, where the candidate searched 0.998 times the baseline's nodes. The
  null move's evaluation unit at 150 in place of 200 read +4 ±8 over 4,000 games (sprt
  [0, 10] unresolved at the cap, LLR -0.55). A probe at half and double, 1,000 games a
  point with no sprt, read 100 at -14.3 ±17 (95% -31 to +2) and 400 at -2.4 ±15, which does
  not locate the unit's best value. Both constants stay where they were.
- SEE pruning in the main search at depths one to three. A quiet losing more than
  50 × depth² on its square went unsearched: -3 ±10 over 2,500 games at 10+0.1 (sprt
  [0, 10] failed at LLR -3.28). Built as first specified it grew the bench tree 15.2%,
  because the late move count searched another quiet in place of each one dropped, and
  counting the drops as searched cut that to 4.7%. A capture whose swap loses more than
  100 × depth went unsearched in a second arm: +7 ±8 over 4,000 games, unresolved at the
  cap at LLR 1.32 after reaching 2.88 of 2.94 at its second batch, with the candidate at
  0.96 to 0.97 times the baseline's nodes in level time. A study of what the two rules drop
  found them dropping the right moves (0.4% and 0.1% of those would have beaten alpha), but
  those moves cost only 3.1% and 7.7% of the bench, because the other rules already refute
  them nearly for free. The form not tried is a floor that eases with depth at every depth,
  and that is the re-ask.
- Extending the deadline for the re-search of a root that failed low, to three shares capped
  at 33% of the clock. +4 ±8 over 3,000 games at 10+0.1 (sprt [0, 10] unresolved at the cap,
  LLR -0.32). The population is small: 3.0% of master's moves stop at the deadline with a
  fail low open, and 2.56% of the candidate's moves ran past their share. The registration
  said before play that 3,000 games could not resolve +10, so this is unresolved rather than
  rejected.
- Halving the history table at each `go` instead of clearing it, the killers still cleared.
  +1 ±9 over 3,000 games at 10+0.1 (sprt [0, 10] unresolved at the cap, LLR -1.85), under
  the +3 the registration named as falsifying it. Time, nodes and rate were level with the
  baseline in every batch. Offline the carried history visited 0.812 times the nodes to
  depth ten summed over a game, but 0.981 at the typical position, and the games agreed with
  the second. Halving is closed as the decay. Keeping the whole table, or another divisor,
  is its own arm.
- Trying the quiet moves the ordering scores at zero in an order drawn from a seed rather
  than in generation order. An offline gain of 0.177 points of policy value did not play:
  -14 ±15 over 1,000 games at 10+0.1 against `251179a` (sprt [0, 10] failed at LLR -3.00),
  with the candidate at 0.97 to 0.98 times the baseline's node rate.
- Replacing the late move pruning skip's attention model with a rule on depth and move
  index, `index >= 12 + 2 (depth - 4)`. Against the model's older weights it read -4 ±7 over
  6,000 games (sprt [-10, 0] unresolved at the cap). Against the refitted model it lost
  -43 ±16 over 1,000 games at 10+0.1 (sprt [-5, 0] failed at LLR -3.04), where 5 elo was the
  most deleting the model's code was allowed to cost. Offline the rule read 41.3 times the
  refitted model's attention rate at 3.5 points less coverage. The -43 is a stopped
  estimate, and the model stays.
- Replacing the pair term's table with one the held out loss prefers. Two tables that read
  level or better on that loss lost their matches. Fitted at a factor ridge of 3e-8, the rank
  sixteen term widens from 36 to 125 centipawns and gains 0.001962 of cross validated loss
  over the table then shipped, at 13.3 pair standard errors. It lost -80 ±26 over 500 games
  at 10+0.1 (sprt [0, 10] failed at LLR -4.52), and -112 ±26 with the reverse futility
  margin at 175 (LLR -5.53), both at level node rates. The 3e-7 table between them (57
  centipawns) passed and is the one shipped. A rank eight table with five named features
  (phalanx, supported and connected pawns, rooks on open and half open files), refitted at
  3e-7, matched the shipped table's held out loss (+0.000012, 0.3 pair standard errors) and
  then failed non-regression: -21 ±14 over 1,500 games at 10+0.1 (sprt [-10, 0] failed at
  LLR -3.27) at 0.997 times master's rate. The rank eight control without the names was not
  played, so the loss is not divided between the narrower table and the names. Somewhere
  between 57 and 125 centipawns the held out loss stopped being a guide to play.
- Tapering the transposition table's aging by the root's phase, from eight plies a search at
  full material down to an endgame weight with only kings and pawns left. A screen at two
  million nodes a move over 53,658 positions from games that reach ply 100 found every lower
  weight dearer in nodes to the depth eight reaches, on the moves whose root phase is six or
  less: 1.047 times at four (98.3% bounds 0.982 to 1.115), 1.147 at two and 1.526 at zero.
  A match of four against master at 30+0.3 with a 256 MB table then lost -10 ±14 over 1,000
  games (sprt [0, 10] accepted H0 at LLR -3.02). The reverse, sixteen plies a search in the
  endgame, is a separate test, dispatched on 2026-10-06 as run 37431969608 and still open.
