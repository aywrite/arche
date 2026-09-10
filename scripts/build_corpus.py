#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Build the tuning corpus from archived strength-run pgns.

Every strength run keeps its games as an artifact, so the archive grows on its
own, and this turns a pile of those pgns into the epd `arche terms` reads:

    python3 scripts/build_corpus.py runs/*/games.pgn --out corpus.epd

One line per unique position, carrying the game it belongs to, the result from
the side to move's point of view, and how many times it appeared. A position
that appeared in more than one game carries the mean of its results, which is a
small correction (duplicates run at a few per cent of the plies) and an exact
one, and the count is the weight the loss reads the position at.

The game a line names is a key rather than a name: the sha256 of the game's
movetext. `scripts/tune.py` splits on it, because the label is the game's and
not the position's, and it is the movetext's alone, so a re-extraction of the
same archive gives the same keys and nothing has to be written down outside the
pgn. A position two games reached belongs to the lower of their keys, so a
position that repeats lands in one group rather than straddling two.

Two games can key alike, and the counters say how many do. A key on the play
cannot tell the same game archived twice from two games played move for move
the same, and either way the archive is holding one game's evidence twice. The
count is what makes that visible. A key on the game's name hid it.

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
import hashlib
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


def game_key(game):
    """The name of a game: the sha256 of its movetext.

    The moves are taken as uci and joined by spaces, which is the movetext with
    the notation's choices and the clock comments out of it, so a pgn another
    tool re-exported keys the same. The key is a property of the play and of
    nothing else. That is what makes it stable across a re-extraction: the same
    game read again is the same key, whatever order the archive's files are
    given in and whatever else the archive has grown since.
    """
    movetext = " ".join(move.uci() for move in game.mainline_moves())
    return hashlib.sha256(movetext.encode("utf-8")).hexdigest()


class Entry:
    """One unique position: what to call it, which game it belongs to, what its
    games said, and how many times it was reached."""

    def __init__(self, identifier, key):
        self.id = identifier
        self.key = key
        self.results = []

    def seen(self, key, result):
        """Another game reaching this position.

        The lowest key owns it. Grouping by the game would otherwise lose the
        property that a repeated position cannot straddle the split, because
        the games that reached it can fall in different groups. The weight
        counts every appearance whichever game it was.
        """
        self.key = min(self.key, key)
        self.results.append(result)

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
    dropped, plies seen, plies past the book, how many of the positions more
    than one game reached, and how many games key alike with one already read.

    The last is there because a repeated position and a repeated game look the
    same in every other number. A game whose movetext another game already had
    is one game's evidence counted twice, and nothing else in the run says so.
    """
    entries = collections.OrderedDict()
    keys = collections.Counter()
    read = dropped = plies = post_book = 0
    for game in games:
        read += 1
        mainline = list(plies_of(game))
        plies += len(mainline)
        white_result = RESULTS.get(game.headers.get("Result", "*"))
        if white_result is None or crashed(game, [c for _, _, c in mainline]):
            dropped += 1
            continue
        key = game_key(game)
        keys[key] += 1
        for ply, board, comment in mainline:
            if ply < book_plies or comment.strip() == BOOK_COMMENT:
                continue
            post_book += 1
            epd = board.epd()
            if epd not in entries:
                entries[epd] = Entry(f"g{read:05d}p{ply:03d}", key)
            entries[epd].seen(key, result_for(white_result, board.turn))
    counts = {
        "games": read,
        "dropped": dropped,
        "plies": plies,
        "post_book": post_book,
        "positions": len(entries),
        "repeated": sum(1 for entry in entries.values() if entry.count > 1),
        "same_key": sum(seen - 1 for seen in keys.values()),
    }
    return entries, counts


def render(entries):
    """The epd the engine reads: the four-field position, a name, the game it
    belongs to, the result and how many times it was reached.

    The name says where the position was first seen, which is a game and a ply
    a reader can go back to. The game it belongs to is the `game` operand, and
    on a position two games reached the two disagree: the name is the first
    game and the operand is the lowest key. The split reads the operand.

    The result is written to four places. It is a mean over a handful of games
    at most, and a place further would be spelling out a repeating decimal.
    """
    lines = []
    for epd, entry in entries.items():
        lines.append(
            f'{epd} id "{entry.id}"; game "{entry.key}"; '
            f'result "{entry.result:.4f}"; count "{entry.count}";'
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
        "post_book {post_book} positions {positions} repeated {repeated} "
        "same_key {same_key}".format(**counts)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
