#!/usr/bin/env python3
"""Print the elo estimate, and any sprt verdict, from a fastchess result file."""

import argparse
import re
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


def trailer(text: str, tc: str, baseline: str) -> str:
    """The result as the Elo trailer a commit carries, in the one shape the
    commit-msg hook accepts: the estimate, the sprt bounds and verdict when
    there were any, the games, the time control and what was played. The
    verdict is the point of an sprt, an estimate of +58 with the test
    failed and one with it passed are not the same claim, so it is named,
    and a match a cap stopped is inconclusive. A sweep has no estimate,
    fastchess prints inf, and a trailer claiming a number it never gave
    would be a lie the hook could not catch."""
    found = ELO.findall(text)
    if not found:
        return "Elo: not measured"
    elo, margin = found[-1]
    games = GAMES.findall(text)[-1]
    sprt = ""
    if bounds := LLR.findall(text):
        low, high = (float(bound) for bound in bounds[-1].strip("[]").split(","))
        accepted = VERDICT.findall(text)
        verdict = (
            "inconclusive"
            if not accepted
            else ("passed" if accepted[-1] == "H1" else "failed")
        )
        sprt = f"sprt [{low:g}, {high:g}] {verdict}, "
    estimate = f"{round(float(elo)):+d} ±{round(float(margin))}"
    return f"Elo: {estimate} ({sprt}{games} games, {tc}, vs {baseline})"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("result", help="a fastchess result file")
    parser.add_argument(
        "--trailer",
        action="store_true",
        help="print the Elo trailer for a commit instead of the summary line",
    )
    parser.add_argument("--tc", default="", help="the time control played")
    parser.add_argument("--baseline", default="", help="what was played against")
    args = parser.parse_args()
    text = Path(args.result).read_text()
    if args.trailer:
        print(trailer(text, args.tc, args.baseline))
    else:
        print(f"{estimate(text)}{verdict(text)}")


if __name__ == "__main__":
    main()
