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
