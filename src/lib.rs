pub trait Game {
    fn debug_print(&self);
    fn from_fen(fen: String) -> Result<Self, String>
    where
        Self: std::marker::Sized;
}
