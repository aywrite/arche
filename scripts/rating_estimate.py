#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Estimate a rating on the ccrl scale from a gauntlet.

The code is in match_tools/rating_estimate.py. This file is the path the
workflows and the documentation call. It runs from a checkout with nothing
installed, because python puts the directory a script sits in at the front of
the import path, and the package sits in this one.
"""

from match_tools.rating_estimate import main

if __name__ == "__main__":
    main()
