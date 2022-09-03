use crate::board::Board;
use crate::misc::index_to_coordinate;
use crate::misc::{Piece, PromotePiece};
use std::fmt;

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

    pub fn mmv_lva(&self, board: &Board) -> u64 {
        let victim_score = match self.capture {
            None => return 0,
            Some(Piece::Pawn) => 10,
            Some(Piece::Knight) => 25,
            Some(Piece::Bishop) => 40,
            Some(Piece::Rook) => 40,
            Some(Piece::Queen) => 50,
            Some(Piece::King) => 100,
        };
        let attacker_score = match board.get_piece_index(self.from) {
            None => return 0,
            Some(Piece::Pawn) => 1,
            Some(Piece::Knight) => 2,
            Some(Piece::Bishop) => 3,
            Some(Piece::Rook) => 4,
            Some(Piece::Queen) => 5,
            Some(Piece::King) => 6,
        };
        victim_score + attacker_score
    }
}

impl fmt::Display for Play {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (from_rank, from_file) = index_to_coordinate(self.from);
        let (to_rank, to_file) = index_to_coordinate(self.to);
        write!(f, "{:?}{}", from_file, from_rank)?;
        write!(f, "{:?}{}", to_file, to_rank)?;
        if let Some(promote) = &self.promote {
            write!(f, "{}", char::from(promote))?;
        }
        if let Some(capture) = &self.capture {
            write!(f, "  x[{:?}]", capture)?;
        }
        if self.castle {
            write!(f, "  -- (castled)")?;
        }
        Ok(())
    }
}
