# Roadmap

What the engine does not do yet, and what it does badly enough to be worth writing down.
See [DEVELOPMENT.md](DEVELOPMENT.md) for how to measure whether one of these helped, and
[INSTRUMENTS.md](INSTRUMENTS.md) for the commands the entries below quote.

## Not implemented yet

The measurement spine (the bench, the commit trailers, a reference search to
compare against) is in place, so each of these arrives with its numbers: a
`Bench:` trailer always, and an `Elo:` trailer from an SPRT when it changes how
the engine plays. Roughly in the order they look worth doing.

- the rest of the late move reductions. The first arm reduces a late quiet move by one
  ply, flat, and a reduction that grows with the depth and the move count is the next
  match. Reducing the losing captures and reading the history table for the eligibility
  come after that, each measured on its own
- evaluate drawn positions
- the rest of evaluation: the rest of king safety, the rest of pawn structure, and
  special cases such as the bishop pair and open files. Mobility is counted for the
  knight, the bishop, the rook and the queen and has been fitted once, and the fit
  left it a rook count:
  six of the eight weights rounded to zero, so only the rook is counted at the leaf
  until a refit prices another piece. King safety counts the pawns on the two ranks in
  front of the king, the open and half open files beside it, and the enemy pawns on the
  three ranks in front of it, and all fourteen of its weights are fitted. What is not
  measured at all is the squares the enemy pieces attack around the
  king, which wants the attack sets the mobility count already walks. The storm is followed
  three ranks and no further, so a pawn four ranks out is not counted, and a storm pawn
  blocked by one of ours counts the same as a free one. Pawn structure counts a side's
  passed pawns by the rank they have reached, its isolated pawns and its doubled ones,
  read off the two pawn boards alone and remembered under the pawn key, and all sixteen
  of its weights are fitted. The seventh rank is the one to distrust: a passed pawn there
  prices below one on the sixth at both ends of the taper, which survives every ridge on
  the grid, and the quiet filter is the likely cause since a position with a passer one
  square from queening is rarely settled unless the pawn is blockaded or falling. Thirteen
  of the strategic suite's fifteen themes rose when the term was fitted; the two that fell
  are AKPC by 112 and 7th Rank by 217, and the second of those is the same seventh rank.
  What it leaves out is everything that reads a square rather than a file: whether
  the square in front of a passer is occupied or attacked, how far each king stands
  from the promotion square, candidate pawns, connected and backward pawns, pawn
  islands, and the rule of the square. The first two are the valuable ones and
  neither can sit behind a key over the pawns. The tuner that sentence asked for is
  built: `arche terms`
  states what each position's evaluation is made of and `scripts/tune.py` fits and scores a
  weight vector against the games, so a candidate term is one appended column whose
  held-out loss can be read before there is engine code for it. What is not settled is
  whether a fit on our own games buys strength, which is the re-tune's own sprt
- the rest of the uci protocol
  - the only options advertised are `Hash`, the `Clear Hash` button and a `Threads` fixed at
    one, so everything else an interface might set, `Ponder` among them, is refused rather
    than acted on
  - `ponderhit`, `debug` and `register` are not handled, so pondering is still out of reach
    even though `stop` is answered now
  - a move in a `position` line that cannot be played is reported the same way whether
    no move of that name exists here or the move exists and leaves the king in check.
    `make_move_str` answers with a bool, so the interface is told which move failed and
    not what was wrong with it
- read an opening book in the engine, only the lichess-bot image has one at the moment and it is
  lichess-bot that reads it rather than the engine
- winboard

## Known limitations

- the strategic suite's total cannot be read against zero. One of the fourteen king
  safety weights changed by a centipawn moves it +242 or -170, twenty eight such nudges
  have a standard deviation of 447, and single themes move up to 510, while the fit that
  took +44 elo moved it -121. It discriminates between vectors of the same size and not
  between a term and its absence, which is how it should be used and what
  `docs/DEVELOPMENT.md` now says. Three themes lose under any shelter term at all:
  Recapturing takes 90.7% of its points already and can only regress, and Square Vacancy
  and Advancement of a/b/c pawns lose under every arbitrary vector tried
- the fitted term makes the engine keep the pawns in front of its own king at home, and
  one graded theme says that is wrong. The midgame cover weights are +10 and +21, and they
  do what they say: over the strategic suite the engine advances a pawn on its king's file
  or a neighbour 100 times where it advanced 123 before, and 167 times with the weights
  negated. AKPC grades such a push as the best move in 79 of its 100 positions and the
  engine now plays one in 22 of them against 31 before. That is the one place the suite
  and the term disagree about chess rather than about noise, and the games were played
  with the term as it stands, so what is unresolved is whether declining those pushes is
  right in positions the games under-sample rather than whether it costs elo overall
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
  selection group's -0.000649. A vector revised after that reading needs sealed games this
  corpus does not hold, so the next fit wanting an honest held-out interval wants games
  this corpus never saw. The access log is not in this repository; it is kept with the
  run's record in the planning repository
