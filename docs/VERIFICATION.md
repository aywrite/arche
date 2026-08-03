# Verification

Perft proves the move generator right for the positions it enumerates and says
nothing about the rest. This is a plan for saying something about the rest.

[Kani](https://github.com/model-checking/kani) is a bounded model checker for
rust. A harness looks like a test, except that its inputs are symbolic: rather
than picking a value, `kani::any()` stands for every value at once, and
`kani::assume` narrows that down to the ones worth considering. If an assertion
can fail for any of them, kani says which. Where proptest samples the space,
kani covers it.

It is not a replacement for perft. Perft checks that the generator produces the
right *set* of moves, which is the property kani is worst at, because the answer
is a count over a tree rather than a fact about one function. Kani checks the
properties perft cannot see: that make and unmake agree, that the key stays in
step with the position, that the invariants the rest of the code assumes are
actually maintained.

## Running it

Kani brings its own toolchain, so it is not part of `cargo test` and does not
respect `rust-toolchain.toml`:

```
cargo install --locked kani-verifier
cargo kani setup                  # downloads about 500MB
cargo kani -Z stubbing -p basic_engine --harness verify::<name>
```

`-Z stubbing` is needed because the harnesses replace `square_attacked` with a
nondeterministic answer. Without that the magic tables are built inside the
proof, which is not tractable.

## What it costs

Measured while working out whether any of this was practical, on one machine, so
treat the numbers as orders of magnitude rather than as figures.

| harness | result |
| --- | --- |
| `pop_lsb`, over every `u64` | 0.9s |
| make/unmake, symbolic occupancy, lazy tables | killed at 30 minutes |
| make/unmake, one knight a side, lazy tables | killed at 25 minutes |
| make/unmake, one knight a side, constant tables | 108s, 4174 checks |

The first two lines were the interesting ones. Narrowing the position from two
symbolic occupancy words to one knight a side is a factor of ten fewer symbolic
bits and it changed nothing: the solver still did not finish. Taking
`lazy_static` off the path finished the same harness in under two minutes.

So the cost of a proof here is dominated by what it has to reason about before
it reaches the code under test, not by the size of the space it covers. That is
worth knowing before writing any more harnesses, and it is why the tables being
built at runtime is the first thing on the list below rather than the last.

## Properties

Roughly in the order they are worth doing. Each one is a separate harness so
that a failure says which property broke.

1. **Make and unmake are inverses.** For any position and any pseudo legal move,
   `make_move` followed by `undo_move` restores the board exactly, field for
   field, and `make_move` returning false has already done so. This is the
   foundation: every other property is stated about a board that survived it.
2. **The key follows the position.** After a move, the incrementally maintained
   `key` equals the key computed from the resulting position from scratch. This
   is the property the en passant issue in the readme's known issues violates,
   and it needs a `recompute_key` that walks the bitboards, which does not exist
   yet.
3. **The invariants hold.** The piece bitboards are pairwise disjoint, their
   union is the occupancy, each side has exactly one king, no pawn is on the
   first or eighth rank. Establishing these as preserved by make/unmake is what
   lets the other harnesses assume them, and it is the same class of bug as a
   fen without a king being accepted and then panicking.
4. **Magic indexing is in bounds.** For every square and every occupancy the
   computed index lands inside that square's table. Provable without building
   the table, if the offsets and shifts are constants.
5. **Magic lookup equals a naive ray walk.** For every square and every
   occupancy, the table returns what walking outwards square by square would
   have returned. This is the one that retires the stub in property 1, and it is
   the most expensive.

Magic collision freedom is deliberately not on the list. Each square has at most
4096 blocker subsets, so enumerating all of them is already exhaustive: an
ordinary `#[test]` proves it, and does so faster than a model checker would.
Reach for kani where the input space is too large to enumerate, not where it
happens to be fashionable.

## What has to change to make this practical

The obstacles are all in the shape of the code rather than in the properties,
and each fix is worth having anyway. In the order the measurements say they
matter.

- **The tables are built at runtime.** `MAGIC`, `ZORB`, `PVT`, `ATTACK_MASKS`
  and `BASE_CONVERSIONS` are all `lazy_static`, so a proof that touches
  `set_piece_index` has to reason about a `Once`, an atomic and a prng seeding
  768 numbers before it gets to the first line that matters. This is the whole
  difference between a harness that finishes and one that does not.

  `ZORB` and `PVT` are constants under `cfg(kani)` as a stopgap, which is only
  sound for harnesses that are not about the key. Computing them at build time
  instead would make them constants for everyone, let the key harnesses have
  the real numbers, remove the `lazy_static` dependency, and cut the startup
  that `-startup-ms 20000` in the development notes exists to accommodate.
- **`Board` is 32,896 bytes, and it is `Copy`.** Almost all of that is
  `history`, a `[Option<PlayState>; 1024]` stored inline. A proof that compares
  a board before and after a move compares that array too. `MAX_GAME_SIZE` is
  cut down under `cfg(kani)` for now, but the real fix is to take the history
  out of `Board` and give it to whoever is making the moves, which also fixes
  the known issue where a long game runs off the end of it. `pv_line` copies a
  whole board on purpose and relies on `Copy`, so it has to be part of that
  change rather than an afterthought.
- **`make_move` has no written precondition.** It cannot be given an arbitrary
  move: it assumes the from square is occupied by the side to move, that
  `capture` names whatever is actually on the to square, that `castle` is only
  set for a king going two squares. Those assumptions have to be written down as
  a predicate before anything can be proved, and the predicate is worth having
  in its own right. `pv_line` already needs exactly it and gets it the expensive
  way, by generating every move and asking whether the stored one is among them.

## Next

1. Widen the make/unmake harness one special case at a time, each as its own
   harness: captures are covered, then promotions, then en passant, then
   castling. A separate harness per case is what makes a failure say which case
   broke.
2. Write the precondition as `is_pseudo_legal`, use it as the single assumption
   in place of the hand written ones, and call it from `pv_line`.
3. Move the tables to build time and drop the `cfg(kani)` constants.
4. Add `recompute_key` and prove property 2, which is the first proof that says
   something the tests do not already sample.
5. Only then consider ci. The tables and the toolchain download make this a
   scheduled job rather than something a pull request waits for, and a proof
   that takes two minutes today will not stay at two minutes as the harnesses
   widen.

## Vacuity

A harness whose assumptions contradict each other passes without checking
anything, and passes quickly, which makes it look like the best harness in the
suite. Every harness therefore has to show that both outcomes are still
reachable, with `kani::cover!` on the legal and the illegal branch. An
unreachable cover is a bug in the harness, not a proof.
