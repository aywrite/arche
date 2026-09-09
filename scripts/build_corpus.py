#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Build the tuning corpus from archived strength-run pgns.

Every strength run keeps its games as an artifact, so the archive grows on its
own, and this turns a pile of those pgns into the epd `arche terms` reads:

    python3 scripts/build_corpus.py runs/*/games.pgn --out corpus.epd

One line per unique position, carrying the game result from the side to move's
point of view and how many games the position appeared in. A position that
appeared in more than one game carries the mean of its results, which is a
small correction (duplicates run at a few per cent of the plies) and an exact
one.

The book is dropped. The first sixteen plies are the opening book's, and so is
any further ply whose comment says `book`, so the corpus starts where the book
stops and its opening variety is the book's rather than the engine's. Games
that did not end normally are dropped whole: a crash or a stall labels the
positions before it with a result the play did not earn.

The known caveat, which this does not fix and does not hide. These are the
engine's own games, so the positions it never reaches are unlabelled and the
mistakes it makes on both sides of the board are labelled as if they were
normal play. Mixing in positions from stronger engines' games would answer a
different question, and a loss change measured on a corpus of two sources
cannot be attributed to either, so this reads one source and says which.

Positions are deduplicated by the four-field epd, which is what the engine
reads back: pieces, side to move, castling and en passant, with no clocks. Two
games reaching the same diagram by different move orders are one row.
"""

import argparse
import collections
import sys
from pathlib import Path

import chess
import chess.pgn

# The opening book's plies, which every run plays out of the same book. Eight
# full moves.
BOOK_PLIES = 16

# What a comment says on a move the book played rather than the engine.
BOOK_COMMENT = "book"

# Words a fastchess comment uses for a game that ended in something other than
# play. The header is checked first; these catch the runs whose header says
# normal and whose last comment does not.
CRASH_WORDS = (
    "disconnect",
    "stall",
    "illegal",
    "abandon",
    "unterminated",
    "interrupt",
    "crash",
    "forfeit",
    "timeout",
)

# The result from white's point of view. Anything else is a game with no
# result to label with, and it is dropped.
RESULTS = {"1-0": 1.0, "0-1": 0.0, "1/2-1/2": 0.5}


class Entry:
    """One unique position: what to call it, what its games said, and how many
    of them there were."""

    def __init__(self, identifier):
        self.id = identifier
        self.results = []

    @property
    def count(self):
        return len(self.results)

    @property
    def result(self):
        return sum(self.results) / len(self.results)


def crashed(game, comments):
    """Whether the game ended in something other than play."""
    if game.headers.get("Termination", "normal").lower() != "normal":
        return True
    joined = " ".join(comments).lower()
    return any(word in joined for word in CRASH_WORDS)


def plies_of(game):
    """The mainline, as (ply index, the board before the move, the comment on
    the move). The board before, so every position has a move to make."""
    board = game.board()
    for ply, node in enumerate(game.mainline()):
        yield ply, board.copy(stack=False), node.comment or ""
        board.push(node.move)


def result_for(white_result, turn):
    """The result from the side to move's point of view."""
    return white_result if turn == chess.WHITE else 1.0 - white_result


def corpus(games, book_plies=BOOK_PLIES):
    """The unique post-book positions of the games, in the order they were
    first seen, with what each one's games said about it.

    Returns the entries and the counts a caller reports: games read, games
    dropped, plies seen and plies past the book.
    """
    entries = collections.OrderedDict()
    read = dropped = plies = post_book = 0
    for game in games:
        read += 1
        mainline = list(plies_of(game))
        plies += len(mainline)
        white_result = RESULTS.get(game.headers.get("Result", "*"))
        if white_result is None or crashed(game, [c for _, _, c in mainline]):
            dropped += 1
            continue
        for ply, board, comment in mainline:
            if ply < book_plies or comment.strip() == BOOK_COMMENT:
                continue
            post_book += 1
            epd = board.epd()
            if epd not in entries:
                entries[epd] = Entry(f"g{read:05d}p{ply:03d}")
            entries[epd].results.append(result_for(white_result, board.turn))
    counts = {
        "games": read,
        "dropped": dropped,
        "plies": plies,
        "post_book": post_book,
        "positions": len(entries),
    }
    return entries, counts


def render(entries):
    """The epd the engine reads: the four-field position, a name, the result
    and how many games it came from.

    The result is written to four places. It is a mean over a handful of games
    at most, and a place further would be spelling out a repeating decimal.
    """
    lines = []
    for epd, entry in entries.items():
        lines.append(
            f'{epd} id "{entry.id}"; result "{entry.result:.4f}"; count "{entry.count}";'
        )
    return "\n".join(lines) + "\n"


def games_of(paths):
    """Every game in the pgn files named, in the order the files were given.

    A directory stands for the `games.pgn` inside it, which is how a strength
    run's artifact is laid out.
    """
    for path in paths:
        path = Path(path)
        if path.is_dir():
            path = path / "games.pgn"
        with path.open(encoding="utf-8", errors="replace") as handle:
            while True:
                game = chess.pgn.read_game(handle)
                if game is None:
                    break
                yield game


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "pgn", nargs="+", help="pgn files, or directories holding games.pgn"
    )
    parser.add_argument("--out", required=True, help="where to write the epd")
    parser.add_argument(
        "--book-plies",
        type=int,
        default=BOOK_PLIES,
        help="plies to drop off the front of each game (default %(default)s)",
    )
    args = parser.parse_args(argv)

    entries, counts = corpus(games_of(args.pgn), args.book_plies)
    if not entries:
        print("build_corpus.py: the pgns hold no post-book position", file=sys.stderr)
        return 1
    Path(args.out).write_text(render(entries), encoding="utf-8", newline="\n")
    print(
        "corpus games {games} dropped {dropped} plies {plies} "
        "post_book {post_book} positions {positions}".format(**counts)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
