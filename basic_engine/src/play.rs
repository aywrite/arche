use crate::board::Board;
use crate::misc::index_to_coordinate;
use crate::misc::{Piece, PromotePiece};
use std::fmt;

/// A move. Six bytes, which is what the fields happen to come to and what the
/// search is tuned around: a move is copied into a move list, out of it, into
/// the history and into the transposition table, several times per node.
/// Widening it has been measured twice and was slower both times.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Play {
    pub from: u8,
    pub to: u8,
    pub capture: Option<Piece>,
    pub promote: Option<PromotePiece>,

    pub en_passant: bool, // True if an en_passant move was played
    pub castle: bool,     // True if the move was a castling
}

impl Play {
    pub fn new(
        from: u8,
        to: u8,
        capture: Option<Piece>,
        promote: Option<PromotePiece>,
        en_passant: bool,
        castle: bool,
    ) -> Self {
        Play {
            from,
            to,
            capture,
            promote,
            en_passant,
            castle,
        }
    }

    /// Most valuable victim, least valuable attacker: take the biggest piece
    /// with the smallest one first.
    ///
    /// The scores index by piece rather than matching on it: the arms did
    /// different arithmetic per piece, which compiled to an indirect jump
    /// taken once per capture scored, and the pieces arrive in no order a
    /// predictor can learn. Inline because the sort computes a key per move
    /// generated, and this is most of the key.
    #[inline]
    pub fn mvv_lva(&self, board: &Board) -> i64 {
        const VICTIM_SCORES: [i64; 6] = [100, 250, 300, 400, 500, 1000];
        const ATTACKER_SCORES: [i64; 6] = [6, 5, 4, 3, 2, 1];
        let Some(victim) = self.capture else {
            return 0;
        };
        let Some(attacker) = board.get_piece_index(self.from) else {
            return 0;
        };
        let score = VICTIM_SCORES[victim as usize] + ATTACKER_SCORES[attacker as usize];
        // a queen taking a defended piece is usually just losing the queen, so
        // push it below the other captures rather than trying it first
        if matches!(attacker, Piece::Queen) && board.square_attacked(self.to, !board.active_color) {
            return score - 300;
        }
        score
    }
}

impl fmt::Display for Play {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (from_rank, from_file) = index_to_coordinate(self.from);
        let (to_rank, to_file) = index_to_coordinate(self.to);
        write!(f, "{}{}", from_file, from_rank)?;
        write!(f, "{}{}", to_file, to_rank)?;
        if let Some(promote) = &self.promote {
            write!(f, "{}", char::from(promote))?;
        }
        Ok(())
    }
}
