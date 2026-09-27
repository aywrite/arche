// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The swap as a table, for the exchanges no slider can join from behind.
//!
//! When every piece that can take part in an exchange on a square already
//! bears on it directly, the swap's line is fixed by the pieces alone: each
//! side recaptures with its least valuable attacker, and no capture uncovers a
//! new one. What the capture wins then depends on three things only: the
//! capturer, and each side's direct attackers counted by value. `Board::see`
//! proves that condition with one probe and reads the answer here; when the
//! condition fails, or a side has more than three attackers, it walks the
//! swap instead.
//!
//! The swap prices a knight and a bishop alike, so the counts are by value
//! class: pawn, minor, rook, queen, king. A side's counts are packed into one
//! number in mixed radix, wide enough for every count a side's direct
//! attackers can reach on one square, and the counts with at most three
//! attackers in all are then numbered densely, 49 of them. The table holds,
//! for each capturer class and each pair of dense codes, what the defender
//! wins back: the result is the victim's value less the entry. Every entry is
//! a multiple of a hundred and at most the king's price, so it is held in
//! hundredths in a byte.
//!
//! Nothing here is built at run time. `every_entry_is_the_exchange_played_out`
//! checks the whole table against a minimax written separately.

/// What each value class is worth to the swap: pawn, minor, rook, queen and
/// king, as `SEE_VALUES` prices them.
const CLASS_VALUE: [i32; 5] = [100, 300, 500, 900, 10_000];

/// The radix of each class's place: one more than the most attackers of that
/// class that can bear on one square directly (two pawns, twelve minors from
/// the eight knight squares and four diagonals, four rooks, eight queens, one
/// king).
const RADIX: [usize; 5] = [3, 13, 5, 9, 2];

/// What one attacker of each class adds to its side's packed code: the
/// product of the radices below it.
const PLACE: [usize; 5] = {
    let mut place = [1; 5];
    let mut class = 1;
    while class < 5 {
        place[class] = place[class - 1] * RADIX[class - 1];
        class += 1;
    }
    place
};

/// The number of packed codes: the product of the radices.
const PACKED: usize = PLACE[4] * RADIX[4];

/// What one attacker adds to its side's packed code, indexed by what a square
/// holds (pawn to king, then no piece, which adds nothing).
pub(crate) static PLACE_VALUE: [usize; 7] = [
    PLACE[0], PLACE[1], PLACE[1], PLACE[2], PLACE[3], PLACE[4], 0,
];

/// The most attackers a side may have and still be in the table.
const MOST: usize = 3;

/// How many sides have at most three attackers: the ways to put up to three
/// attackers into five classes, less the seven with a third pawn or a second
/// king.
const SIDES: usize = 49;

/// A side's packed code is read masked to this length, a power of two past
/// `PACKED`, so the read carries no bounds check; the codes reach no further
/// than `PACKED`, and the padding holds `FAR`.
pub(crate) const CODES: usize = 4096;

/// What a side with more than three attackers reads, far enough past the table
/// that the sum of both sides' offsets still misses it and the swap is walked.
const FAR: u16 = 0x4000;

/// A packed code's counts, pawn to king.
const fn counts(packed: usize) -> [usize; 5] {
    let mut counts = [0; 5];
    let mut class = 0;
    while class < 5 {
        counts[class] = (packed / PLACE[class]) % RADIX[class];
        class += 1;
    }
    counts
}

/// The dense numbering: each packed code's number among the sides with at
/// most three attackers (`FAR` for the rest), and each number's counts.
const fn dense() -> ([u16; PACKED], [[usize; 5]; SIDES]) {
    let mut number = [FAR; PACKED];
    let mut back = [[0; 5]; SIDES];
    let mut next = 0;
    let mut packed = 0;
    while packed < PACKED {
        let c = counts(packed);
        if c[0] + c[1] + c[2] + c[3] + c[4] <= MOST {
            number[packed] = next as u16;
            back[next] = c;
            next += 1;
        }
        packed += 1;
    }
    assert!(
        next == SIDES,
        "the sides with at most three attackers are 49"
    );
    (number, back)
}

const DENSE: ([u16; PACKED], [[usize; 5]; SIDES]) = dense();

/// A side's offset into the table, from its packed code: its dense number
/// scaled by `scale`, or `FAR`.
const fn offsets(scale: u16) -> [u16; CODES] {
    let mut out = [FAR; CODES];
    let mut packed = 0;
    while packed < PACKED {
        let n = DENSE.0[packed];
        if n != FAR {
            out[packed] = n * scale;
        }
        packed += 1;
    }
    out
}

/// The capturing side's offset, a row of 49.
pub(crate) static OURS: [u16; CODES] = offsets(SIDES as u16);

