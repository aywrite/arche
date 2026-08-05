"""The scripts are plain files rather than a package, so put their directory
on the path and each test imports the one it covers as a module."""

import sys
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(SCRIPTS))
