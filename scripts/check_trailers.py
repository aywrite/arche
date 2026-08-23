#!/usr/bin/env python3
"""Check the trailers a commit message carries against what its kind requires.

A commit that changes the engine says what the bench counts after it, so the
history states how much of the tree each change looks at; a commit that
claims speed says how much it measured; and an elo claim is in the one shape
the changelog and the release notes read. Runs as a commit-msg hook, given
the file holding the message, and in ci over every commit of a pull request.

Trailers are read the way git reads them: the `Key: value` lines of the
final paragraph, and nothing else. A Bench line with prose after it is body
text to git, to the changelog and to the ci check, so it is body text here.

The scopes and types are the engine half of the table in docs/DEVELOPMENT.md.
"""

import re
import sys
from pathlib import Path

ENGINE_SCOPES = {"board", "eval", "magic", "search", "zobrist"}
MEASURED_TYPES = {"feat", "fix", "perf", "refactor", "revert"}
SUBJECT = re.compile(r"^(?P<type>\w+)(\((?P<scope>[\w-]+)\))?!?: ")
EXEMPT = ("Merge ", "fixup! ", "squash! ", "chore(release): prepare for ")
TRAILER_LINE = re.compile(r"^[A-Za-z][\w-]*: ")
TRAILER = re.compile(r"^(Bench|Speed|Elo): (.*)$")
# what openbench's server reads the expected bench from: the last of these
# anywhere in the message, so the trailer has to be it
OPENBENCH = re.compile(r"(?:bench|nodes)[ :=]+([0-9,]+)", re.IGNORECASE)
# one round shows no spread, so it is no measurement
SPEED = re.compile(
    r"^[+-]\d+(\.\d+)?% \(bench nps, ([2-9]|\d{2,}) interleaved rounds "
    r"vs [0-9a-f]{7,40}, spread \d+(\.\d+)?%\)$"
)
ELO = re.compile(
    r"^(not measured"
    r"|[+-]\d+ ±\d+ \((sprt \[-?\d+(\.\d+)?, -?\d+(\.\d+)?\], )?"
    r"\d+ games, [^,()]+, vs [^()]+\))$"
)


def trailers_of(lines: list[str]) -> dict[str, str]:
    """The trailers of a message, from its final paragraph, as git keeps
    them. A final paragraph with anything else in it holds none."""
    body = "\n".join(lines[1:]).strip("\n")
    paragraphs = re.split(r"\n\s*\n", body) if body else []
    last = (
        [line for line in paragraphs[-1].splitlines() if line.strip()]
        if paragraphs
        else []
    )
    if not last or not all(TRAILER_LINE.match(line) for line in last):
        return {}
    found = {}
    for line in last:
        if match := TRAILER.match(line):
            found[match.group(1)] = match.group(2)
    return found


def problems(message: str) -> list[str]:
    """Everything wrong with the message's trailers, in the order found."""
    lines = [line for line in message.splitlines() if not line.startswith("#")]
    if not lines or lines[0].startswith(EXEMPT):
        return []
    match = SUBJECT.match(lines[0])
    kind = match.group("type") if match else ""
    scope = match.group("scope") if match else ""
    trailers = trailers_of(lines)

    engine = scope in ENGINE_SCOPES and kind in MEASURED_TYPES
    found = []
    if engine and "Bench" not in trailers:
        found.append("an engine commit needs a Bench: trailer")
    if "Bench" in trailers:
        value = trailers["Bench"]
        # exactly digits: the ci check reads the line with git's trailer
        # parser and compares the value as printed
        if not re.fullmatch(r"\d+", value):
            found.append(f"Bench: must be a plain number, got {value}")
        else:
            read = OPENBENCH.findall("\n".join(lines))
            last = read[-1] if read else None
            if last != value:
                found.append(
                    f"Bench: {value} is not the last bench number in the message, "
                    f"which openbench reads: {last}"
                )
    if engine and kind == "perf" and "Speed" not in trailers:
        found.append("a perf commit needs a Speed: trailer")
    if "Speed" in trailers and not SPEED.match(trailers["Speed"]):
        found.append(
            "Speed: is not in the shape scripts/speed.sh prints, "
            f"got {trailers['Speed']}"
        )
    if "Elo" in trailers and not ELO.match(trailers["Elo"]):
        found.append(
            "Elo: is not in the shape the strength workflow prints, "
            f"got {trailers['Elo']}"
        )
    return found


def main(argv: list[str]) -> int:
    message = Path(argv[0]).read_text(encoding="utf-8", errors="replace")
    found = problems(message)
    for problem in found:
        print(f"commit message: {problem}", file=sys.stderr)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