/// The defending side's offset, a column.
pub(crate) static THEIRS: [u16; CODES] = offsets(1);

/// Where each capturer's block of the table starts, indexed by what the from
/// square holds. An empty from square cannot move: it is sent past the table,
/// to the walk, which panics. (`see`'s two exits return before reading it.)
pub(crate) static CAPTURER_BLOCK: [usize; 7] = [
    0,
    SIDES * SIDES,
    SIDES * SIDES,
    2 * SIDES * SIDES,
    3 * SIDES * SIDES,
    4 * SIDES * SIDES,
    1 << 16,
];

/// What the defender wins back, for a capturer of class `capturer` on the
/// square and the two sides' attackers: the swap played out on the lines it
/// has to follow. The defender moves first, each side takes with its least
/// valuable attacker, taking the king ends it, and each side stops when
/// going on would cost it.
const fn won_back(capturer: usize, ours: [usize; 5], theirs: [usize; 5]) -> i32 {
    // each side's attackers, least valuable first
    let mut queue = [[0; MOST]; 2];
    let mut length = [0; 2];
    let sides = [ours, theirs];
    let mut side = 0;
    while side < 2 {
        let mut class = 0;
        while class < 5 {
            let mut n = 0;
            while n < sides[side][class] {
                queue[side][length[side]] = class;
                length[side] += 1;
                n += 1;
            }
            class += 1;
        }
        side += 1;
    }
    // the pieces that stand on the square in turn, the capturer first
    let mut line = [0; 2 * MOST + 1];
    line[0] = capturer;
    let mut taken = 0;
    let mut next = [0; 2];
    let mut to_move = 1;
    while next[to_move] < length[to_move] {
        taken += 1;
        if line[taken - 1] == 4 {
            break;
        }
        line[taken] = queue[to_move][next[to_move]];
        next[to_move] += 1;
        to_move ^= 1;
    }
    // from the last capture back: each side takes only if it gains
    let mut back = 0;
    while taken > 0 {
        let gain = CLASS_VALUE[line[taken - 1]] - back;
        back = if gain > 0 { gain } else { 0 };
        taken -= 1;
    }
    back
}

const fn table() -> [u8; 5 * SIDES * SIDES] {
    let mut table = [0; 5 * SIDES * SIDES];
    let mut capturer = 0;
    while capturer < 5 {
        let mut ours = 0;
        while ours < SIDES {
            let mut theirs = 0;
            while theirs < SIDES {
                let back = won_back(capturer, DENSE.1[ours], DENSE.1[theirs]);
                assert!(back % 100 == 0 && back / 100 <= 255);
                table[capturer * SIDES * SIDES + ours * SIDES + theirs] = (back / 100) as u8;
                theirs += 1;
            }
            ours += 1;
        }
        capturer += 1;
    }
    table
}

/// What the defender wins back, in hundredths, by capturer block and the two
/// sides' offsets.
pub(crate) static TABLE: [u8; 5 * SIDES * SIDES] = table();

#[cfg(test)]
mod tests {
    use super::*;

    /// The exchange by plain minimax over every choice, rather than the
    /// least valuable attacker and a fold: at each turn the side to move may
    /// stop, or take with any attacker it has left.
    fn minimax(on_square: usize, mut sides: [[usize; 5]; 2], to_move: usize) -> i32 {
        let mut best = 0;
        for class in 0..5 {
            if sides[to_move][class] == 0 {
                continue;
            }
            sides[to_move][class] -= 1;
            // taking the king ends the exchange: the side that left it there
            // could not legally have, which its price says
            let answer = if on_square == 4 {
                0
            } else {
                minimax(class, sides, to_move ^ 1)
            };
            sides[to_move][class] += 1;
            best = best.max(CLASS_VALUE[on_square] - answer);
        }
        best
    }

    #[test]
    fn every_entry_is_the_exchange_played_out() {
        let mut checked = 0;
        for capturer in 0..5 {
            for ours in 0..SIDES {
                for theirs in 0..SIDES {
                    let sides = [DENSE.1[ours], DENSE.1[theirs]];
                    let expected = minimax(capturer, sides, 1);
                    let entry = i32::from(TABLE[capturer * SIDES * SIDES + ours * SIDES + theirs]);
                    assert_eq!(
                        100 * entry,
                        expected,
                        "capturer {capturer}, ours {:?}, theirs {:?}",
                        sides[0],
                        sides[1]
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 5 * SIDES * SIDES);
    }

    #[test]
    fn a_packed_code_and_its_offsets_agree() {
        for packed in 0..CODES {
            let counted = packed < PACKED && counts(packed).iter().sum::<usize>() <= MOST;
            assert_eq!(OURS[packed] != FAR, counted, "{packed}");
            assert_eq!(THEIRS[packed] != FAR, counted, "{packed}");
        }
    }
}
