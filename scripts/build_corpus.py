#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Build the tuning corpus from archived strength-run pgns.

Every strength run keeps its games as an artifact, so the archive grows on its
own, and this turns a pile of those pgns into the epd `arche terms` reads:

    python3 scripts/build_corpus.py runs/*/games.pgn --out corpus.epd

One line per unique position, carrying the game it belongs to, the run and the
round that game was played in, the result from the side to move's point of
view, and how many times it appeared. The result is the mean of what its games
did and the count is the weight the loss reads the position at.

The game a line names is a key rather than a name: the sha256 of the game's
movetext. It is the movetext's alone, so a re-extraction of the same archive
gives the same keys and nothing has to be written down outside the pgn.

What `scripts/tune.py` splits on is the pair. A strength run plays every
opening twice with the colours reversed, and the two games are one opening's
evidence: they share their first moves, and their results lean against each
other. Split by the game they land in one group eleven times in twenty five,
which is the chance two keys agree under the split's shares of three fifths,
a fifth and a fifth, so two groups could hold the two halves of one opening
and neither would know. The pair key is the sha256 of the two games' keys
sorted and joined by a space, so it is the movetexts' alone, and the same
whichever of the two the archive lists first. A game with no partner, because
the archive named no round or the shard's clock stopped after the first game
of a pair, is a pair of one and its pair key is its own. The counters report
the pairs and the games that stood alone.

A position two games reached belongs to the group of the lower key, and its
result and its count are taken from that group's games alone. The appearances
in other groups are dropped rather than merged. Merging them would put a
calibration game's result into a training row's label and a training game's
into a sealed row's, so the group a coverage claim is made on would have been
read after all, which is the only thing that group is for. Dropping costs a
handful of appearances and slightly under-weights the positions common enough
to recur, and the counters say how many of each. A position whose every
appearance is in one group, which is nearly all of them, is unaffected.

Two games can key alike, and the counters say how many do. A key on the play
cannot tell the same game archived twice from two games played move for move
the same, and either way the archive is holding one game's evidence twice. The
count is what makes that visible. A key on the game's name hid it.

The run and the round are where the game came from rather than what it is, so
they are carried beside the key and not folded into it. The run is read off the
`manifest.txt` a strength run keeps beside its `games.pgn`, as the run id and
the shard, and off the directory's name where there is no manifest; the round
is the pgn's own `Round` header. A row that names them can be excluded or
weighted by its source after extraction, which a row naming only its game
could not, and the round is what says which two games played one opening with
the colours reversed.

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
import re
import sys
from pathlib import Path

import chess
import chess.pgn
from groups import group_of, sealed_pairs

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

# What a game's row says for a round the archive did not name, and what
# `corpus` is handed for the run by a caller that has none. A pgn read from
# the archive always has a run: the manifest's, or the directory's name.
UNKNOWN = "-"

# The file a strength run keeps beside its games, and the two lines of it that
# name the run: `run_id: 34468958876` and `shard: 0`.
MANIFEST = "manifest.txt"
MANIFEST_LINE = re.compile(r"^(run_id|shard):\s*(\S+)\s*$")

# The run and the game as read from the archive, which is what `corpus` is
# handed: a game on its own does not know which run played it.
Sourced = collections.namedtuple("Sourced", "run game")


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


class Played:
    """Where one game came from: the run and the round it was played in, and
    the pair it belongs to, which is filled in once every game is read."""

    def __init__(self, run, round_):
        self.run = run
        self.round = round_
        self.pair = None


class Entry:
    """One unique position: what to call it, and what every game that reached
    it said about it.

    The position belongs to one group and is labelled by that group alone. The
    lowest key of the games that reached it says which game owns it, that
    game's pair says which group, which is what keeps a repeated position out
    of two groups at once, and the results of the games in that group are what
    it is labelled and weighted by. The appearances in other groups are
    dropped: a label that meaned them would carry a sealed game's result into a
    row the fit reads, and a training game's into a row that is meant to be
    unread.
    """

    def __init__(self, identifier, key, result, played, sealed=None):
        self.id = identifier
        self.appearances = [(key, result)]
        # the table of every game read, by key, shared with every entry
        self._played = played
        # the pairs the caller sealed, or none to draw the seal from the key.
        # Shared with every entry the way the table above is
        self._sealed = sealed

    def seen(self, key, result):
        """Another game reaching this position."""
        self.appearances.append((key, result))

    @property
    def key(self):
        """The game the position belongs to: the lowest key that reached it."""
        return min(key for key, _ in self.appearances)

    @property
    def played(self):
        """Where the game the position belongs to came from."""
        return self._played[self.key]

    def group_of(self, key):
        """The group a game is in, which is its pair's."""
        return group_of(self._played[key].pair, self._sealed)

    @property
    def results(self):
        """What the games of its own group said, which is the whole of what it
        is labelled and weighted by."""
        group = self.group_of(self.key)
        return [
            result for key, result in self.appearances if self.group_of(key) == group
        ]

    @property
    def count(self):
        return len(self.results)

    @property
    def result(self):
        return sum(self.results) / len(self.results)

    @property
    def dropped(self):
        """The appearances in other groups, which are not merged in."""
        return len(self.appearances) - len(self.results)


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


def pair_key(keys):
    """The name of a pair: the sha256 of its games' keys, sorted and joined by
    a space. The same whichever game the archive listed first, and a game's own
    key when it has no partner."""
    keys = sorted(set(keys))
    if len(keys) == 1:
        return keys[0]
    return hashlib.sha256(" ".join(keys).encode("utf-8")).hexdigest()


def pair_up(played):
    """Fill in every game's pair: the games of one run and one round are a
    pair, and a game with no round or no partner is a pair of one. A round of
    more than two games, which a run asked for more than two games an opening
    would produce, is one pair of all of them, since what the pair holds
    together is the opening. Returns how many pairs of more than one game
    there were and how many games stood alone."""
    rounds = collections.defaultdict(set)
    for key, game in played.items():
        if game.round != UNKNOWN:
            rounds[game.run, game.round].add(key)
    pairs, unpaired = set(), 0
    for key, game in played.items():
        partners = rounds.get((game.run, game.round), {key})
        game.pair = pair_key(partners)
        if len(partners) == 1:
            unpaired += 1
        else:
            pairs.add(game.pair)
    return len(pairs), unpaired


def round_of(game):
    """The round the game was played in, as the pgn names it. A pgn with no
    Round header reads back as a question mark, which is no round either."""
    found = game.headers.get("Round", "").strip()
    return UNKNOWN if found in ("", "?") else found


def corpus(sourced, book_plies=BOOK_PLIES, sealed=None):
    """The unique post-book positions of the games, in the order they were
    first seen, with what each one's games said about it. Each game arrives
    with the run that played it, which `games_of` reads off the archive.

    Returns the entries and the counts a caller reports: runs read, games read,
    games dropped, plies seen, plies past the book, how many pairs of more than
    one game the rounds made and how many games stood alone, how many of the
    positions more than one game reached, how many of those were reached by
    games in more than one group and how many appearances that dropped, and how
    many games key alike with one already read.

    The dropped appearances are what the group-local label costs, so the run
    says how many rather than leaving a reader to work it out from the plies.
    The last count is there because a repeated position and a repeated game
    look the same in every other number. A game whose movetext another game
    already had is one game's evidence counted twice, and nothing else in the
    run says so.
    """
    entries = collections.OrderedDict()
    keys = collections.Counter()
    played = {}
    runs = set()
    read = dropped = plies = post_book = 0
    for run, game in sourced:
        read += 1
        runs.add(run)
        mainline = list(plies_of(game))
        plies += len(mainline)
        white_result = RESULTS.get(game.headers.get("Result", "*"))
        if white_result is None or crashed(game, [c for _, _, c in mainline]):
            dropped += 1
            continue
        key = game_key(game)
        keys[key] += 1
        # a game the archive holds twice keeps the first place it was seen
        played.setdefault(key, Played(run, round_of(game)))
        for ply, board, comment in mainline:
            if ply < book_plies or comment.strip() == BOOK_COMMENT:
                continue
            post_book += 1
            epd = board.epd()
            result = result_for(white_result, board.turn)
            entry = entries.get(epd)
            if entry is None:
                entries[epd] = Entry(f"g{read:05d}p{ply:03d}", key, result, played)
            else:
                entry.seen(key, result)
    # the pairs are known only once every game is read, and nothing above
    # asked which group a game is in
    pairs, unpaired = pair_up(played)
    counts = {
        "runs": len(runs),
        "games": read,
        "dropped": dropped,
        "plies": plies,
        "post_book": post_book,
        "pairs": pairs,
        "unpaired": unpaired,
        "positions": len(entries),
        "repeated": sum(1 for entry in entries.values() if len(entry.appearances) > 1),
        "straddled": sum(1 for entry in entries.values() if entry.dropped),
        "dropped_appearances": sum(entry.dropped for entry in entries.values()),
        "same_key": sum(seen - 1 for seen in keys.values()),
    }
    return entries, counts


def render(entries):
    """The epd the engine reads: the four-field position, a name, the game it
    belongs to and where that game was played, the result and how many times
    it was reached.

    The name says where the position was first seen, which is a game and a ply
    a reader can go back to. The game it belongs to is the `game` operand, and
    on a position two games reached the two disagree: the name is the first
    game, which may be one of the games the label drops, and the operand is the
    lowest key. The `pair` operand is that game's pair, which is what the split
    reads. The `run` and `round` operands are that game's as well, so they say
    where the label came from and not where the position was first seen.

    The result is written to four places. It is a mean over a handful of games
    at most, and a place further would be spelling out a repeating decimal.
    """
    lines = []
    for epd, entry in entries.items():
        lines.append(
            f'{epd} id "{entry.id}"; game "{entry.key}"; '
            f'pair "{entry.played.pair}"; '
            f'run "{entry.played.run}"; round "{entry.played.round}"; '
            f'result "{entry.result:.4f}"; count "{entry.count}";'
        )
    return "\n".join(lines) + "\n"


def run_of(path):
    """The run that played the games in a pgn: the run id and the shard off the
    manifest beside it, the run id alone where the manifest names no shard,
    the directory's name where there is no manifest, and the file's own name
    where there is no directory to speak of either."""
    manifest = path.parent / MANIFEST
    if manifest.is_file():
        found = {}
        for line in manifest.read_text(encoding="utf-8", errors="replace").splitlines():
            match = MANIFEST_LINE.match(line)
            if match:
                found[match.group(1)] = match.group(2)
        if "run_id" in found:
            # a run before the workflow was sharded names no shard
            return "-".join(
                found[word] for word in ("run_id", "shard") if word in found
            )
    return path.parent.name or path.stem


def games_of(paths):
    """Every game in the pgn files named, in the order the files were given,
    each with the run that played it.

    A directory stands for the `games.pgn` inside it, which is how a strength
    run's artifact is laid out.
    """
    for path in paths:
        path = Path(path)
        if path.is_dir():
            path = path / "games.pgn"
        run = run_of(path)
        with path.open(encoding="utf-8", errors="replace") as handle:
            while True:
                game = chess.pgn.read_game(handle)
                if game is None:
                    break
                yield Sourced(run, game)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "pgn", nargs="+", help="pgn files, or directories holding games.pgn"
    )
    parser.add_argument("--out", required=True, help="where to write the epd")
    parser.add_argument(
        "--sealed",
        help="a file naming the sealed pairs, one key to a line; without it "
        "the sealed group is drawn from the keys",
    )
    parser.add_argument(
        "--book-plies",
        type=int,
        default=BOOK_PLIES,
        help="plies to drop off the front of each game (default %(default)s)",
    )
    args = parser.parse_args(argv)

    sealed = sealed_pairs(args.sealed) if args.sealed else None
    entries, counts = corpus(games_of(args.pgn), args.book_plies, sealed)
    if not entries:
        print("build_corpus.py: the pgns hold no post-book position", file=sys.stderr)
        return 1
    Path(args.out).write_text(render(entries), encoding="utf-8", newline="\n")
    print(
        "corpus runs {runs} games {games} dropped {dropped} plies {plies} "
        "post_book {post_book} pairs {pairs} unpaired {unpaired} "
        "positions {positions} repeated {repeated} "
        "straddled {straddled} dropped_appearances {dropped_appearances} "
        "same_key {same_key}".format(**counts)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
