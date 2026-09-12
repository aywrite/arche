# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the tuner's split into three groups.

The mapping is small and two scripts read it, which is why it is a file of its
own and why it is tested where it lives rather than through either of them. A
corpus built to one reading of it and fitted against another would print
nothing about the disagreement.
"""

import groups
import pytest


def test_the_three_groups_are_assigned_by_the_first_byte_of_the_game_key():
    """Slices nought, one and two train, slice three chooses the ridge, and
    slice four is not read. The key is the sha256 of the game's movetext, so
    the group is a property of the play and a re-extraction moves nothing."""
    assert [groups.group_of(f"{slice_:02x}" + "0" * 62) for slice_ in range(5)] == [
        "train",
        "train",
        "train",
        "selection",
        "calibration",
    ]
    # every byte that is that slice modulo five lands in the same group, which
    # is what makes the shares three fifths, a fifth and a fifth
    assert {groups.group_of(f"{byte:02x}" + "0" * 62) for byte in range(0, 256, 5)} == {
        "train"
    }
    assert {groups.group_of(f"{byte:02x}" + "0" * 62) for byte in range(4, 256, 5)} == {
        "calibration"
    }
    # and a key that is no sha256 is refused rather than bucketed
    for bad in ("", "z", "zz" + "0" * 62):
        with pytest.raises(ValueError, match="no sha256"):
            groups.group_of(bad)


def sliced(byte):
    """A pair key whose first byte puts it in the slice `byte` names."""
    return f"{byte:02x}" + "0" * 62


def test_a_named_seal_is_the_calibration_group_and_nothing_else_is():
    """The keys the caller names are sealed and every other pair splits three
    to one between training and selection, the pairs the key rule would have
    sealed included. That is the point of naming them: the archive only grows,
    so the fifth the key rule seals is the same fifth every time it is asked,
    and a vector revised after a reading needs games nothing has read."""
    sealed = {sliced(7)}
    assert groups.group_of(sliced(7), sealed) == "calibration"
    # slice four is what the key rule seals, and under a named seal it trains
    assert groups.group_of(sliced(4)) == "calibration"
    assert groups.group_of(sliced(4), sealed) == "train"
    # nothing outside the named set reaches the sealed group, whatever its key
    assert {
        groups.group_of(sliced(byte), sealed)
        for byte in range(256)
        if sliced(byte) not in sealed
    } == {"train", "selection"}
    # and the four open slices are three to one, in the order the five were
    assert [groups.group_of(sliced(byte), sealed) for byte in range(4)] == [
        "train",
        "train",
        "train",
        "selection",
    ]


def test_an_empty_seal_is_not_the_same_as_no_seal():
    """`None` draws the group from the key and an empty set seals nothing.
    The two are different answers and have to stay different, or a caller that
    passed an empty set by accident would silently get the key rule's fifth
    back and call it a seal."""
    assert groups.group_of(sliced(4), None) == "calibration"
    assert groups.group_of(sliced(4), set()) == "train"


def test_a_seal_file_is_read_and_a_line_that_is_no_key_is_refused(tmp_path):
    """The file says which runs it was drawn from, so comments and blank lines
    are skipped. Anything else has to be a pair key: a typo that named no pair
    is a game quietly not sealed, and the seal is the one thing nobody should
    have to take on trust."""
    named = tmp_path / "sealed.txt"
    named.write_text(
        "# drawn 2026-09-12 from runs 34655239490 and 34656775686\n"
        "\n"
        f"{sliced(1)}\n"
        f"{sliced(2)}  # the second run's first pair\n"
        f"{sliced(1)}\n",
        encoding="utf-8",
    )
    assert groups.sealed_pairs(named) == {sliced(1), sliced(2)}
    for bad in ("zz" + "0" * 62, "0" * 63, "0" * 65):
        broken = tmp_path / "broken.txt"
        broken.write_text(f"{sliced(1)}\n{bad}\n", encoding="utf-8")
        with pytest.raises(ValueError, match="no pair key"):
            groups.sealed_pairs(broken)
    empty = tmp_path / "empty.txt"
    empty.write_text("# nothing but a comment\n", encoding="utf-8")
    with pytest.raises(ValueError, match="names no pair"):
        groups.sealed_pairs(empty)
