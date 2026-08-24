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


def sprt(text: str) -> tuple[str, str] | None:
    """The bounds an sprt was run under, as fastchess printed them, and which
    of passed, failed and inconclusive it came to; or nothing for a match that
    was not run under -sprt. Both renderings below read the verdict from here
    rather than each deciding it, or a summary could call a test inconclusive
    while the trailer beside it called the same test failed."""
    bounds = LLR.findall(text)
    if not bounds:
        return None
    accepted = VERDICT.findall(text)
    # no verdict line at all means a cap stopped it before either hypothesis
    # was reached
    if not accepted:
        return bounds[-1], "inconclusive"
    return bounds[-1], ("passed" if accepted[-1] == "H1" else "failed")


def verdict(text: str) -> str:
    """The sprt verdict as the workflow summary words it."""
    if (found := sprt(text)) is None:
        return ""
    bounds, reached = found
    if reached == "inconclusive":
        return f", SPRT {bounds} inconclusive"
    hypothesis, reading = (
        ("H1", "stronger") if reached == "passed" else ("H0", "not stronger")
    )
    return f", SPRT {bounds} accepted {hypothesis}: {reading}"


def trailer(text: str, tc: str, baseline: str) -> str:
    """The result as the Elo trailer a commit carries, in the one shape the
    commit-msg hook accepts: the estimate, the sprt bounds and verdict when
    there were any, the games, the time control and what was played. The
    verdict is the point of an sprt, an estimate of +58 with the test
    failed and one with it passed are not the same claim, so it is named. A
    sweep has no estimate, fastchess prints inf, and a trailer claiming a
    number it never gave would be a lie the hook could not catch."""
    found = ELO.findall(text)
    if not found:
        return "Elo: not measured"
    elo, margin = found[-1]
    games = GAMES.findall(text)[-1]
    test = ""
    if (found_sprt := sprt(text)) is not None:
        bounds, reached = found_sprt
        # the bounds are workflow inputs and arrive with trailing zeroes the
        # hook's shape does without
        low, high = (float(bound) for bound in bounds.strip("[]").split(","))
        test = f"sprt [{low:g}, {high:g}] {reached}, "
    estimate = f"{round(float(elo)):+d} ±{round(float(margin))}"
    return f"Elo: {estimate} ({test}{games} games, {tc}, vs {baseline})"


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
