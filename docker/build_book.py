#!/usr/bin/env python3
"""Build a Polyglot opening book from the lichess-org/chess-openings data set.

The data set (CC0) is a list of named openings given as PGN move sequences. Every
line is replayed and each position/move pair is recorded, weighted by the number
of named openings that pass through it. Mainline theory appears in many named
variations and so ends up with a much higher weight than novelties such as
1. Nh3, which keeps the "weighted_random" selection of lichess-bot sensible.
"""

import argparse
import csv
import struct
import sys
from collections import defaultdict
from pathlib import Path

import chess
import chess.polyglot

ENTRY_STRUCT = struct.Struct(">QHHI")
MAX_WEIGHT = 0xFFFF

# Polyglot promotion codes, indexed the same way as chess.PIECE_TYPES.
PROMOTION_CODES = {chess.KNIGHT: 1, chess.BISHOP: 2, chess.ROOK: 3, chess.QUEEN: 4}


def encode_move(board: chess.Board, move: chess.Move) -> int:
    """Encode a move in the Polyglot 16 bit representation."""
    to_square = move.to_square
    if board.is_castling(move):
        # Polyglot encodes castling as the king capturing its own rook.
        rook_file = (
            chess.BB_FILE_H if board.is_kingside_castling(move) else chess.BB_FILE_A
        )
        to_square = chess.msb(board.rooks & board.occupied_co[board.turn] & rook_file)

    promotion = PROMOTION_CODES[move.promotion] if move.promotion else 0
    return (
        chess.square_file(to_square)
        | (chess.square_rank(to_square) << 3)
        | (chess.square_file(move.from_square) << 6)
        | (chess.square_rank(move.from_square) << 9)
        | (promotion << 12)
    )


def read_openings(data_dir: Path) -> list[str]:
    """Read the `pgn` column out of every opening table in `data_dir`."""
    lines = []
    for table in sorted(data_dir.glob("[a-e].tsv")):
        with table.open(newline="") as handle:
            for row in csv.DictReader(handle, delimiter="\t"):
                lines.append(row["pgn"])
    if not lines:
        raise SystemExit(f"no opening tables found in {data_dir}")
    return lines


def count_moves(openings: list[str], max_plies: int) -> dict[tuple[int, int], int]:
    """Count how many opening lines play a given move in a given position."""
    counts: dict[tuple[int, int], int] = defaultdict(int)
    for pgn in openings:
        board = chess.Board()
        for token in pgn.split():
            if token.endswith("."):
                continue
            if board.ply() >= max_plies:
                break
            move = board.parse_san(token)
            counts[(chess.polyglot.zobrist_hash(board), encode_move(board, move))] += 1
            board.push(move)
    return counts


def write_book(counts: dict[tuple[int, int], int], destination: Path) -> None:
    """Write the counted moves as a Polyglot book, sorted by key as the format requires."""
    with destination.open("wb") as handle:
        for (key, move), weight in sorted(counts.items()):
            handle.write(ENTRY_STRUCT.pack(key, move, min(weight, MAX_WEIGHT), 0))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "data_dir", type=Path, help="checkout of lichess-org/chess-openings"
    )
    parser.add_argument("output", type=Path, help="path of the Polyglot book to write")
    parser.add_argument(
        "--max-plies", type=int, default=16, help="how deep to record each line"
    )
    args = parser.parse_args()

    openings = read_openings(args.data_dir)
    counts = count_moves(openings, args.max_plies)
    write_book(counts, args.output)
    print(f"wrote {len(counts)} entries from {len(openings)} openings to {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