- an evaluation term is allowed 5% of the search, and mobility is over it. The figure had
  no home in the repository but the king safety bullet this list used to carry, so it is
  written here instead of being lost with it. Measured on the bench at the commit that
  cached the shelter: `mobility_with` takes 334,907,944 instructions of 3,950,062,514,
  which is 8.48%, and the shelter with its cache takes 133,627,554, which is 3.38%.
  Mobility has been over the budget since the fit that priced it and nothing has charged
  it since, so either it wants the treatment the shelter has just had or the 5% wants
  restating as what it is, which is a rule of thumb nothing enforces
- a held-out loss on our own games cannot resolve a fit of the piece square tables one way
  or the other, so an sprt is what decides a re-tune. Measured 2026-09-10 over 1,812
  archived games, 100,726 quiet positions across 1,807 of them: the shipped weights score
  0.093561 on the selection games and the fit psqt.rs now holds beats them by 0.000618
  against a standard error of 0.000627, which is inside its own interval. The sealed third
  of the games, opened once after the vector was frozen, reads the same fit 0.001754 better
  against 0.000674, which is outside it. The two readings differ by 1.23 standard errors,
  so they are one corpus disagreeing with itself rather than two findings, and the games
  settled it at +54 ±13 over 2,000 at 10+0.1. The corpus is the engine's own play, so the
  positions it never reaches are unlabelled, and that is the ceiling on what any fit of it
  can say. The 2026-09-12 corpus holds sixteen times the games and did resolve a fit, at
  5.6 standard errors, but of fourteen weights rather than 768, so it says the corpus was
  small for that question as well as the question hard. A re-tune of the tables on it has
  not been run
- the harness said otherwise until 2026-09-10, and why is worth keeping. It split the
  corpus on the fen, and 1,805 of the corpus's 1,809 games had rows on both sides: 53.1% of
  the held-out rows had the position a ply away, from the same game and carrying the same
  label, sitting in the training set. Read that way a fit of the same tables came to
  -0.005831 at 27.8 standard errors, which is what the leak was worth. Any number quoted
  from a tuner run made before that date is a number on a split that held nothing out
- a transposition score that came from a repetition or fifty move draw is trusted, except
  within four plies of the fifty move horizon, where every cutoff is refused. The search can
  therefore read a draw down a path that could not reach it; the policies were played against
  each other and refusing such scores lost about forty five elo to trusting them, so the
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
  another 1,980. Two stopped runs do not pool into one interval, so there is no
  combined number; a re-run of the same arm carries the pairs of the earlier
  ones in and is read as one test over all of them, which is how repeated runs
  are read from now on. Neither of these two recorded its pairs, so a re-run of
  this arm starts the test again. The mechanism was live and the tree 2.2%
  smaller with the tactical suite unmoved, so the games say the corrections
  were nearly free rather than nearly right. One suspect is on record: the
  correction's ±31 centipawn clamp is twice the fifteen the reverse futility
  margin was sized to keep clear of a mate it can miss, so the table may spend
  its gains inside the margin's own headroom. That is one arm of the re-ask,
  not the fix. The re-ask starts from the shadow sampler's records: the raw
  evaluation's clearance of beta, the correction offset, the depth, and the
  reference outcome, from which a corrected margin is chosen on held-out
  positions before any games are played. Correcting the leaf and correcting the
  gates are then measured separately, and only then together. The pawn key the
  arm was built on landed on its own and stays.
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
  the same way was measured separately and rejected inside the same arm: it
  grew the tree and changed nothing the tactical suite could see.
- Tightening the reverse futility margin under a hundred centipawns a ply.
  The shadow lane prices a margin with no game played. A candidate row
  carries the evaluation's clearance of beta and the reference's answer at
  the node's own depth; a rule with margin `m` fires on that row exactly when
  the clearance is at least `m` times the depth, and whether the reference
  came back under beta does not depend on `m` at all. One run therefore
  scores every margin over the same rows. `residuals 7 every 1 cap 400000`
  keeps all 347,946 events of the bench's tree, and at the shipped margin the
  shadow rows reproduce the live gate depth by depth, 143,917 firings and 29
  crossings; that agreement is what says the offline reading is the live one.
  The margin turns out not to be spare. Its pooled crossing rate of 0.02%
  belongs to the three quarters of the candidates that clear beta by more
  than five pawns a ply and would fire at any margin at all. What a
  tightening buys is the band beside the boundary, and that band is the dear
  one. Over the three hundred held-out positions of `tactics.epd`, which
  `epd <file>` on the residuals argument exists to reach, the band from a
  hundred down to ninety five crosses at 0.47% and the band from a hundred
  down to ninety nine at 1.39%, against 0.04% over everything already firing.
  Two of the ten crossings those five points buy are forced mates against the
  side that would have cut off, one at depth four with the evaluation
  standing 399 above beta, which is a centipawn of margin between the
  shortcut and a lost position. The bench's own eighteen positions put the
  same band at 0.10% and its halves cannot resolve it at all, so the size of
  this number is read off the corpus and not off the bench. Downstream the
  two gates say the same. Nine margins from ninety one to ninety nine move
  the bench between 0.6% and 1.9% smaller with no order to the sizes, and the
  tactical count over the same nine runs 227 to 229, unordered as well: 227
  at ninety nine and 229 at ninety five. A count at any one of them is the
  tree being reshuffled rather than the search answering better. Under ninety
  one the depth four mate in two goes. Nothing here was played, and nothing
  here needs to be: there is no margin in the range to put in front of an
  sprt. Re-ask this with the correction the correction history arm is about,
  which is what would move the clearance the rows are read by; the margin
  against a raw evaluation is where it should be.
