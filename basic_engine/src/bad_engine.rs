//pub struct RandomEngine {
//    pub board: Board,
//}
//
//impl Engine for RandomEngine {
//    fn new(board: Board) -> Self {
//        Self { board }
//    }
//
//    fn search(&mut self, depth: u8) -> Option<Play> {
//        // TODO make this not mut
//        let mut moves = self.board.generate_moves();
//        moves.shuffle(&mut rand::thread_rng());
//        for m in moves.iter() {
//            if self.board.make_move(m) {
//                self.board.undo_move().unwrap();
//                return Some(*m);
//            }
//        }
//        None
//    }
//
//    fn make_move(&mut self, play: &Play) {
//        self.board.make_move(play);
//    }
//}
//
//pub struct SimpleEngine {
//    pub board: Board,
//}
//
//impl Engine for SimpleEngine {
//    fn new(board: Board) -> Self {
//        Self { board }
//    }
//
//    fn search(&mut self, depth: u8) -> Option<Play> {
//        // TODO make this not mut
//        let mut best_score = 0;
//        let mut best_move: Option<&Play> = None;
//
//        let moves = self.board.generate_moves();
//        for m in moves.iter() {
//            if self.board.make_move(m) {
//                // TODO switch on color instead of using abs
//                if self.board.white_value >= best_score {
//                    best_score = self.board.white_value;
//                    best_move = Some(m);
//                }
//                self.board.undo_move().unwrap();
//            }
//        }
//        if let Some(play) = best_move {
//            return Some(*play);
//        }
//        None
//    }
//
//    fn make_move(&mut self, play: &Play) {
//        self.board.make_move(play);
//    }
//}
