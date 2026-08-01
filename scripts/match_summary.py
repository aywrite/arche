#!/usr/bin/env python3
"""Print the elo estimate from a fastchess result file."""

import re
import sys
from pathlib import Path

# the lookbehind keeps this from matching the nElo figure on the same line
ELO = re.compile(r"(?<![A-Za-z])Elo:\s*(-?[\d.]+)\s*\+/-\s*([\d.]+)")
GAMES = re.compile(r"Games:\s*(\d+)")


def main() -> None:
    text = Path(sys.argv[1]).read_text()
    elo, margin = ELO.findall(text)[-1]
    games = GAMES.findall(text)[-1]
    print(f"{round(float(elo)):+d} ±{round(float(margin))} Elo ({games} games)")


if __name__ == "__main__":
    main()
