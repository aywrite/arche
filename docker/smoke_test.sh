#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Check that a built image contains a working engine, book and lichess-bot config.
set -eu

image=${1:?usage: smoke_test.sh <image>}

echo "Checking the UCI handshake"
handshake=$(printf 'uci\nisready\nposition startpos\ngo movetime 500\nquit\n' \
    | docker run --rm -i "$image" engines/arche)
echo "$handshake"
echo "$handshake" | grep -qx 'uciok'
echo "$handshake" | grep -qx 'readyok'
echo "$handshake" | grep -qE '^bestmove [a-h][1-8][a-h][1-8]'

echo "Checking the config and the opening book"
docker run --rm -i "$image" python3 - <<'PY'
import chess
import chess.polyglot
from lib.config import load_config

config = load_config("config.yml")
assert config.engine.polyglot.enabled, "the polyglot book is not enabled"

board = chess.Board()
for book in config.engine.polyglot.book.standard:
    with chess.polyglot.open_reader(book) as reader:
        entries = list(reader.find_all(board))
    assert entries, f"{book} has no move for the starting position"
    print(f"{book}: {len(entries)} first moves, best is {board.san(max(entries, key=lambda e: e.weight).move)}")
PY
