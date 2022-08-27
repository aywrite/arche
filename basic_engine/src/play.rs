use crate::misc::index_to_coordinate;
use crate::misc::{Piece, PromotePiece};
use std::fmt;

#[derive(Debug, Copy, Clone, PartialEq)]
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
}

impl fmt::Display for Play {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (from_rank, from_file) = index_to_coordinate(self.from.into());
        let (to_rank, to_file) = index_to_coordinate(self.to.into());
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
