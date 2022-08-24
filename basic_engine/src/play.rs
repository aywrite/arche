use crate::misc::index_to_coordinate;
use crate::misc::{Piece, PromotePiece};
use std::fmt;

#[derive(Debug)]
pub struct Play {
    from: u8,
    to: u8,
    capture: Option<Piece>,
    promote: Option<PromotePiece>,
}

impl Play {
    pub fn new(from: u8, to: u8, capture: Option<Piece>, promote: Option<PromotePiece>) -> Self {
        Play {
            from,
            to,
            capture,
            promote,
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
        Ok(())
    }
}
