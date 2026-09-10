# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""The tools a match is read with: the pooled estimate, the ccrl fit, the
terminations count and the book slice.

They are one package because they read one thing. The fit owns the two regular
expressions a fastchess pgn is split into games with, and the estimate reads
the terminations count as well as the fit.
"""
