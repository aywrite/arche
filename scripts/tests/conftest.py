# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""The scripts are plain files rather than a package, so put their directory on
the path: each test then imports the one it covers as a module. The insert is
also what lets a clone run the tests with nothing installed."""

import sys
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(SCRIPTS))
