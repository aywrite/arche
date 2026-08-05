#!/usr/bin/env python3
"""Print the elo estimate, and any sprt verdict, from a fastchess result file."""

import re
import sys
from pathlib import Path

# the lookbehind keeps this from matching the nElo figure on the same line
ELO = re.compile(r"(?<![A-Za-z])Elo:\s*(-?[\d.]+)\s*\+/-\s*([\d.]+)")
GAMES = re.compile(r"Games:\s*(\d+)")
POINTS = re.compile(r"Points:\s*[\d.]+\s*\(([\d.]+)\s*%\)")
# fastchess only prints an LLR line when it was run under -sprt, so the line
# doubles as the sign that there is a verdict to look for
LLR = re.compile(r"LLR:.*(\[[^\]]+\])")
# printed the moment the log likelihood ratio crosses a bound. A match that
# hits a cap first stops without printing one
VERDICT = re.compile(r"SPRT \(\[[^\]]+\]\) completed - (H[01]) was accepted")


def estimate(text: str) -> str:
    games = GAMES.findall(text)[-1]
    found = ELO.findall(text)
    if not found:
        # every game pair went the same way: fastchess prints the elo as inf,
        # and the score is all there is to report
        score = POINTS.findall(text)[-1]
        return f"{score}% score ({games} games)"
    elo, margin = found[-1]
    return f"{round(float(elo)):+d} ±{round(float(margin))} Elo ({games} games)"


def verdict(text: str) -> str:
    """The sprt verdict, or nothing for a match that was not run under -sprt."""
    bounds = LLR.findall(text)
    if not bounds:
        return ""
    accepted = VERDICT.findall(text)
    if not accepted:
        return f", SPRT {bounds[-1]} inconclusive"
    hypothesis = accepted[-1]
    reading = "stronger" if hypothesis == "H1" else "not stronger"
    return f", SPRT {bounds[-1]} accepted {hypothesis}: {reading}"


def main() -> None:
    text = Path(sys.argv[1]).read_text()
    print(f"{estimate(text)}{verdict(text)}")


if __name__ == "__main__":
    main()
