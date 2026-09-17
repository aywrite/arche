#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Build the tuning corpus from archived strength-run pgns.

Turns the pgns the strength runs keep as artifacts into the epd `arche terms`
reads:

    python3 scripts/build_corpus.py runs/*/games.pgn --out corpus.epd

One line per unique position (the four-field epd, so two games reaching the
same diagram by different move orders are one row), carrying the game it
belongs to, the run and round that game was played in, the result from the
side to move's point of view, and how many times it appeared. The result is
the mean over its games and the count is the weight the loss reads it at.

A game's key is the sha256 of its movetext, so a re-extraction of the same
archive gives the same keys. `scripts/tune.py` splits on the pair: a strength
run plays every opening twice with the colours reversed, and split by the game
the two halves land in one group only eleven times in twenty five. The pair
key is the sha256 of the two games' keys sorted and joined by a space. A game
with no partner (no round named, or the shard's clock stopped after the first
game) is a pair of one and its pair key is its own.

A position two games reached belongs to the group of the lower key, and its
result and count are taken from that group's games alone; appearances in
other groups are dropped rather than merged, since merging would carry a
sealed game's result into a training label and the reverse. The counters say
how many appearances that dropped. Two games can key alike (the same game
archived twice, or two played move for move the same), and the counters say
how many do.

The run is read off the `manifest.txt` beside `games.pgn` as the run id and
the shard, or off the directory's name where there is no manifest; the round
is the pgn's `Round` header. Both are carried beside the key rather than
folded into it, so a row can be excluded or weighted by its source.

The first sixteen plies are the opening book's, and so is any further ply
whose comment says `book`, so the corpus starts where the book stops. Games
that did not end normally are dropped whole: a crash or a stall labels the
positions before it with a result the play did not earn.

The known caveat: these are the engine's own games, so the positions it never
reaches are unlabelled and its mistakes on both sides are labelled as normal
play. Mixing in another engine's games would answer a different question, and
a loss change on a corpus of two sources cannot be attributed to either.
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

# The opening book's plies: eight full moves.
BOOK_PLIES = 16

# What a comment says on a move the book played rather than the engine.
BOOK_COMMENT = "book"

# Words a fastchess comment uses for a game that ended in something other than
# play, for the runs whose Termination header says normal and whose last
# comment does not.
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

# The result from white's point of view. A game with any other result is
# dropped.
RESULTS = {"1-0": 1.0, "0-1": 0.0, "1/2-1/2": 0.5}

# A round the archive did not name, or a run a caller of `corpus` did not.
UNKNOWN = "-"

# The file a strength run keeps beside its games, and the two lines of it that
# name the run: `run_id: 34468958876` and `shard: 0`.
MANIFEST = "manifest.txt"
MANIFEST_LINE = re.compile(r"^(run_id|shard):\s*(\S+)\s*$")

# A game with the run that played it, which the game alone does not know.
Sourced = collections.namedtuple("Sourced", "run game")


def game_key(game):
    """The sha256 of the game's movetext, as uci moves joined by spaces, so a
    pgn another tool re-exported keys the same."""
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
    it said about it. The lowest key of those games owns it, that game's pair
    says which group, and it is labelled and weighted by that group's
    appearances alone.
    """

    def __init__(self, identifier, key, result, played, sealed=None):
        self.id = identifier
        self.appearances = [(key, result)]
        # the table of every game read, by key, shared with every entry
        self._played = played
        # the pairs the caller sealed, or none to draw the seal from the key
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
        """What the games of its own group said."""
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
    the move)."""
    board = game.board()
    for ply, node in enumerate(game.mainline()):
        yield ply, board.copy(stack=False), node.comment or ""
        board.push(node.move)


def result_for(white_result, turn):
    """The result from the side to move's point of view."""
    return white_result if turn == chess.WHITE else 1.0 - white_result


def pair_key(keys):
    """The sha256 of the games' keys, sorted and joined by a space, or a game's
    own key when it has no partner."""
    keys = sorted(set(keys))
    if len(keys) == 1:
        return keys[0]
    return hashlib.sha256(" ".join(keys).encode("utf-8")).hexdigest()


def pair_up(played):
    """Fill in every game's pair: the games of one run and one round, and a
    game with no round or no partner is a pair of one. A round of more than
    two games is one pair of all of them, since what the pair holds together
    is the opening. Returns how many pairs of more than one game there were
    and how many games stood alone."""
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
    """The round as the pgn names it. python-chess reads a missing Round
    header back as a question mark, which is no round either."""
    found = game.headers.get("Round", "").strip()
    return UNKNOWN if found in ("", "?") else found


def corpus(sourced, book_plies=BOOK_PLIES, sealed=None):
    """The unique post-book positions of the games, in the order they were
    first seen, with what each one's games said about it.

    Returns the entries and the counts a caller reports: runs read, games
    read, games dropped, plies seen, plies past the book, pairs of more than
    one game and games that stood alone, positions more than one game reached,
    positions reached by games in more than one group and the appearances that
    dropped, and games that key alike with one already read. The last is there
    because a repeated game looks the same as a repeated position in every
    other number.
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
                entries[epd] = Entry(
                    f"g{read:05d}p{ply:03d}", key, result, played, sealed
                )
            else:
                entry.seen(key, result)
    # the pairs are known only once every game is read; nothing above asked
    # which group a game is in
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

    The name says where the position was first seen; the `game`, `pair`, `run`
    and `round` operands are the owning game's (the lowest key), which on a
    position two games reached may be a different game. The result is written
    to four places, being a mean over a handful of games at most.
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
    """The run that played the games in a pgn: the run id and the shard off
    the manifest beside it, the run id alone where the manifest names no
    shard, the directory's name where there is no manifest, and the file's own
    name where there is no directory either."""
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
    each with the run that played it. A directory stands for the `games.pgn`
    inside it, which is how a strength run's artifact is laid out."""
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