- The delta margin in quiescence, measured on its own. It landed in one pair
  with principal variation search, and the pair's +50 ±24 over 530 games at
  10+0.1 (sprt [0, 10] passed, PR #171) sits on the margin's commit. Turning
  the margin off against that master gave +7 ±17 over 840 games at 10+0.1
  (sprt [-10, 0] inconclusive at the time cap, final LLR 1.47, branch
  ablate/delta-off): the games could not see the margin at all, and the pair's
  gain is principal variation search's. The margin stays on its switch because
  it costs nothing measurable and the quiescence SEE prune is the form it
  converges on. One guess per run from here, unless two parts cannot be
  measured apart.
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
  wrong denominator. What prices a reduction is the nodes it removes, and
  reading a population share as a cost is the reverse futility entry's error
  in another shape. Measured against master at `ce8b662`, with the two
  halves of the floor asked apart. The deeper
  scout at depth three is quiescence, and it grows the bench tree by 0.17%
  while the strategic suite falls 813 points (92502 to 91689) and the
  tactical count rises two (221 to 223). The skip at depth three leaves the
  suites where they were (221, and 8 points down) and saves 0.70% at the
  bench's depth, 0.43% at eight, 0.13% at nine and -0.04% at ten: what it
  takes off the shallowest nodes is given back as the tree around them
  widens, so nothing of it is left at the depths a game reaches. Both
  together are 1.18% of the bench tree, 223 tactical and 91625 strategic.
  Re-ask only with a reason the depth three nodes have become expensive,
  which is a change to what a depth one search costs rather than a change to
  the reduction.
- Widening the late move pruning band, so that a late quiet the attention
  model prices at or under -6000 is skipped where the threshold stood at
  -7954. The offline reading was favourable and the games could not see it.
  What prices the move is the rate in the band the wider threshold newly
  reaches, since the moves already skipped are skipped either way and the
  moves the reduced scout writes off are written off either way. Over the
  fifteen hundred held-out positions of `strategy.epd` that band hands back
  0.114% of 18,442 rows for a full search (95% upper bound 0.163%), against
  the 0.16% at depth four, 0.21% at five and 0.46% at six that the reduction
  already gets wrong, so -6000 is the last point on the grid under all
  three. The tree came out 1.68% smaller at the bench's depth, and 14.6%
  smaller at depth nine over the sixteen bench positions the skip can reach.
  Two sprt batches at 10+0.1 against master at `ce8b662` (sprt [0, 10]) then
  put it at nothing: -13 ±24 over 500 games with the likelihood ratio at
  -1.18, and +1 ±17 over a further 1,000 at -0.47. The first batch's -13
  sits inside its own interval and the second contradicted it, so the number
  to read is the 1,500 games together, which are centred near zero. The
  batches ran from different seeds and neither carried the other's pairs in,
  so their ratios were added by hand rather than read as one continued test.
  That is weaker than a run that carries them, and -1.65 against a -2.94
  bound falls short of accepting H0 either way. The suites dissented from
  the start and were right to. The tactical count went 221 to 220 and the
  strategic total 92502 to 92456, both small enough to read as the tree
  being reshuffled, and neither suite is given the depth the smaller tree
  buys. The band reading was sound about what the skip costs in accuracy. It
  says nothing about what the nodes the skip saves are worth, and at this
  control they are worth nothing a game can see. Re-ask only at a control
  long enough for 1.7% of the tree to show, or with the skip moved to where
  it takes more than that. The `epd <file>` word on the reductions argument
  the band was read with landed on its own and stays.
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
  node.
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
  `inline(always)` moved on `Quiet::bonus` below. Only the sort's read
  carries it. The write in `cutoff` and the census read are cold and keep
  their check, which is the better failure for a square that cannot be out
  of range: a check panics where a mask reads a different square.
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
  one go.
- `#[inline(always)]` on `Board::shelter_counts`, which moved 127 instructions
  of 3.4 billion over the bench and is not carried. It is worth recording
  because the case for it is good and the measurement still says no: the
  function is read twice at every leaf and every quiescence node, which is
  where the attribute has paid elsewhere. It is small enough that llvm inlines
  it unasked. A larger counting helper read from the same place is a different
  question and is measured on its own.
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
