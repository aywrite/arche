# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Print the opening a shard of a match starts at.

The shards of a match play at the same time and are pooled afterwards, so no
two of them may play the same opening: a position played twice would be counted
twice and the games would not be the independent sample the estimate reads them
as. The book is taken in order rather than drawn, and each shard is given a
slice of its own, which is where its games begin.

The run's first shard starts at a remainder of the seed, so two runs of the same
size play different regions of a book far larger than either of them uses, and
each shard after it starts a slice further along. The starts are worked out from
the seed and the counts alone, so the manifest is enough to play the schedule
again, without depending on how fastchess draws its own openings.

The index printed is one based, which is what fastchess's `start=` takes.
"""

import argparse
import sys


def first_opening(openings: int, wanted: int, seed: int) -> int:
    """Where the first shard begins. The offset leaves room for every shard, so
    the last one still ends inside the book."""
    room = openings - wanted
    if room < 1:
        return 1
    return 1 + seed % room


def start(openings: int, pairs: int, shards: int, shard: int, seed: int) -> int:
    """The opening this shard begins at, one based."""
    return first_opening(openings, pairs * shards, seed) + shard * pairs


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--openings", type=int, required=True, help="what the book has")
    parser.add_argument("--pairs", type=int, required=True, help="openings per shard")
    parser.add_argument("--shards", type=int, required=True, help="shards in the run")
    parser.add_argument("--shard", type=int, required=True, help="which one, from zero")
    parser.add_argument("--seed", type=int, required=True, help="the run's seed")
    args = parser.parse_args()

    if min(args.openings, args.pairs, args.shards) < 1 or args.seed < 0:
        sys.exit("the counts are all at least one and the seed is not negative")
    if not 0 <= args.shard < args.shards:
        sys.exit(f"shard {args.shard} is not one of {args.shards}")

    wanted = args.pairs * args.shards
    if args.openings - wanted < 1:
        # fastchess reads on around the end of the book, so the match still
        # plays. It is the shards no longer being disjoint that is worth saying
        print(
            f"the book holds {args.openings} openings and the run wants {wanted},"
            " so the shards repeat each other",
            file=sys.stderr,
        )
    print(start(args.openings, args.pairs, args.shards, args.shard, args.seed))


if __name__ == "__main__":
    main()
