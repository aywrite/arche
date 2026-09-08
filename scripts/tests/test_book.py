# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the table of opening books a match can be played on.

Fetching one needs the network and fifteen megabytes, which a test has
neither of, so what is checked here is the table itself: that every book it
names is described completely, that a book it does not name is refused rather
than fetched, that counting openings gives the right answer for each of the
two formats, and that the default of each workflow names a book the table
knows. The last of those is the one that would otherwise go unnoticed, since
a default naming a book with no block fails a run rather than a check.

The pin is checked too. It is written out a second time in the match-tools
cache key, because a cache key is wanted before a step has run, and the two
saying different things would mean a run restoring the file fetched at the
old pin under the new one's name.
"""

import re
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "book.sh"
ACTION = ROOT / ".github" / "actions" / "match-tools" / "action.yml"
WORKFLOWS = [
    ROOT / ".github" / "workflows" / "strength.yml",
    ROOT / ".github" / "workflows" / "calibrate.yml",
]

pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)


def book(*args):
    return subprocess.run(
        [str(SCRIPT), *args], cwd=ROOT, check=False, capture_output=True, text=True
    )


def books() -> list[str]:
    listed = book("list")
    assert listed.returncode == 0, listed.stderr
    return listed.stdout.split()


def field(array: str, name: str) -> str:
    """A field of a block that no command prints, read from the table itself."""
    read = subprocess.run(
        [
            "bash",
            "-c",
            f'source "$1" list > /dev/null; echo "${{{array}[$2]-}}"',
            "book",
            str(SCRIPT),
            name,
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    assert read.returncode == 0, read.stderr
    return read.stdout.strip()


def pin() -> str:
    """The commit of the books repository every book is fetched at."""
    read = subprocess.run(
        [
            "bash",
            "-c",
            'source "$1" list > /dev/null; echo "$PIN"',
            "book",
            str(SCRIPT),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    assert read.returncode == 0, read.stderr
    return read.stdout.strip()


def defaults(workflow: Path) -> list[str]:
    read = yaml.safe_load(workflow.read_text(encoding="utf-8"))
    # yaml reads a bare on: as a boolean, so the triggers are under True
    return [trigger["inputs"]["book"]["default"] for trigger in read[True].values()]


def test_the_table_names_both_books():
    assert books() == ["8moves_v3", "UHO_4060_v2"]


def test_every_book_named_says_what_it_plays_as():
    for name in books():
        played = book("file", name)
        read = book("format", name)
        assert played.returncode == 0, played.stderr
        assert read.returncode == 0, read.stderr
        # the format is the suffix, which is what fastchess is told twice: the
        # file it opens and the format it reads it in
        assert read.stdout.strip() in ("pgn", "epd"), read.stdout
        assert played.stdout.strip().endswith("." + read.stdout.strip())


def test_every_book_named_is_described_completely():
    # a book missing one of these is not in the list at all, so this is what
    # the list is asserting as much as it is a check on the blocks
    for name in books():
        assert field("OPENINGS", name).isdigit(), name
        assert re.fullmatch(r"[0-9a-f]{64}", field("SHA256", name)), name


def test_a_book_the_table_does_not_know_is_refused(tmp_path):
    for asked in (
        book("file", "UHO_Lichess_4852_v1"),
        book("format", "UHO_Lichess_4852_v1"),
        book("count", "UHO_Lichess_4852_v1", str(tmp_path / "nothing")),
        book("fetch", "UHO_Lichess_4852_v1", str(tmp_path)),
    ):
        assert asked.returncode != 0
        assert "UHO_Lichess_4852_v1" in asked.stderr
    # refused rather than attempted: nothing was fetched
    assert not list(tmp_path.iterdir())


def test_a_name_that_is_not_a_name_is_refused(tmp_path):
    # an associative array reads @ and * as every key rather than as a name
    # nobody has used, and the book arrives from a text box
    for asked in ("", "@", "*", "8moves_v3 UHO_4060_v2"):
        assert book("file", asked).returncode != 0, asked
        assert book("fetch", asked, str(tmp_path)).returncode != 0, asked
    assert not list(tmp_path.iterdir())


def test_the_commands_it_does_not_have_are_refused():
    for asked in (book(), book("sha256", "8moves_v3"), book("count", "8moves_v3")):
        assert asked.returncode != 0
        assert "usage" in asked.stderr


def test_counting_a_pgn_counts_its_games(tmp_path):
    # the tag pair a game opens with, and a comment holding the same word,
    # which is not an opening
    played = tmp_path / "small.pgn"
    played.write_text(
        '[Event "?"]\n[Result "*"]\n\n1. e4 e5 *\n\n'
        '[Event "?"]\n[Result "*"]\n\n{ [Event ] } 1. d4 d5 *\n',
        encoding="utf-8",
    )
    counted = book("count", "8moves_v3", str(played))
    assert counted.returncode == 0, counted.stderr
    assert counted.stdout.strip() == "2"


def test_counting_an_epd_counts_its_lines(tmp_path):
    # a blank line is not a position, which is the whole difference from the
    # answer above: grepping for a tag pair here would count nothing at all
    played = tmp_path / "small.epd"
    played.write_text(
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq -\n"
        "\n"
        "r1bq1rk1/ppp2ppp/5n2/2bp4/2NPP3/2P5/PP3PPP/RNBQK2R w KQ d6\n",
        encoding="utf-8",
    )
    counted = book("count", "UHO_4060_v2", str(played))
    assert counted.returncode == 0, counted.stderr
    assert counted.stdout.strip() == "2"


def test_a_file_with_no_openings_in_it_is_refused(tmp_path):
    empty = tmp_path / "empty.pgn"
    empty.write_text("", encoding="utf-8")
    for asked in (
        book("count", "8moves_v3", str(empty)),
        book("count", "8moves_v3", str(tmp_path / "absent.pgn")),
    ):
        assert asked.returncode != 0
        assert asked.stdout.strip() == ""


def test_both_workflows_default_to_a_book_the_table_knows():
    known = books()
    for workflow in WORKFLOWS:
        named = defaults(workflow)
        assert named, f"{workflow.name} has no book to check"
        # one default written twice, once per trigger. A run from the actions
        # tab and a run from a release play the same openings or neither
        # figure means what the other does
        assert len(set(named)) == 1, named
        assert named[0] in known, f"{workflow.name} defaults to {named[0]}"


def test_the_default_is_still_the_book_the_figures_were_played_on():
    # every Strength and Calibrate figure in the ledger and the release notes
    # was played on this one, so changing the default quietly would make new
    # numbers incomparable with old ones
    for workflow in WORKFLOWS:
        assert defaults(workflow) == ["8moves_v3", "8moves_v3"]


def test_the_cache_key_names_the_pin_the_books_come_from():
    action = yaml.safe_load(ACTION.read_text(encoding="utf-8"))
    steps = action["runs"]["steps"]
    key = next(step["with"]["key"] for step in steps if step.get("id") == "cache")
    abbreviated = key.rsplit("-", 1)[-1]
    assert len(abbreviated) >= 7, key
    assert pin().startswith(abbreviated), f"{key} is not the pin book.sh fetches at"


def test_a_book_that_is_not_the_one_the_table_names_is_refused(tmp_path):
    name = books()[0]
    played = tmp_path / field("FILE", name)
    played.write_text("not the book\n", encoding="utf-8")
    checked = book("verify", name, str(tmp_path))
    assert checked.returncode == 1, checked.stdout
    assert "not the" in checked.stderr, checked.stderr


def test_a_book_that_is_not_there_is_refused(tmp_path):
    checked = book("verify", books()[0], str(tmp_path))
    assert checked.returncode == 1, checked.stdout
    assert "not there" in checked.stderr, checked.stderr


def test_the_books_are_checked_even_when_the_cache_hits():
    # the fetch is skipped on a cache hit, so a check that carries the same
    # condition would leave the file most runs play unchecked
    steps = yaml.safe_load(ACTION.read_text(encoding="utf-8"))["runs"]["steps"]
    checking = [step for step in steps if "book.sh verify" in step.get("run", "")]
    assert checking, "no step checks the books in tools/ against the table"
    for step in checking:
        assert "if" not in step, f"{step['name']} is skipped when the cache hits"
