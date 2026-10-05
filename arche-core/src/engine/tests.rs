// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The tests of the search: what it answers, what its shortcuts do, and
//! what its instruments record. The switches, the root bounds and the
//! aspiration window are tested beside their code in `engine.rs`.

mod search {
    use crate::board::{fens, fens::SHARP_MIDDLEGAME, play_named};
    use crate::engine::AlphaBeta;
    use crate::engine::Board;
    use crate::engine::Engine;
    use crate::engine::{
        ASPIRATION_MIN_DEPTH, Aspiration, Decision, Limits, MAX_PLY, NULL_MOVE_MIN_DEPTH,
        NULL_MOVE_REDUCTION, Play, RootBounds, Score, ScoreBound, SearchConfig, SearchOutcome,
        SearchParameters, SearchResult, TaintPolicy, Value, null_move_reduction,
    };
    use crate::late_move::{
        DEEP_REDUCTION, DEEP_REDUCTION_MIN_DEPTH, LATE_MOVE_MIN_DEPTH, LATE_MOVE_REDUCTION,
        LATE_MOVE_THRESHOLD,
    };
    use crate::limits::Clock;
    use crate::misc::{Color, Piece};
    use crate::value::CHECKMATE_THRESHOLD;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time;

    /// A later move as the loop asks for it, scouted `reduction` plies
    /// shallower first when that is not zero, with no ledger staging.
    fn later(reduction: u8) -> Decision {
        Decision::Search {
            reduction,
            staged: None,
        }
    }

    /// The default table is 256MB, and one per test dominated the suite's
    /// memory and run time.
    const TABLE_BYTES: usize = 16 * 1024 * 1024;

    fn engine(board: Board) -> AlphaBeta {
        AlphaBeta::with_table_bytes(board, TABLE_BYTES)
    }

    /// A full width node takes its static evaluation from the entry its
    /// probe finds, so what the search stores beside a position has to be
    /// that position's evaluation. After a search, every position within two
    /// plies of the root whose entry holds one is held to the evaluation
    /// computed afresh.
    #[test]
    fn a_table_entry_holds_its_own_positions_evaluation() {
        fn stored(e: &AlphaBeta, board: &Board) -> Option<Score> {
            let _ = e
                .transpositions
                .probe(board, Score::MIN + 1, Score::MAX - 1, 0, false, false);
            let found = e.transpositions.probed_eval(board.key);
            (found != crate::transposition::NO_EVAL).then_some(found)
        }
        for fen in fens::CORE {
            let mut e = engine(Board::from_fen(fen).unwrap());
            completed(e.search(6));
            let mut board = e.board.clone();
            let mut positions = vec![board.clone()];
            for m in &board.generate_moves() {
                if board.make_move(m) {
                    positions.push(board.clone());
                    for reply in &board.generate_moves() {
                        if board.make_move(reply) {
                            positions.push(board.clone());
                            board.undo_move();
                        }
                    }
                    board.undo_move();
                }
            }
            let mut held = 0;
            for position in &positions {
                if let Some(found) = stored(&e, position) {
                    assert_eq!(found, crate::eval::eval(position), "{}", fen);
                    held += 1;
                }
            }
            assert!(held > 0, "no entry held an evaluation in {}", fen);
        }
    }

    #[test]
    fn a_resized_table_is_the_size_asked_for_and_still_searched_on() {
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(1024 * 1024));

        // whole buckets, as a new engine asked for a megabyte would have
        assert_eq!(
            e.table_bytes(),
            AlphaBeta::with_table_bytes(Board::new(), 1024 * 1024).table_bytes()
        );
        assert!(e.table_bytes() <= 1024 * 1024);
        assert!(matches!(e.search(4), SearchOutcome::Complete(_, _)));
    }

    #[test]
    fn a_table_there_is_no_memory_for_leaves_the_old_one_in_place() {
        // both ways of failing: usize::MAX is refused before the allocator
        // is reached, while isize::MAX is a size it may describe and no
        // machine can meet, which is the refusal a real oversized Hash hits
        for bytes in [usize::MAX, isize::MAX as usize] {
            let mut e = engine(Board::new());
            assert!(!e.set_table_bytes(bytes), "{}", bytes);
            assert_eq!(e.table_bytes(), engine(Board::new()).table_bytes());
            assert!(matches!(e.search(3), SearchOutcome::Complete(_, _)));
        }
    }

    #[test]
    fn a_table_too_small_to_hold_an_entry_is_still_a_table() {
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(0));
        assert!(e.table_bytes() > 0);
        assert!(matches!(e.search(3), SearchOutcome::Complete(_, _)));
    }

    #[test]
    fn a_new_table_keeps_the_counts_the_search_made() {
        // the counts are the engine's, so neither a Hash change nor a new
        // game resets them
        let mut e = engine(Board::new());
        completed(e.search(5));
        let counted = e.ghi();
        assert!(counted.stores > 0, "the search stored nothing");
        assert!(e.set_table_bytes(1024 * 1024));
        assert_eq!(e.ghi(), counted);
        e.new_game();
        e.clear_table();
        assert_eq!(e.ghi(), counted);
    }

    /// The reference search, for the tests that hold it to answering the
    /// same whatever the table holds.
    fn reference(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(board, TABLE_BYTES, SearchConfig::reference())
    }

    /// The reference with reverse futility on and nothing else touched.
    fn shortcut(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                reverse_futility: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// The reference with the pass on.
    fn passing(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                null_move: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// `passing` with the adaptive reduction on.
    fn passing_adaptively(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                null_move: true,
                adaptive_null_move: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// The reference with the quiet memories on, which move the tree and
    /// never the answer.
    fn remembering(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                move_memory: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// The reference with the losing capture skip on.
    fn skipping(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                see_pruning: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// The reference with the late move reductions on, and no memories to
    /// order the quiets it reduces.
    fn reducing(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                late_move_reductions: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// `reducing` with the deep reduction on top.
    fn deep_reducing(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                late_move_reductions: true,
                deep_reductions: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// `deep_reducing` with the pruning on top.
    fn pruning(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                late_move_reductions: true,
                deep_reductions: true,
                late_move_pruning: true,
                ..SearchConfig::reference()
            },
        )
    }

    fn completed(outcome: SearchOutcome) -> SearchResult {
        match outcome {
            SearchOutcome::Complete(result, _) => result,
            other => panic!("expected a completed search, got {:?}", other),
        }
    }

    /// The depth a seeded entry claims: deep enough that nothing these tests
    /// search can outrank it.
    const SEEDED_DEPTH: u8 = 5;

    #[test]
    fn a_losing_position_is_still_losing_with_a_warm_table() {
        // a search of another position first once left entries that made
        // this losing position look better
        let game =
            Board::from_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(7));
        assert!(
            result.score < -800,
            "expect bad score (first) got {}",
            result.score
        );

        let game = Board::from_fen(SHARP_MIDDLEGAME).unwrap();
        let mut e = engine(game);
        completed(e.search(7));
        let _ = e.parse_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16");
        let result = completed(e.search(7));
        assert!(result.score < -800, "expect bad score got {}", result.score);
    }

    #[test]
    fn the_reported_line_opens_with_the_move_actually_answered() {
        // A deeper entry for the root position, left by an earlier search of
        // it, used to win the depth contest against the root's own store:
        // the table then told a line opening with the leftover's move while
        // bestmove answered the fresh one, and the two disagreed in front of
        // whatever was relaying the search. Here the queen hangs, so a fresh
        // search must answer with the capture, while the planted leftover
        // claims a quiet king move from a depth no shallow search can beat.
        let game = Board::from_fen("k7/8/8/3q4/8/8/3R4/K7 w - - 0 1").unwrap();
        let mut e = engine(game);
        let quiet = play_named(&e.board, "a1b1");
        assert!(e.transpositions.record_best(
            &e.board,
            quiet,
            Value::clean(0),
            14,
            crate::transposition::NO_EVAL
        ));
        let result = completed(e.search(2));
        let takes = play_named(&e.board, "d2d5");
        assert_eq!(result.best_move, takes);
        assert_eq!(e.pv_line().line.first(), Some(&takes));
    }

    #[test]
    fn a_fail_high_on_a_later_move_is_re_searched_once_at_the_full_window() {
        // Three quiet moves and nothing for either side to capture, so at
        // depth one every child visit is exactly two nodes: the full width
        // frame and the quiescence stand pat under it. That makes the
        // re-search countable. With the middle scoring move planted as the
        // table's, the root searches it first with the full window, the
        // worse move fails its zero width search, and the best move alone
        // comes back above alpha and is searched a second time: the root's
        // node plus four child visits. Planted with the best move instead,
        // nothing fails high and the second search never happens.
        const FEN: &str = "8/8/8/8/8/8/2k4P/K7 w - - 0 1";
        let mut b = Board::from_fen(FEN).unwrap();
        let moves = b.generate_moves();
        let mut scored: Vec<(Play, Score)> = Vec::new();
        for m in &moves {
            // generation is pseudo legal, and the king stepping next to the
            // other one is refused here the way the search refuses it
            if !b.make_move(m) {
                continue;
            }
            scored.push((*m, -crate::eval::eval(&b)));
            b.undo_move();
        }
        assert_eq!(scored.len(), 3);
        scored.sort_by_key(|(_, score)| *score);
        // distinct scores, or there is no middle one to plant
        assert!(scored[0].1 < scored[1].1 && scored[1].1 < scored[2].1);
        let (middle, _) = scored[1];
        let (best, best_score) = scored[2];

        let mut e = engine(Board::from_fen(FEN).unwrap());
        assert!(e.transpositions.record_best(
            &e.board,
            middle,
            Value::clean(0),
            SEEDED_DEPTH,
            crate::transposition::NO_EVAL
        ));
        let result = completed(e.search(1));
        assert_eq!(result.best_move, best);
        assert_eq!(result.score, best_score);
        assert_eq!(e.nodes, 9);

        let mut e = engine(Board::from_fen(FEN).unwrap());
        assert!(e.transpositions.record_best(
            &e.board,
            best,
            Value::clean(0),
            SEEDED_DEPTH,
            crate::transposition::NO_EVAL
        ));
        let result = completed(e.search(1));
        assert_eq!(result.best_move, best);
        assert_eq!(result.score, best_score);
        assert_eq!(e.nodes, 7);
    }

    #[test]
    fn a_re_search_answers_with_its_own_score_not_the_probes() {
        // The node count above says a re-search ran, not whose answer came
        // back. A ceiling planted one point inside the probe's window and
        // outside the re-search's makes the probe fail high short of the
        // exact score, which comes from an engine of its own; only the
        // re-search's answer matches it.
        const FEN: &str = "8/8/8/8/8/8/2k4P/K7 w - - 0 1";
        let mut oracle = reference(Board::from_fen(FEN).unwrap());
        let m = play_named(&oracle.board, "h2h4");
        assert!(oracle.board.make_move(&m));
        let Ok(exact) = oracle.windowed(
            Score::MIN + 2,
            Score::MAX,
            2,
            &Decision::First,
            RootBounds::Both,
        ) else {
            panic!("an unlimited search aborted");
        };

        let alpha = exact.score - 50;
        let beta = exact.score + 50;
        let mut e = reference(Board::from_fen(FEN).unwrap());
        let m = play_named(&e.board, "h2h4");
        assert!(e.board.make_move(&m));
        let reply = play_named(&e.board, "c2c3");
        assert!(e.transpositions.record_ceiling(
            &e.board,
            reply,
            Value::clean(-alpha - 1),
            SEEDED_DEPTH,
            crate::transposition::NO_EVAL
        ));
        let Ok(value) = e.windowed(alpha, beta, 2, &later(0), RootBounds::Neither) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(value.score, exact.score);
    }

    #[test]
    fn a_proven_mate_is_not_searched_again_a_ply_deeper() {
        // The bench position `wac 4`, a mate in two. When mate distance
        // pruning landed (c7730f1) depth nine searched 385 times depth
        // five's nodes without it and 15 times with it. With the attention
        // weights refitted on game positions it reads 98 times with it and
        // 384 without. The bound holds that shape loosely; bench.rs pins
        // the exact counts.
        const FEN: &str = "r1bq2rk/pp3pbp/2p1p1pQ/7P/3P4/2PB1N2/PP3PPR/2KR4 w - - 0 1";
        let at_five = completed(engine(Board::from_fen(FEN).unwrap()).search(5));
        let at_nine = completed(engine(Board::from_fen(FEN).unwrap()).search(9));
        assert_eq!(
            at_five.checkmate_in(),
            Some(2),
            "the mate moved at depth five"
        );
        assert_eq!(
            at_nine.checkmate_in(),
            Some(2),
            "the mate moved at depth nine"
        );
        assert!(
            at_nine.nodes < at_five.nodes * 150,
            "depth nine searched {} nodes against depth five's {}",
            at_nine.nodes,
            at_five.nodes
        );
    }

    #[test]
    fn the_mate_distance_survives_a_deeper_warm_search() {
        // Searching again deeper off a warm table reuses mate scores stored
        // at other plies, and the distance reported must not move. Whether
        // a given depth finds this mate under the shortcuts is not monotone
        // in the depth (the late move count loses it at three to five), so
        // the test asks that no depth disagree and that several find it.
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = engine(game);
        let mut found = 0;
        for depth in 3..=8 {
            let result = completed(e.search(depth));
            let Some(mate) = result.checkmate_in() else {
                continue;
            };
            found += 1;
            assert_eq!(mate, 2, "the distance moved at depth {}", depth);
            assert_eq!(
                format!("{}", result.best_move),
                "g3g6",
                "the move moved at depth {}",
                depth
            );
        }
        assert!(found > 1, "{found} of the depths saw the mate");
    }

    /// What holds `REVERSE_FUTILITY_MARGIN` above the boundary its comment
    /// gives: with the shortcut the only thing added to the reference, a
    /// margin of seventy six or less cuts off the line this mate is found
    /// in. Cold, so no table decides it.
    #[test]
    fn the_reverse_futility_margin_keeps_the_depth_four_mate() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let result = completed(shortcut(game).search(4));
        assert_eq!(result.checkmate_in(), Some(2));
        assert_eq!(format!("{}", result.best_move), "g3g6");
    }

    #[test]
    fn checkmate_in_one_is_found_for_black() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbNQp/3pN3/2pP4/2P5/PPB4P/R4RK1 b - - 1 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(-1));
    }

    /// Material that cannot mate is searched as the draw it is. Before the
    /// rule each of these read as a win of three pawns or more.
    #[test]
    fn material_that_cannot_mate_is_searched_as_a_draw() {
        for fen in [
            "8/8/8/8/8/4k3/8/4K1N1 w - - 0 1",
            "8/8/8/8/8/4k3/8/4K1N1 b - - 0 1",
            "8/8/8/8/8/4k3/8/4KB2 w - - 0 1",
            "8/8/8/8/8/4k3/8/4KB2 b - - 0 1",
            "8/8/8/8/8/4k3/8/4K1NN w - - 0 1",
            "8/8/8/8/8/4k3/8/4K1NN b - - 0 1",
        ] {
            let mut e = engine(Board::from_fen(fen).unwrap());
            let result = completed(e.search(12));
            assert_eq!(result.score, 0, "{}", fen);
        }
    }

    /// A mate inside the horizon is still found in a position the rule calls
    /// drawn, which is why the rule sits in the evaluation and not at the
    /// node: a `Value::clean(0)` returned from `alpha_beta` before the moves
    /// were generated would lose this. Two knights cannot force mate, but
    /// the black king here stands in a helpmate.
    #[test]
    fn a_helpmate_survives_the_rule() {
        let mut e = engine(Board::from_fen("k7/3N4/1K6/1N6/8/8/8/8 w - - 0 1").unwrap());
        let result = completed(e.search(5));
        assert_eq!(result.checkmate_in(), Some(1));
        assert_eq!(format!("{}", result.best_move), "b5c7");
    }

    #[test]
    fn quiescence_does_not_stand_pat_out_of_a_mate() {
        // the queen on a8 hangs and taking it loses: Rxa8 Nxf2 is mate, by a
        // capture two plies into quiescence, where the mated node used to
        // stand pat as though it could decline to move
        let game = Board::from_fen("q7/7k/8/8/6n1/8/5PPP/R5RK w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_ne!(format!("{}", result.best_move), "a1a8");
    }

    /// Black is stalemated and not in check. Qxf7 from
    /// `TAKES_INTO_STALEMATE` reaches this position.
    const STALEMATED: &str = "7k/5Q2/7K/8/8/8/8/8 b - - 0 1";
    /// White to move, a queen against a bishop. The one capture, Qxf7,
    /// leaves black stalemated.
    const TAKES_INTO_STALEMATE: &str = "7k/5b2/7K/8/8/8/8/5Q2 w - - 0 1";

    #[test]
    fn a_stalemate_at_the_horizon_is_scored_as_a_draw() {
        let mut board = Board::from_fen(STALEMATED).unwrap();
        assert!(!board.in_check());
        assert!(!board.has_legal_move());
        let mut e = engine(board);
        assert!(e.eval() < 0);
        assert_eq!(e.quiescence_value(), 0);
    }

    #[test]
    fn a_capture_into_stalemate_is_not_scored_as_the_material_it_wins() {
        let mut after = Board::from_fen(TAKES_INTO_STALEMATE).unwrap();
        let takes = play_named(&after, "f1f7");
        assert!(after.make_move(&takes));
        assert_eq!(
            after.to_fen(),
            Board::from_fen(STALEMATED).unwrap().to_fen()
        );

        // the capture draws, so white stands pat a queen against a bishop
        let mut e = engine(Board::from_fen(TAKES_INTO_STALEMATE).unwrap());
        let standing = e.eval();
        assert!(standing > 0);
        assert_eq!(e.quiescence_value(), standing);
    }

    #[test]
    fn a_search_one_ply_short_does_not_take_into_stalemate() {
        let mut e = engine(Board::from_fen(TAKES_INTO_STALEMATE).unwrap());
        let shallow = completed(e.search(1));
        assert_ne!(format!("{}", shallow.best_move), "f1f7");
        assert!(shallow.score > 0);
        // a ply more finds Qa1+ Kg8 Qg7#
        let mut e = engine(Board::from_fen(TAKES_INTO_STALEMATE).unwrap());
        let deeper = completed(e.search(2));
        assert_eq!(deeper.checkmate_in(), Some(2));
    }

    #[test]
    fn the_king_test_answers_as_playing_the_moves_out_does() {
        // side to move, none in check
        let cases = [
            // the king steps out, so no move is played
            ("8/8/8/3k4/8/3K4/3P4/8 b - - 0 1", true),
            // stalemate
            (STALEMATED, false),
            // the king is boxed in and the pawn can still push
            ("7k/5Q2/7K/8/8/8/p7/8 b - - 0 1", true),
            // boxed in, and the pawn is blocked
            ("7k/5Q2/7K/8/8/p7/P7/8 b - - 0 1", false),
            // boxed in, and the pawn's one capture is pinned by the bishop
            ("7k/4N1p1/6KP/8/8/8/8/B7 b - - 0 1", false),
        ];
        for (fen, expected) in cases {
            let mut board = Board::from_fen(fen).unwrap();
            assert!(!board.in_check(), "{}", fen);
            assert_eq!(board.has_legal_move(), expected, "{}", fen);
            assert_eq!(board.has_legal_move_out_of_check(), expected, "{}", fen);
        }
    }

    #[test]
    fn a_capture_that_cannot_reach_alpha_is_not_searched() {
        // one capture on the board: one node when it is skipped, two when
        // it is searched
        let fen = "7k/8/8/8/R3p3/8/8/7K w - - 0 1";
        let mut e = engine(Board::from_fen(fen).unwrap());
        let standing = e.eval();
        let gain = crate::eval::material(Piece::Pawn) as Score;

        // one point past what the pawn and the whole margin can make up
        let alpha = standing + gain + crate::engine::DELTA_MARGIN + 1;
        let Ok(value) = e.quiescence(alpha, alpha + 1) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, 1);
        assert_eq!(value, Value::clean(standing));

        // at the edge the capture is searched
        let mut e = engine(Board::from_fen(fen).unwrap());
        let alpha = standing + gain + crate::engine::DELTA_MARGIN;
        assert!(e.quiescence(alpha, alpha + 1).is_ok());
        assert_eq!(e.nodes, 2);
    }

    #[test]
    fn an_evasion_is_searched_whatever_the_margin_says() {
        // taking the checking queen is the one evasion, at an alpha no
        // capture could reach under the margin
        let fen = "7k/8/8/8/8/8/1q6/K7 w - - 0 1";
        let mut b = Board::from_fen(fen).unwrap();
        let takes = play_named(&b, "a1b2");
        assert!(b.make_move(&takes));
        let expected = -crate::eval::eval(&b);

        let mut e = engine(Board::from_fen(fen).unwrap());
        let Ok(value) = e.quiescence(20_000, 20_001) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, 2);
        assert_eq!(value, Value::clean(expected));
    }

    #[test]
    fn a_promotion_is_searched_whatever_the_margin_says() {
        // the pawn can promote, taking the rook or pushing, at an alpha far
        // past what any margin allows
        let fen = "r6k/1P6/8/8/8/8/8/7K w - - 0 1";
        let mut e = engine(Board::from_fen(fen).unwrap());
        assert!(e.quiescence(10_000, 10_001).is_ok());
        assert!(e.nodes > 1, "no promotion was searched");
    }

    #[test]
    fn a_mating_capture_is_searched_whatever_the_margin_says() {
        // rook takes rook and mates on the back rank, asked under an alpha
        // inside the mate window, where the margin would call every capture
        // hopeless
        let fen = "3r3k/6pp/8/8/8/8/8/3R3K w - - 0 1";
        let mut e = engine(Board::from_fen(fen).unwrap());
        let Ok(value) = e.quiescence(29_500, 29_501) else {
            panic!("an unlimited search aborted");
        };
        assert!(e.nodes > 1, "the mating capture was not searched");
        assert!(
            crate::engine::is_mate(value.score) && value.score > 29_500,
            "no mate found: {}",
            value.score
        );
    }

    /// Quiescence at a window one point wide around the standing eval, so
    /// the stand pat neither cuts the node nor leaves the captures out of
    /// the window: what the node does with them is the node count.
    fn quiet_nodes(mut e: AlphaBeta) -> (u64, Value) {
        let standing = e.eval();
        let Ok(value) = e.quiescence(standing, standing + 1) else {
            panic!("an unlimited search aborted");
        };
        (e.nodes, value)
    }

    #[test]
    fn a_losing_capture_is_not_searched() {
        // the one capture, the rook taking the e4 pawn, loses the rook to
        // the d5 pawn
        let fen = "7k/8/8/3p4/R3p3/8/8/7K w - - 0 1";
        let (searched, _) = quiet_nodes(reference(Board::from_fen(fen).unwrap()));
        assert!(searched > 1, "the reference did not search the capture");

        let mut e = skipping(Board::from_fen(fen).unwrap());
        let standing = e.eval();
        let (skipped, value) = quiet_nodes(e);
        assert_eq!(skipped, 1);
        assert_eq!(value, Value::clean(standing));
    }

    #[test]
    fn a_winning_and_an_even_capture_are_searched_whatever_the_swap_says() {
        // a winning capture and an even one
        for fen in [
            "7k/8/8/8/R3p3/8/8/7K w - - 0 1",
            "3rr2k/8/8/8/8/8/8/4R2K w - - 0 1",
        ] {
            let (searched, answer) = quiet_nodes(reference(Board::from_fen(fen).unwrap()));
            assert!(searched > 1, "the reference did not search {fen}");
            assert_eq!(
                quiet_nodes(skipping(Board::from_fen(fen).unwrap())),
                (searched, answer),
                "{fen}"
            );
        }
    }

    #[test]
    fn a_side_in_check_searches_a_losing_evasion() {
        // the queen taking the checking knight is the one evasion, and the
        // d3 pawn takes her back. Three nodes: this one, the capture and
        // the recapture
        let fen = "7k/8/8/8/8/3p4/PPn5/KQ6 w - - 0 1";
        for mut e in [
            reference(Board::from_fen(fen).unwrap()),
            skipping(Board::from_fen(fen).unwrap()),
        ] {
            let Ok(value) = e.quiescence(-10_000, 10_000) else {
                panic!("an unlimited search aborted");
            };
            assert_eq!(e.nodes, 3);
            assert!(
                !crate::engine::is_mate(value.score),
                "read as mated: {}",
                value.score
            );
        }
    }

    #[test]
    fn a_promoting_capture_is_never_skipped() {
        // the pawn takes the rook on a8 and promotes, and the b8 rook takes
        // the queen back. Today's swap prices no promoting capture as
        // losing, so the exemption has nothing to catch; this holds the
        // promise for a swap that one day does
        let fen = "rr5k/1P6/8/8/8/8/8/7K w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let promotes = play_named(&board, "b7a8q");
        assert!(
            board.see(&promotes) >= 0,
            "the swap priced the promotion as losing"
        );

        let (searched, answer) = quiet_nodes(reference(Board::from_fen(fen).unwrap()));
        assert!(searched > 1, "the reference did not search the promotion");
        assert_eq!(
            quiet_nodes(skipping(Board::from_fen(fen).unwrap())),
            (searched, answer)
        );
    }

    #[test]
    fn the_mate_window_stands_the_skip_down() {
        // queen takes rook on e8 and mates: the knight that could take her
        // back is pinned, which the swap does not see, so it prices the
        // capture as losing. Under a mate window alpha the exemption stands
        // the skip down; under an ordinary window the same capture is
        // skipped, which says the exemption and not the swap saved it
        let fen = "4r2k/5pnp/8/8/8/2B5/8/K3Q3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        assert!(board.see(&play_named(&board, "e1e8")) < 0);

        let mut e = skipping(Board::from_fen(fen).unwrap());
        let Ok(value) = e.quiescence(29_500, 29_501) else {
            panic!("an unlimited search aborted");
        };
        assert!(e.nodes > 1, "the mating capture was not searched");
        assert!(
            crate::engine::is_mate(value.score) && value.score > 29_500,
            "no mate found: {}",
            value.score
        );

        // wide, because a narrow window would cut the node off on the even
        // capture of the knight before either arm reached the queen's
        let wide = |mut e: AlphaBeta| {
            assert!(e.quiescence(-10_000, 10_000).is_ok());
            e.nodes
        };
        let searched = wide(reference(Board::from_fen(fen).unwrap()));
        let skipped = wide(skipping(Board::from_fen(fen).unwrap()));
        assert!(
            skipped < searched,
            "nothing was skipped outside the mate window"
        );
    }

    #[test]
    fn the_horizon_sees_a_promotion_coming() {
        // the rook can win the knight or take the pawn one step from
        // promoting. The push captures nothing, so quiescence used not to
        // generate it and the knight looked free to take
        let game = Board::from_fen("4k3/8/8/R5n1/8/8/p5K1/8 w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_eq!(format!("{}", result.best_move), "a5a2");
    }

    #[test]
    fn a_shallow_search_still_sees_the_recapture() {
        // the queen can take a defended pawn, and at depth one only
        // quiescence sees the recapture
        let game = Board::from_fen("4k3/8/3p4/2p5/8/2Q5/8/4K3 w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_ne!(format!("{}", result.best_move), "c3c5");
    }

    #[test]
    fn deepening_through_shallow_depths_matches_a_cold_search() {
        // iterations shallower than four used to store scores whose leaves
        // were never quiesced, so the same depth answered differently warm
        // than cold. a_warm_cache_matches_a_cold_search searches each depth
        // directly and cannot see this
        let positions = [
            fens::KIWIPETE,
            // the pawn endgame, with the fifty move counter wound on
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            fens::PROMOTIONS,
        ];
        for fen in positions {
            let mut cold = reference(Board::from_fen(fen).unwrap());
            let expected = completed(cold.search(5));
            let mut warm = reference(Board::from_fen(fen).unwrap());
            let result = (1..=5)
                .map(|depth| completed(warm.search(depth)))
                .next_back()
                .unwrap();
            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
        }
    }

    #[test]
    fn a_narrowed_root_answers_what_the_full_one_does() {
        // the reference with the window on: a depth reached by deepening
        // under a window answers as one searched directly, so a failure
        // here is a failure of the schedule
        let aspiring = SearchConfig {
            aspiration: true,
            ..SearchConfig::reference()
        };
        const DEPTH: u8 = 6;
        let mut narrowed_somewhere = false;
        // the promotions position was the third until the pair term's refit
        // at a ridge of 3e-7, under which a queen and a rook promotion tie
        // at depth six (573 each) and the two searches break the tie
        // differently. A tie says nothing about the schedule, so the sharp
        // middlegame stands in for it, and every position here answers with
        // the same move both ways with the fitted table and the test rank
        for fen in [
            fens::KIWIPETE,
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            fens::SHARP_MIDDLEGAME,
        ] {
            let mut direct = reference(Board::from_fen(fen).unwrap());
            let expected = completed(direct.search(DEPTH));

            let mut open = reference(Board::from_fen(fen).unwrap());
            let full = completed(
                open.iterative_deepening_search(SearchParameters::to_depth(DEPTH), |_, _, _, _| {}),
            );
            let mut narrow =
                AlphaBeta::with_config(Board::from_fen(fen).unwrap(), TABLE_BYTES, aspiring);
            let result = completed(
                narrow
                    .iterative_deepening_search(SearchParameters::to_depth(DEPTH), |_, _, _, _| {}),
            );

            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
            narrowed_somewhere |= result.nodes != full.nodes;
        }
        assert!(
            narrowed_somewhere,
            "the window cost nothing anywhere, so the answers prove nothing"
        );
    }

    #[test]
    fn a_losing_side_plays_for_the_fifty_move_draw() {
        // white is a bishop down, and every move but a pawn push or a
        // capture takes the clock to a hundred
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    /// A fifty move draw is claimable and not automatic (FIDE 9.3), so a
    /// root whose counter has expired still answers with a move rather
    /// than `bestmove 0000`. The score is zero because every move here
    /// leaves the counter running.
    #[test]
    fn a_root_whose_fifty_move_counter_has_expired_still_answers_with_a_move() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 100 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
        assert!(
            e.board.generate_moves().contains(&result.best_move),
            "{} is not a legal move here",
            result.best_move
        );
    }

    /// The same position one ply before expiry, so the pair says the
    /// counter is what changed and not the position.
    #[test]
    fn the_same_root_one_ply_before_expiry_answers_the_same_way() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    /// A root mated on the hundredth half move is still game over.
    #[test]
    fn a_mate_on_the_hundredth_half_move_is_still_game_over() {
        let game = Board::from_fen("7k/6Q1/6K1/8/8/8/8/8 b - - 100 112").unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(3), SearchOutcome::GameOver));
    }

    /// A move that resets the counter is worth what it wins: with the
    /// counter expired, every quiet move here scores zero and the queen
    /// capture resets it.
    #[test]
    fn an_expired_counter_does_not_cost_a_win_a_capture_is_worth() {
        let game = Board::from_fen("3q3k/8/8/8/8/8/8/3Q2K1 w - - 100 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(6));
        assert_eq!(format!("{}", result.best_move), "d1d8");
        assert!(
            result.score > 500,
            "winning a queen scored {}",
            result.score
        );
    }

    #[test]
    fn a_depth_past_the_rail_from_a_check_is_clamped_in_the_library_too() {
        // search() is public, and the check extension on u8::MAX used to
        // overflow
        let game = Board::from_fen("3R2k1/5ppp/8/8/8/8/8/6K1 b - - 0 1").unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(u8::MAX), SearchOutcome::GameOver));
    }

    /// The position the two below start from: black to move and in check,
    /// so the extension fires at the first node either of them searches,
    /// and the king has h7 to step out to, so there is a tree under it.
    const IN_CHECK: &str = "3R2k1/5pp1/7p/8/8/8/8/6K1 b - - 0 1";

    #[test]
    fn a_full_width_line_stops_at_the_rail() {
        // a node on the rail answers from the static eval whatever depth
        // it still holds
        let mut e = engine(Board::from_fen(IN_CHECK).unwrap());
        assert!(e.board.in_check());
        e.board.line_ply = MAX_PLY as usize;

        let Ok(railed) = e.alpha_beta(Score::MIN + 1, Score::MAX - 1, 4, true, RootBounds::Both)
        else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, 1, "the node on the rail searched on");
        assert_eq!(railed.score, e.eval());
        assert!(!railed.tainted);
    }

    #[test]
    fn the_ply_under_the_rail_is_searched() {
        // the ply under the rail searches its one evasion, which rails: a
        // rail a ply early would count one, and no rail would search on
        let mut e = engine(Board::from_fen(IN_CHECK).unwrap());
        e.board.line_ply = MAX_PLY as usize - 1;

        assert!(
            e.alpha_beta(Score::MIN + 1, Score::MAX - 1, 4, true, RootBounds::Both)
                .is_ok(),
            "an unlimited search aborted"
        );
        assert_eq!(e.nodes, 2, "the ply under the rail and the one it rails");
    }

    #[test]
    fn a_mate_on_the_hundredth_half_move_is_a_mate_not_a_draw() {
        // Rh8 mates on the hundredth half move, and the mate outranks the
        // fifty move rule
        let game = Board::from_fen("k7/8/1K6/8/8/8/8/7R w - - 99 100").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(2));
        assert_eq!(result.checkmate_in(), Some(1));
        assert_eq!(format!("{}", result.best_move), "h1h8");
    }

    #[test]
    fn a_check_that_does_not_mate_on_the_hundredth_half_move_is_still_a_draw() {
        // the same check, but the king slips out to a7
        let game = Board::from_fen("k7/8/2K5/8/8/8/8/7R w - - 99 100").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    /// A budget of nodes and no clock.
    fn nodes_only(nodes: u64) -> Limits {
        Limits::starting_at(time::Instant::now(), None, nodes)
    }

    /// A search whose clock ran out before it began.
    fn already_spent() -> Limits {
        Limits::starting_at(
            time::Instant::now() - time::Duration::from_secs(1),
            Some(Clock::Share(time::Duration::from_millis(1))),
            u64::MAX,
        )
    }

    #[test]
    fn a_blown_deadline_stops_before_it_searches() {
        // the first poll happens before the root is counted
        let mut e = engine(Board::new());
        assert!(matches!(
            e.search_within(5, already_spent()),
            SearchOutcome::Aborted(None)
        ));
        assert_eq!(e.nodes, 0, "it searched past a clock that had run out");
    }

    #[test]
    fn deepening_with_no_time_budget_still_answers_depth_one() {
        // depth one runs whatever the clock says, so a spent clock still
        // gets a legal move back
        let mut e = engine(Board::new());
        let options = SearchParameters::new(None, already_spent());
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| depths.push(depth));
        assert!(
            matches!(outcome, SearchOutcome::Aborted(Some(_))),
            "expected a move from depth one, got {:?}",
            outcome
        );
        assert_eq!(depths, vec![1]);
    }

    #[test]
    fn a_node_budget_stops_the_search_on_exactly_that_node() {
        // a spread of budgets, so the last node falls in the full search,
        // in quiescence and now and then exactly on an iteration's end
        for limit in (50..6_000).step_by(97) {
            let mut e = engine(Board::new());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut completed: u64 = 0;
            let outcome =
                e.iterative_deepening_search(options, |_, result, _, _| completed = result.nodes);
            assert!(
                matches!(outcome, SearchOutcome::Aborted(Some(_))),
                "{}",
                limit
            );
            // either the last report is the last finished search and the
            // aborted search's nodes are still on the engine, or the report
            // is a swapped move's and already covers the whole deepening
            assert!(
                completed + e.nodes == limit || completed == limit,
                "budget {}: {} reported with {} left on the engine",
                limit,
                completed,
                e.nodes
            );
        }
    }

    #[test]
    fn an_aborted_iteration_still_counts_the_whole_deepening() {
        // wherever the root finished a move before the budget ran out, that
        // move answers, and its count is the budget to the node
        let mut deeper = 0;
        for limit in (50..6_000).step_by(97) {
            let mut e = engine(Board::new());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut reported = 0;
            let outcome =
                e.iterative_deepening_search(options, |_, result, _, _| reported = result.nodes);
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!(
                    "expected a move under a budget of {}, got {:?}",
                    limit, outcome
                )
            };
            if reported + e.nodes == limit {
                // no move to swap in. A ceiling finishes and does not
                // answer, so the answer's own count can be shallower than
                // the last report; the test above covers this arm
                continue;
            }
            assert_eq!(result.nodes, limit, "budget {}", limit);
            deeper += 1;
        }
        assert!(
            deeper > 0,
            "no budget in the sweep aborted with a root move in hand"
        );
    }

    #[test]
    fn an_iteration_that_searched_no_root_move_leaves_the_completed_depth_answering() {
        // a budget of exactly what three depths cost aborts the fourth on
        // its first poll
        let mut e = engine(Board::new());
        let three = completed(e.iterative_deepening_search(
            SearchParameters::new(Some(3), Limits::unlimited()),
            |_, _, _, _| {},
        ));

        let mut e = engine(Board::new());
        let options = SearchParameters::new(None, nodes_only(three.nodes));
        let outcome = e.iterative_deepening_search(options, |_, _, _, _| {});
        let SearchOutcome::Aborted(Some(result)) = outcome else {
            panic!("expected the completed depth's move, got {:?}", outcome)
        };
        assert_eq!(result.nodes, three.nodes);
        assert_eq!(result.best_move, three.best_move);
    }

    #[test]
    fn an_answer_the_deepening_rewrote_times_the_nodes_it_reports() {
        // the rows the test above skips: an answer from before the aborted
        // iteration has its count raised to cover it, and its time has to
        // be raised with it
        let mut rewritten = 0;
        for limit in (50..6_000).step_by(97) {
            let mut e = engine(Board::new());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut reported = None;
            let outcome = e.iterative_deepening_search(options, |_, result, _, _| {
                reported = Some((result.nodes, result.elapsed));
            });
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!(
                    "expected a move under a budget of {}, got {:?}",
                    limit, outcome
                )
            };
            let (nodes, elapsed) = reported.expect("a search reported no depth");
            if nodes + e.nodes != limit {
                continue;
            }
            assert!(
                result.elapsed > elapsed,
                "budget {}: {} nodes against the {} last reported, over the same {:?}",
                limit,
                result.nodes,
                nodes,
                elapsed
            );
            rewritten += 1;
        }
        assert!(
            rewritten > 0,
            "no budget in the sweep answered from before the aborted iteration"
        );
    }

    /// What a fresh engine answers depth four from the opening with, and
    /// what a depth five search under `budget` answers after it. Built anew
    /// for every budget so the answer depends on the budget alone.
    fn five_after_four(budget: u64) -> (Play, SearchOutcome) {
        let mut e = engine(Board::new());
        let four = completed(e.search(4));
        let five = e.search_within(5, nodes_only(budget));
        (four.best_move, five)
    }

    #[test]
    fn the_root_searches_the_previous_depths_best_move_first() {
        // what makes the swap sound. Whatever answers at the smallest budget
        // with a move in hand is the move the root tried first, and it has
        // to be the one the depth before answered with
        let finished = |budget| !matches!(five_after_four(budget).1, SearchOutcome::Aborted(None));
        // more nodes is never fewer root moves finished, so bisect
        let (mut none, mut some) = (0, 100_000);
        assert!(
            finished(some),
            "depth five finished no root move in {} nodes",
            some
        );
        while none + 1 < some {
            let mid = (none + some) / 2;
            if finished(mid) {
                some = mid
            } else {
                none = mid
            }
        }

        let (previous, outcome) = five_after_four(some);
        let SearchOutcome::Aborted(Some(result)) = outcome else {
            panic!(
                "one root move is not the whole of depth five, got {:?}",
                outcome
            )
        };
        assert_eq!(result.best_move, previous);
    }

    /// A fresh engine's depth five search from the opening under `budget`
    /// nodes, opened at a window far above what the position is worth, so
    /// every root move fails low. The deepening never opens one that wide
    /// of its last score, which is why this goes to the root directly.
    fn failing_low(budget: u64) -> SearchOutcome {
        let depth = ASPIRATION_MIN_DEPTH;
        let window = Aspiration::open(Some(530), depth);
        assert_eq!((window.alpha, window.beta), (500, 560));
        engine(Board::new()).search_root(depth, nodes_only(budget), None, window)
    }

    #[test]
    fn a_root_no_move_lifted_answers_a_ceiling_when_complete_and_nothing_when_aborted() {
        let SearchOutcome::Complete(result, bound) = failing_low(u64::MAX) else {
            panic!("an unlimited search did not complete");
        };
        assert!(result.score <= 500, "the opening is worth {}", result.score);
        assert_eq!(bound, ScoreBound::Upper);
        // one node short of the whole search stops in the last root move,
        // with every move before it searched and none of them past alpha.
        // The closest of them is never answered with
        assert!(matches!(
            failing_low(result.nodes - 1),
            SearchOutcome::Aborted(None)
        ));
    }

    #[test]
    fn a_root_whose_best_move_only_meets_alpha_answers_a_ceiling() {
        // one half move short of the fifty move draw with no capture on
        // the board, so every move scores the draw, exactly the alpha of a
        // window opened at thirty. Meeting alpha does not raise it
        let fen = "4k3/8/8/8/8/8/8/R3K3 w - - 99 120";
        let depth = ASPIRATION_MIN_DEPTH;
        let window = Aspiration::open(Some(30), depth);
        assert_eq!(window.alpha, 0);
        let mut e = engine(Board::from_fen(fen).unwrap());
        let SearchOutcome::Complete(result, bound) =
            e.search_root(depth, Limits::unlimited(), None, window)
        else {
            panic!("an unlimited search did not complete");
        };
        assert_eq!((result.score, bound), (0, ScoreBound::Upper));
    }

    #[test]
    fn a_swapped_answer_is_reported_before_the_search_ends() {
        // the swap is the one answer no completed depth reported. A sweep of
        // budgets, because which of them ends an iteration on a better move
        // is a fact about this position
        let mut swaps = 0;
        for limit in (500..40_000).step_by(311) {
            let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut reports: Vec<(Play, Option<Play>, ScoreBound)> = Vec::new();
            let outcome = e.iterative_deepening_search(options, |_, result, pv, bound| {
                reports.push((result.best_move, pv.line.first().copied(), bound));
            });
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                continue;
            };
            let completed = reports
                .iter()
                .rfind(|(_, _, bound)| *bound == ScoreBound::Exact)
                .map(|(play, _, _)| *play);
            if completed == Some(result.best_move) {
                // the deepest completed depth answered. A later ceiling names
                // no answer; a later floor must name the move in hand
                if let Some((play, _, ScoreBound::Lower)) = reports.last() {
                    assert_eq!(
                        *play, result.best_move,
                        "budget {}: a floor named a move the search did not answer with",
                        limit
                    );
                }
                continue;
            }
            swaps += 1;
            assert_eq!(
                reports
                    .last()
                    .map(|(play, first, bound)| (*play, *first, *bound)),
                Some((result.best_move, Some(result.best_move), ScoreBound::Lower)),
                "budget {}: the swapped move was never reported",
                limit
            );
        }
        assert!(swaps > 0, "no budget in the sweep swapped a move in");
    }

    /// A search stopped by its budget says it spent the budget, whichever
    /// of the two aborted answers it came back with. A caller cannot recover
    /// the count itself, since the two arrive in the same shape.
    #[test]
    fn a_search_stopped_by_its_budget_counts_the_iteration_it_gave_up() {
        for limit in [1_000u64, 2_500, 5_000, 7_500, 10_000, 25_000, 50_000] {
            let mut e = engine(Board::new());
            let outcome = e.iterative_deepening_search(
                SearchParameters::new(None, Limits::starting_now(None, Some(limit))),
                |_, _, _, _| {},
            );
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!("budget {limit}: an unlimited depth under a budget aborts with an answer");
            };
            assert_eq!(
                result.nodes, limit,
                "budget {limit}: the search says it spent other than its budget"
            );
        }
    }

    #[test]
    fn a_completed_depth_is_reported_as_an_exact_score() {
        let mut e = engine(Board::new());
        let mut bounds = Vec::new();
        let outcome = e.iterative_deepening_search(
            SearchParameters::new(Some(4), Limits::unlimited()),
            |_, _, _, bound| bounds.push(bound),
        );
        assert!(matches!(outcome, SearchOutcome::Complete(_, _)));
        assert_eq!(bounds, vec![ScoreBound::Exact; 4]);
    }

    #[test]
    fn a_node_budget_and_a_clock_stop_at_whichever_comes_first() {
        // the clock wins, after depth one
        let mut e = engine(Board::new());
        let options = SearchParameters::new(
            None,
            Limits::starting_at(
                time::Instant::now() - time::Duration::from_secs(1),
                Some(Clock::Share(time::Duration::from_millis(1))),
                1_000_000,
            ),
        );
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| depths.push(depth));
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(depths, vec![1]);

        // the budget wins, on its node
        let mut e = engine(Board::new());
        let limit = 1_000;
        let options = SearchParameters::new(
            None,
            Limits::starting_now(
                Some(Clock::Share(time::Duration::from_secs(10))),
                Some(limit),
            ),
        );
        let mut completed: u64 = 0;
        let outcome =
            e.iterative_deepening_search(options, |_, result, _, _| completed = result.nodes);
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(completed + e.nodes, limit);
    }

    #[test]
    fn a_stop_flag_already_set_still_answers_the_first_depth() {
        // the flag is armed the way the clock is, so depth one runs
        let mut e = engine(Board::new());
        let stop = Arc::new(AtomicBool::new(true));
        let options = SearchParameters::stoppable(None, Limits::unlimited(), Arc::clone(&stop));
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| depths.push(depth));
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(
            depths,
            vec![1],
            "the flag stopped the search before it had a move"
        );
    }

    #[test]
    fn a_stop_flag_set_mid_search_ends_the_deepening_with_a_move() {
        // an unlimited search of a sharp position, stopped from the report
        // of depth three
        let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let options = SearchParameters::stoppable(None, Limits::unlimited(), Arc::clone(&stop));
        let mut deepest = 0;
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| {
            deepest = depth;
            if depth >= 3 {
                stop.store(true, Ordering::Relaxed);
            }
        });
        let (SearchOutcome::Aborted(Some(result)) | SearchOutcome::Complete(result, _)) = outcome
        else {
            panic!("a stopped search must still answer, got {:?}", outcome)
        };
        assert!(deepest < crate::engine::MAX_PLY, "the flag stopped nothing");
        assert!(result.nodes > 0);
    }

    /// The clock and the node budget are armed the same way, which `Limits`
    /// says of itself; this is the flag.
    #[test]
    fn the_stop_flag_is_not_armed_until_a_depth_has_been_answered() {
        let stop = Arc::new(AtomicBool::new(false));
        let options = SearchParameters::stoppable(None, Limits::unlimited(), Arc::clone(&stop));
        let (_, unarmed) = options.for_iteration(false, 0);
        assert!(unarmed.is_none(), "the first iteration was stoppable");
        let (_, armed) = options.for_iteration(true, 0);
        let armed = armed.expect("an answered search was not stoppable");
        assert!(
            Arc::ptr_eq(&armed, &stop),
            "the armed flag is not the caller's"
        );
    }

    #[test]
    fn a_search_asked_for_directly_carries_no_flag_to_read() {
        // nothing but the deepening loop ever arms a flag
        let mut e = engine(Board::new());
        e.stop = Some(Arc::new(AtomicBool::new(true)));
        assert!(matches!(e.search(2), SearchOutcome::Complete(_, _)));
        assert!(e.stop.is_none(), "a leftover flag outlived the search");
        assert!(
            SearchParameters::new(Some(2), Limits::unlimited())
                .stop
                .is_none()
        );
    }

    #[test]
    fn a_node_budget_too_small_for_depth_one_still_answers_a_move() {
        let mut e = engine(Board::new());
        let options = SearchParameters::new(None, nodes_only(0));
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| depths.push(depth));
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(depths, vec![1]);
    }

    #[test]
    fn a_node_budget_and_a_depth_stop_at_whichever_comes_first() {
        let mut e = engine(Board::new());
        let options = SearchParameters::new(Some(2), nodes_only(1_000_000));
        assert!(matches!(
            e.iterative_deepening_search(options, |_, _, _, _| {}),
            SearchOutcome::Complete(_, _)
        ));

        let mut e = engine(Board::new());
        let options = SearchParameters::new(Some(crate::engine::MAX_PLY), nodes_only(1_000));
        let mut last_depth = 0;
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| last_depth = depth);
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert!(last_depth < crate::engine::MAX_PLY);
    }

    #[test]
    fn deepening_reports_each_completed_depth() {
        let mut e = engine(Board::new());
        let mut depths = Vec::new();
        let mut node_counts = Vec::new();
        let outcome =
            e.iterative_deepening_search(SearchParameters::to_depth(3), |depth, result, _, _| {
                assert!(result.nodes > 0);
                depths.push(depth);
                node_counts.push(result.nodes);
            });
        assert_eq!(depths, vec![1, 2, 3]);
        // the count covers the whole deepening so far
        assert!(
            node_counts.windows(2).all(|w| w[0] < w[1]),
            "node counts must grow with each depth: {:?}",
            node_counts
        );
        let SearchOutcome::Complete(result, _) = outcome else {
            panic!("expected a completed search, got {:?}", outcome);
        };
        assert_eq!(
            Some(result.nodes),
            node_counts.last().copied(),
            "the returned result must carry the same total the last report did"
        );
    }

    #[test]
    fn a_finished_game_is_game_over_with_no_depth_to_report() {
        // fool's mate and a stalemate
        let fens = [
            "rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3",
            "k7/8/1Q6/8/8/8/8/7K b - - 0 1",
        ];
        for fen in fens {
            let mut e = engine(Board::from_fen(fen).unwrap());
            assert!(matches!(e.search(3), SearchOutcome::GameOver), "{fen}");
            let outcome = e
                .iterative_deepening_search(SearchParameters::to_depth(3), |_, _, _, _| {
                    panic!("a finished game has no depths to report")
                });
            assert!(matches!(outcome, SearchOutcome::GameOver), "{fen}");
        }
    }

    /// The move loop reads no legal move found as mate or stalemate, so the
    /// shallow rules exempt the first move searched: a node whose every
    /// quiet the rule would prune still answers with what a move is worth
    /// rather than the stalemate's zero. Asked of the default, since the
    /// reference has the rule off.
    #[test]
    fn a_node_that_prunes_every_late_quiet_is_not_stalemated() {
        // no capture and no move that gives check, so the rule can reach
        // every move the node has
        let mut e = engine(Board::from_fen("7k/8/8/8/8/8/8/KN1B4 w - - 0 1").unwrap());
        assert!(e.config.quiet_futility, "the default carries the rule");
        let eval = e.eval();
        assert!(eval > 100, "the side to move is a piece up twice: {eval}");
        let alpha = eval + 10_000;
        assert!(!crate::value::is_mate(alpha));
        for depth in 1..=crate::late_move::SHALLOW_MAX_DEPTH {
            let Ok(value) = e.alpha_beta(alpha, alpha + 1, depth, true, RootBounds::Neither) else {
                panic!("nothing was armed to abort this search");
            };
            assert!(
                value.score > 100,
                "depth {depth} answered {}, which is the stalemate a pruned first move leaves",
                value.score
            );
        }
    }

    #[test]
    fn a_clock_that_runs_out_mid_deepening_still_answers_with_a_move() {
        // the one clock in the suite that is not already spent, so the only
        // test of the clock being read again thousands of nodes on
        let mut e = engine(Board::new());
        let params = SearchParameters::new(
            None,
            Limits::starting_now(Some(Clock::Share(time::Duration::from_millis(50))), None),
        );
        let outcome = e.iterative_deepening_search(params, |_, _, _, _| {});
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
    }

    #[test]
    fn a_deepening_stops_before_an_iteration_the_clock_will_not_cover() {
        // more than the soft share of a second has gone and the second
        // itself has not. The fraction belongs to Limits; this asserts the
        // deepening loop asks it, and only of a share of a game clock
        for (kind, depths) in [
            (Clock::Share as fn(time::Duration) -> Clock, vec![1]),
            (Clock::Fixed as fn(time::Duration) -> Clock, vec![1, 2]),
        ] {
            let mut e = engine(Board::new());
            let params = SearchParameters::new(
                Some(2),
                Limits::starting_at(
                    time::Instant::now() - time::Duration::from_millis(600),
                    Some(kind(time::Duration::from_secs(1))),
                    u64::MAX,
                ),
            );
            let mut reached = Vec::new();
            e.iterative_deepening_search(params, |depth, _, _, _| reached.push(depth));
            assert_eq!(reached, depths, "{:?}", kind(time::Duration::from_secs(1)));
        }
    }

    #[test]
    fn deepening_to_depth_zero_finds_nothing() {
        let mut e = engine(Board::new());
        let outcome = e.iterative_deepening_search(SearchParameters::to_depth(0), |_, _, _, _| {});
        assert!(matches!(outcome, SearchOutcome::Aborted(None)));
    }

    #[test]
    fn draw_taint_is_still_recorded_and_never_trusted() {
        // the pawn endgame carries the most draw traffic of the bench
        // positions. No tainted stores means propagation broke; a tainted
        // cutoff means a probe path without the refusal guard was added
        let fen = fens::PAWN_ENDGAME;
        let mut e = reference(Board::from_fen(fen).unwrap());
        for depth in 1..=7 {
            completed(e.search(depth));
        }
        assert!(e.ghi().stores > 0, "the search stored nothing");
        assert!(
            e.ghi().tainted_stores > 0,
            "no draw taint was recorded: propagation is broken"
        );
        assert_eq!(
            e.ghi().tainted_score_cutoffs,
            0,
            "a path dependent score was trusted"
        );
        assert!(
            e.ghi().refused_cutoffs > 0,
            "the refusal refused nothing, or refused without counting"
        );
    }

    #[test]
    fn every_taint_word_names_a_policy_and_the_policy_names_it_back() {
        // a report has to be rerunnable from the word its header prints
        for word in ["refuse", "trust", "skip", "rule50"] {
            let config =
                SearchConfig::with_taint(word).unwrap_or_else(|| panic!("{word} is not a policy"));
            assert_eq!(config.taint_word(), word);
        }
        let default = SearchConfig::default();
        assert_eq!(
            SearchConfig::with_taint(default.taint_word()),
            Some(default)
        );
        assert_eq!(SearchConfig::with_taint("maybe"), None);
    }

    #[test]
    fn a_refused_cutoff_still_orders_the_capture_search_by_its_move() {
        // a tainted entry the reference refuses to cut on still names the
        // move to try first. Seeded with each capture in turn, the capture
        // search's tree has to change with the move named; were the refusal
        // read as a miss, every seed would search the same tree. Kiwipete was
        // this fixture until the pair term's refit at a ridge of 3e-7, under
        // which every one of its eight seeds searches the same 681 nodes. The
        // promotions position's six seeds search four different trees
        let board = Board::from_fen(fens::PROMOTIONS).unwrap();
        let mut probe = board.clone();
        let captures: Vec<Play> = board
            .generate_captures()
            .iter()
            .copied()
            .filter(|m| {
                let legal = probe.make_move(m);
                if legal {
                    probe.undo_move();
                }
                legal
            })
            .collect();
        assert!(captures.len() > 2, "too few captures to tell orders apart");
        let mut trees = Vec::new();
        for play in captures {
            let mut e = reference(board.clone());
            assert!(e.transpositions.record_best(
                &e.board,
                play,
                Value::tainted(0),
                1,
                crate::transposition::NO_EVAL
            ));
            e.quiescence_value();
            assert_eq!(e.ghi().refused_cutoffs, 1, "the seed was not refused");
            trees.push(e.nodes);
        }
        trees.sort_unstable();
        trees.dedup();
        assert!(trees.len() > 1, "the refused move did not reach the order");
    }

    #[test]
    fn taint_crosses_a_quiescence_frame_whose_tainted_capture_is_not_last() {
        // a trusting search that cuts on a tainted entry inside a capture
        // tree must taint what flows out of it. The queen forks rook and
        // pawn; the rook is taken first, into a seeded tainted entry, and
        // the pawn capture searched after it must not launder the flag
        let fen = "7k/3q4/8/8/R5P1/8/8/K7 w - - 0 1";
        let seeded = |config: SearchConfig| {
            let mut e = AlphaBeta::with_config(Board::from_fen(fen).unwrap(), TABLE_BYTES, config);
            let mut board = e.board.clone();
            for name in ["a1b1", "d7a4"] {
                let play = play_named(&board, name);
                assert!(board.make_move(&play), "failed to play {}", name);
            }
            let any = play_named(&board, "b1c1");
            assert!(e.transpositions.record_best(
                &board,
                any,
                Value::tainted(0),
                9,
                crate::transposition::NO_EVAL
            ));
            // the root's entry names the king move, so the seeded line is
            // searched first, at the open window, before standing pat could
            // end the frame
            let king = play_named(&e.board, "a1b1");
            assert!(e.transpositions.record_best(
                &e.board,
                king,
                Value::clean(0),
                9,
                crate::transposition::NO_EVAL
            ));
            // the seeding went straight into the table, which counts
            // nothing, so every tainted store here is the search's
            completed(e.search(1));
            e.ghi().tainted_stores
        };
        let trusting = SearchConfig {
            taint: TaintPolicy::Trust,
            ..SearchConfig::reference()
        };
        assert!(
            seeded(trusting) > 0,
            "the taint was laundered between the capture and the root"
        );
        assert_eq!(
            seeded(SearchConfig::reference()),
            0,
            "a refusing search took the tainted cutoff after all"
        );
    }

    #[test]
    fn a_search_told_to_trust_tainted_scores_takes_their_cutoffs() {
        // the switch has to reach the probe
        let fen = fens::PAWN_ENDGAME;
        let trusting = SearchConfig {
            taint: TaintPolicy::Trust,
            ..SearchConfig::reference()
        };
        let mut e = AlphaBeta::with_config(Board::from_fen(fen).unwrap(), TABLE_BYTES, trusting);
        for depth in 1..=7 {
            completed(e.search(depth));
        }
        assert!(
            e.ghi().tainted_score_cutoffs > 0,
            "the scores the search was told to trust cut nothing"
        );
        assert_eq!(
            e.ghi().refused_cutoffs,
            0,
            "a search trusting tainted scores refused one"
        );
    }

    #[test]
    fn the_static_shortcut_looks_at_less_of_the_tree() {
        // the switch has to reach the search
        let mut e = shortcut(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(6));
        assert!(
            e.nodes < cold.nodes,
            "the shortcut searched {} nodes against the reference's {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn the_static_shortcut_is_never_taken_by_a_node_in_check() {
        // hxg7+ Kxg7 Rxh7+ Kxh7 Qf6 mates. Every node of the line after the
        // first is in check with black the material up, so a shortcut that
        // fired in check would answer from that material and lose the mate
        let fen = "r5rk/2p1Nppp/3p3P/pp2p1P1/4P3/2qnPQK1/8/R6R w - - 0 1";
        let mut e = shortcut(Board::from_fen(fen).unwrap());
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(4));

        let mut cold = reference(Board::from_fen(fen).unwrap());
        let expected = completed(cold.search(4));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn a_mate_in_the_window_is_searched_for_rather_than_guessed_at() {
        // once a mate is in hand, later siblings are searched against a
        // mate beta that every eval clears. A canary rather than a
        // discrimination: on every position tried, dropping the guard
        // changed the tree and not the answers, so this holds that the mate
        // distances stay right, not that the guard alone keeps them so
        let fens = [
            "5n1k/5Kpp/8/8/8/8/8/2Q4R w - - 0 1",
            "2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 1",
            "2r3k1/p4p2/3Rp2p/1p2P1pK/8/1P4P1/P3Q2P/1q6 b - - 0 1",
        ];
        for fen in fens {
            let mut e = shortcut(Board::from_fen(fen).unwrap());
            let result = completed(e.search(5));
            let mut cold = reference(Board::from_fen(fen).unwrap());
            let expected = completed(cold.search(5));
            assert!(expected.checkmate_in().is_some(), "{} mates nobody", fen);
            assert_eq!(result.checkmate_in(), expected.checkmate_in(), "{}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "{}",
                fen
            );
        }
    }

    #[test]
    fn the_static_shortcut_is_never_taken_by_a_side_holding_only_pawns() {
        // the trebuchet, mutual zugzwang, where a static floor is exactly
        // what is not true. Neither side ever has a piece, so the arm
        // searches what the reference searches, node for node
        let fen = "8/8/8/4p3/4Pk2/3K4/8/8 w - - 0 1";
        let mut e = shortcut(Board::from_fen(fen).unwrap());
        let result = completed(e.search(7));
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let expected = completed(cold.search(7));
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn passing_looks_at_less_of_the_tree() {
        let mut e = passing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(6));
        assert!(
            e.nodes < cold.nodes,
            "the pass searched {} nodes against the reference's {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn the_depth_term_leaves_the_reduced_search_a_depth_it_can_hold() {
        // `depth - 1 - r` is unsigned at the call site, so every depth a
        // pass is offered at is held to leaving it a number
        let adaptive = SearchConfig::default();
        for depth in NULL_MOVE_MIN_DEPTH..=u8::MAX {
            for eval_beta in [0, 1, 199, 200, 399, 400, 599, 600, 5_000, Score::MAX] {
                let r = null_move_reduction(adaptive, depth, eval_beta);
                assert!(
                    r < depth,
                    "depth {depth} at a margin of {eval_beta} reduced by {r}, \
                     which the subtraction cannot hold"
                );
                assert!(
                    r >= NULL_MOVE_REDUCTION,
                    "depth {depth} at a margin of {eval_beta} reduced by {r}, \
                     under the flat base"
                );
            }
        }
    }

    #[test]
    fn the_depth_term_grows_the_reduction_and_the_switch_holds_it_flat() {
        // off the switch every depth reads the base, on it the base plus a
        // sixth of the depth
        let flat = SearchConfig {
            adaptive_null_move: false,
            ..SearchConfig::default()
        };
        for depth in [3, 4, 5, 6, 9, 11, 12, 18] {
            assert_eq!(
                null_move_reduction(flat, depth, 0),
                NULL_MOVE_REDUCTION,
                "depth {depth} off the switch"
            );
            assert_eq!(
                null_move_reduction(flat, depth, 600),
                NULL_MOVE_REDUCTION,
                "depth {depth} off the switch at a wide margin"
            );
        }
        for (depth, expected) in [(3, 2), (5, 2), (6, 3), (9, 3), (11, 3), (12, 4), (18, 5)] {
            assert_eq!(
                null_move_reduction(SearchConfig::default(), depth, 0),
                expected,
                "depth {depth} on the switch at no margin"
            );
        }
    }

    #[test]
    fn the_margin_term_steps_every_two_pawns_and_stops_at_its_cap() {
        // at depth 18 the clamp cannot reach and the depth term gives 5
        let deep = 18;
        for (eval_beta, expected) in [
            (0, 5),
            (199, 5),
            (200, 6),
            (399, 6),
            (400, 7),
            (599, 7),
            (600, 8),
            (5_000, 8),
            (Score::MAX, 8),
        ] {
            assert_eq!(
                null_move_reduction(SearchConfig::default(), deep, eval_beta),
                expected,
                "depth {deep} at a margin of {eval_beta}"
            );
        }
    }

    #[test]
    fn the_margin_term_is_clamped_where_the_depth_cannot_hold_it() {
        // the clamp leaves the reduced search at depth zero, quiescence
        for (depth, expected) in [(3, 2), (4, 3), (5, 4), (6, 5), (7, 6), (8, 6)] {
            assert_eq!(
                null_move_reduction(SearchConfig::default(), depth, 600),
                expected,
                "depth {depth} at the cap's margin"
            );
        }
    }

    #[test]
    fn the_depth_term_looks_at_less_of_the_tree_than_the_flat_reduction() {
        // the switch has to reach the search. Depth 8, because the term
        // first moves at a node of depth 6
        let mut e = passing_adaptively(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(8));
        let mut flat = passing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(flat.search(8));
        assert!(
            e.nodes < flat.nodes,
            "the grown reduction searched {} nodes against the flat one's {}",
            e.nodes,
            flat.nodes
        );
    }

    #[test]
    fn a_mate_found_through_a_grown_pass_does_not_come_back_as_one() {
        // `a_mate_found_through_a_pass_does_not_come_back_as_one` at a
        // depth the term moves
        let board = Board::from_fen("7k/5K1N/8/8/8/8/Q7/8 w - - 0 1").unwrap();
        let mut e = passing_adaptively(board);
        let beta = e.eval();
        let Ok(value) = e.alpha_beta(beta - 1, beta, 6, true, RootBounds::Neither) else {
            panic!("nothing was armed to abort this search");
        };
        assert!(
            value.score >= beta,
            "the pass did not fail high, so nothing was clamped: {}",
            value.score
        );
        assert!(
            value.score < CHECKMATE_THRESHOLD,
            "a mate proved only through a pass came back as one: {}",
            value.score
        );
    }

    #[test]
    fn a_side_holding_only_pawns_never_passes() {
        // the trebuchet again: in zugzwang a pass is better than every move
        // there is, so a reduced search of one proves nothing
        let fen = "8/8/8/4p3/4Pk2/3K4/8/8 w - - 0 1";
        let mut e = passing(Board::from_fen(fen).unwrap());
        let result = completed(e.search(7));
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let expected = completed(cold.search(7));
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn a_mate_only_a_move_refutes_survives_the_pass() {
        // the sacrifice line above, with the pass in place of the margin
        let fen = "r5rk/2p1Nppp/3p3P/pp2p1P1/4P3/2qnPQK1/8/R6R w - - 0 1";
        let mut e = passing(Board::from_fen(fen).unwrap());
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(4));

        let mut cold = reference(Board::from_fen(fen).unwrap());
        let expected = completed(cold.search(4));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn a_mate_found_through_a_pass_does_not_come_back_as_one() {
        // after a pass black has to take the knight and Qh2 mates, so the
        // reduced search comes back with a mate white never had. Beta is
        // the eval, the largest the pass is tried under, so only the mate
        // clears it. Asked of the node directly, because by the time the
        // root has searched its moves the real mate outscores the invented
        // one
        let board = Board::from_fen("7k/5K1N/8/8/8/8/Q7/8 w - - 0 1").unwrap();
        let mut e = passing(board);
        let beta = e.eval();
        let Ok(value) = e.alpha_beta(beta - 1, beta, 5, true, RootBounds::Neither) else {
            panic!("nothing was armed to abort this search");
        };
        assert!(
            value.score >= beta,
            "the pass did not fail high, so nothing was clamped: {}",
            value.score
        );
        assert!(
            value.score < CHECKMATE_THRESHOLD,
            "a mate proved only through a pass came back as one: {}",
            value.score
        );
    }

    /// One ply short of the fifty move horizon. Every move white has resets
    /// the counter and the pass does not, so whatever taint comes out of a
    /// node here came out of the pass.
    const ONLY_A_PASS_READS_THE_DRAW: &str = "1k6/8/8/8/8/5p1p/4P1PP/6NK w - - 99 60";

    #[test]
    fn a_cutoff_from_a_pass_carries_the_pass_taint() {
        // the pass reads the draw, which clears a beta of zero
        let board = Board::from_fen(ONLY_A_PASS_READS_THE_DRAW).unwrap();
        let mut e = passing(board);
        let Ok(value) = e.alpha_beta(-1, 0, 3, true, RootBounds::Neither) else {
            panic!("nothing was armed to abort this search");
        };
        assert_eq!(value, Value::tainted(0));
    }

    #[test]
    fn a_pass_that_failed_still_taints_the_node() {
        // beta at the eval, so the pass fails and the moves are searched,
        // and the node still carries the draw the pass read
        let board = Board::from_fen(ONLY_A_PASS_READS_THE_DRAW).unwrap();
        let mut e = passing(board);
        let beta = e.eval();
        assert!(beta > 0, "the pass has to fail, so beta must beat a draw");
        let Ok(value) = e.alpha_beta(beta - 1, beta, 3, true, RootBounds::Neither) else {
            panic!("nothing was armed to abort this search");
        };
        assert!(value.tainted, "the failed pass left no taint behind it");
    }

    /// The child the reduction's seam is driven at: the pawn push from a
    /// position with three quiet moves and nothing to capture, so every
    /// leaf is one quiescence node and the counts below are exact.
    const REDUCIBLE_CHILD: &str = "8/8/8/8/8/8/2k4P/K7 w - - 0 1";

    /// An engine of the configuration given, stood on that child.
    fn at_reducible_child(config: SearchConfig) -> AlphaBeta {
        let mut e = AlphaBeta::with_config(
            Board::from_fen(REDUCIBLE_CHILD).unwrap(),
            TABLE_BYTES,
            config,
        );
        let m = play_named(&e.board, "h2h3");
        assert!(e.board.make_move(&m));
        e
    }

    /// The scout on its own, as `windowed` asks it: what it costs and what
    /// it answers, from the parent's side.
    fn scout(e: &mut AlphaBeta, alpha: Score, depth: u8) -> (u64, Value) {
        let Ok(value) = e.alpha_beta(
            -alpha - 1,
            -alpha,
            depth - 1 - LATE_MOVE_REDUCTION,
            true,
            RootBounds::Neither,
        ) else {
            panic!("an unlimited search aborted");
        };
        (e.nodes, -value)
    }

    #[test]
    fn a_late_quiet_is_scouted_a_ply_shallower_and_answered_by_a_scout_that_fails_low() {
        // `windowed` driven at the child with the reduction asked for
        // directly. Alpha stands well above the move, so the scout fails
        // low and the call costs exactly the scout's nodes
        const DEPTH: u8 = 3;
        let mut oracle = at_reducible_child(SearchConfig::reference());
        let Ok(exact) = oracle.windowed(
            Score::MIN + 2,
            Score::MAX,
            DEPTH,
            &Decision::First,
            RootBounds::Both,
        ) else {
            panic!("an unlimited search aborted");
        };
        let alpha = exact.score + 500;
        assert!(!crate::engine::is_mate(alpha));

        let mut alone = at_reducible_child(SearchConfig::reference());
        let (scout_nodes, scout_value) = scout(&mut alone, alpha, DEPTH);
        assert!(scout_value.score <= alpha, "the scout did not fail low");

        let mut e = at_reducible_child(SearchConfig::reference());
        let Ok(value) = e.windowed(
            alpha,
            alpha + 1,
            DEPTH,
            &later(LATE_MOVE_REDUCTION),
            RootBounds::Neither,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, scout_nodes);
        assert_eq!(value, scout_value);

        let mut probe = at_reducible_child(SearchConfig::reference());
        let Ok(unreduced) = probe.windowed(alpha, alpha + 1, DEPTH, &later(0), RootBounds::Neither)
        else {
            panic!("an unlimited search aborted");
        };
        assert!(unreduced.score <= alpha);
        assert!(
            probe.nodes > scout_nodes,
            "the probe cost {} nodes against the scout's {}",
            probe.nodes,
            scout_nodes
        );
    }

    #[test]
    fn a_scout_that_fails_high_is_re_searched_at_full_depth() {
        // a window the move sits inside, so the scout fails high. The
        // reduced call costs the scout and then an unreduced call on the
        // table the scout left (the second engine), and answers the exact
        // score
        const DEPTH: u8 = 3;
        let mut oracle = at_reducible_child(SearchConfig::reference());
        let Ok(exact) = oracle.windowed(
            Score::MIN + 2,
            Score::MAX,
            DEPTH,
            &Decision::First,
            RootBounds::Both,
        ) else {
            panic!("an unlimited search aborted");
        };
        let alpha = exact.score - 500;
        let beta = exact.score + 50;
        assert!(!crate::engine::is_mate(alpha) && !crate::engine::is_mate(beta));

        let mut alone = at_reducible_child(SearchConfig::reference());
        let (scout_nodes, scout_value) = scout(&mut alone, alpha, DEPTH);
        assert!(scout_value.score > alpha, "the scout did not fail high");

        let mut then_probed = at_reducible_child(SearchConfig::reference());
        scout(&mut then_probed, alpha, DEPTH);
        let Ok(unreduced) =
            then_probed.windowed(alpha, beta, DEPTH, &later(0), RootBounds::Neither)
        else {
            panic!("an unlimited search aborted");
        };
        assert!(
            then_probed.nodes > scout_nodes,
            "nothing was searched after the scout"
        );

        let mut e = at_reducible_child(SearchConfig::reference());
        let Ok(value) = e.windowed(
            alpha,
            beta,
            DEPTH,
            &later(LATE_MOVE_REDUCTION),
            RootBounds::Neither,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, then_probed.nodes);
        assert_eq!(value.score, unreduced.score);
        assert_eq!(value.score, exact.score);
    }

    #[test]
    fn a_node_in_check_searches_what_the_reference_searches() {
        // seven quiet evasions. Asked at depth two the node extends to the
        // floor and its children can reduce nothing, so the arm searches
        // what the reference searches if and only if the node in check
        // declines to reduce (`late_move::tests::a_node_in_check_reduces_nothing`)
        let fen = "4r2k/8/8/8/8/8/2Q2N2/4K3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        assert!(board.in_check());
        let evasions = board.evasions();
        assert!(evasions.len() > LATE_MOVE_THRESHOLD, "{}", evasions.len());
        assert!(evasions.iter().all(|m| m.capture.is_none()));

        let mut e = reducing(Board::from_fen(fen).unwrap());
        let Ok(value) = e.alpha_beta(
            -10_000,
            10_000,
            LATE_MOVE_MIN_DEPTH - 1,
            true,
            RootBounds::Neither,
        ) else {
            panic!("an unlimited search aborted");
        };
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let Ok(expected) = cold.alpha_beta(
            -10_000,
            10_000,
            LATE_MOVE_MIN_DEPTH - 1,
            true,
            RootBounds::Neither,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(value, expected);
    }

    #[test]
    fn the_mate_window_searches_what_the_reference_searches() {
        // a zero window inside the mate scores puts every node at a mate
        // edge, so none reduces, where an ordinary window reduces plenty.
        // Depth five puts the grandchildren at the floor with alpha the
        // mate in hand (`late_move::tests::the_mate_window_stands_the_reduction_down`)
        let fen = SHARP_MIDDLEGAME;
        let mut e = reducing(Board::from_fen(fen).unwrap());
        let Ok(value) = e.alpha_beta(29_500, 29_501, 5, true, RootBounds::Neither) else {
            panic!("an unlimited search aborted");
        };
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let Ok(expected) = cold.alpha_beta(29_500, 29_501, 5, true, RootBounds::Neither) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(value, expected);

        let mut e = reducing(Board::from_fen(fen).unwrap());
        assert!(e.alpha_beta(-1, 0, 5, true, RootBounds::Neither).is_ok());
        let mut cold = reference(Board::from_fen(fen).unwrap());
        assert!(cold.alpha_beta(-1, 0, 5, true, RootBounds::Neither).is_ok());
        assert!(
            e.nodes < cold.nodes,
            "nothing was reduced outside the mate window: {} against {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn reducing_looks_at_less_of_the_tree() {
        let mut e = reducing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(6));
        assert!(
            e.nodes < cold.nodes,
            "the reduction searched {} nodes against the reference's {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn a_dead_quiet_is_scouted_two_plies_shallower_and_answered_by_its_scout() {
        // the seam driven with two plies at the deep floor: the scout fails
        // low, the call costs exactly its nodes, and the one ply scout
        // beside it is dearer
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH;
        let mut oracle = at_reducible_child(SearchConfig::reference());
        let Ok(exact) = oracle.windowed(
            Score::MIN + 2,
            Score::MAX,
            DEPTH,
            &Decision::First,
            RootBounds::Both,
        ) else {
            panic!("an unlimited search aborted");
        };
        let alpha = exact.score + 500;
        assert!(!crate::engine::is_mate(alpha));

        let mut alone = at_reducible_child(SearchConfig::reference());
        let Ok(scout_value) = alone.alpha_beta(
            -alpha - 1,
            -alpha,
            DEPTH - 1 - DEEP_REDUCTION,
            true,
            RootBounds::Neither,
        ) else {
            panic!("an unlimited search aborted");
        };
        let scout_value = -scout_value;
        let scout_nodes = alone.nodes;
        assert!(scout_value.score <= alpha, "the scout did not fail low");

        let mut e = at_reducible_child(SearchConfig::reference());
        let Ok(value) = e.windowed(
            alpha,
            alpha + 1,
            DEPTH,
            &later(DEEP_REDUCTION),
            RootBounds::Neither,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, scout_nodes);
        assert_eq!(value, scout_value);

        let mut shallower = at_reducible_child(SearchConfig::reference());
        let Ok(one_ply) = shallower.windowed(
            alpha,
            alpha + 1,
            DEPTH,
            &later(LATE_MOVE_REDUCTION),
            RootBounds::Neither,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert!(one_ply.score <= alpha);
        assert!(
            shallower.nodes > scout_nodes,
            "the one ply scout cost {} nodes against the two ply scout's {}",
            shallower.nodes,
            scout_nodes
        );
    }

    #[test]
    fn the_deep_reduction_looks_at_less_of_the_tree() {
        // measured against the rung below it
        let mut e = deep_reducing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut one_ply = reducing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(one_ply.search(6));
        assert!(
            e.nodes < one_ply.nodes,
            "the deep reduction searched {} nodes against the flat reduction's {}",
            e.nodes,
            one_ply.nodes
        );
    }

    #[test]
    fn the_pruning_reaches_the_search() {
        // a different tree rather than a smaller one: skipping a move
        // changes what the table and the ordering hold, so the pruning can
        // cost nodes
        let mut e = pruning(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut deep = deep_reducing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(deep.search(6));
        assert_ne!(
            e.nodes, deep.nodes,
            "the skip changed no node, so it did not reach the search"
        );
    }

    #[test]
    fn a_node_that_skips_its_dead_quiets_still_answers_from_the_front() {
        // a skipped move can lower a node's answer and never raise it, so
        // where the first moves carry the answer the skip changes only the
        // cost
        let mut e = pruning(Board::from_fen(fens::A_CAPTURE_AND_QUIETS).unwrap());
        let pruned = completed(e.search(6));
        let mut deep = deep_reducing(Board::from_fen(fens::A_CAPTURE_AND_QUIETS).unwrap());
        let whole = completed(deep.search(6));
        assert_eq!(pruned.score, whole.score);
        assert_eq!(pruned.best_move, whole.best_move);
        assert!(
            e.nodes <= deep.nodes,
            "the pruning searched {} nodes against the deep reduction's {}",
            e.nodes,
            deep.nodes
        );
    }

    #[test]
    fn the_quiet_memories_look_at_less_of_the_tree_for_the_same_answer() {
        // nothing here prunes, so the root's score stands and only the tree
        // may move
        let mut e = remembering(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        let result = completed(e.search(6));
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        let expected = completed(cold.search(6));
        assert_eq!(result.score, expected.score);
        assert!(
            e.nodes < cold.nodes,
            "the memories searched {} nodes against the reference's {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn the_quiet_memories_refuse_a_ply_past_the_rail() {
        // a search no longer arrives past the rail, so the ply is set by hand
        let mut e = remembering(Board::new());
        assert_eq!(e.memory_ply(), Some(0));
        e.board.line_ply = MAX_PLY as usize - 1;
        assert_eq!(e.memory_ply(), Some(MAX_PLY as usize - 1));
        e.board.line_ply = MAX_PLY as usize;
        assert_eq!(e.memory_ply(), None);

        let mut off = reference(Board::new());
        off.board.line_ply = 3;
        assert_eq!(off.memory_ply(), None);
    }

    #[test]
    fn a_cutoff_is_credited_to_the_side_that_played_it() {
        // at depth two every full width cutoff is a quiet black reply to a
        // white root move, so the history must be black's alone and the
        // killers at ply one exactly: the update sites read the board after
        // the move is unmade. Black is asked for a credited entry rather than
        // a positive total, since the malus on the moves tried before a
        // cutoff can outweigh the credit
        let mut e = remembering(Board::new());
        completed(e.search(2));
        assert_eq!(e.ordering.history_total(Color::White), 0);
        assert!(e.ordering.history_credited(Color::Black) > 0);
        assert_eq!(e.ordering.killers_at(0), [None; 2]);
        assert!(e.ordering.killers_at(1).iter().any(|k| k.is_some()));
        assert_eq!(e.ordering.killers_at(2), [None; 2]);
    }

    #[test]
    fn the_moves_a_node_tried_before_its_cutoff_reach_the_history() {
        // only a marked down move puts an entry under zero, so a negative
        // entry says the move loop hands the table the moves it tried and
        // not only the one that cut
        let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(3));
        let marked = e.ordering.history_marked_down(Color::White)
            + e.ordering.history_marked_down(Color::Black);
        assert!(marked > 0, "nothing was marked down");

        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(3));
        assert_eq!(cold.ordering.history_marked_down(Color::White), 0);
        assert_eq!(cold.ordering.history_marked_down(Color::Black), 0);
    }

    #[test]
    fn every_search_starts_with_the_quiet_memories_empty() {
        // a killer from the position before would order this one. Both
        // entry points empty them
        fn run(e: &mut AlphaBeta, deepened: bool) -> SearchResult {
            let depth = 5;
            if deepened {
                let options = SearchParameters::to_depth(depth);
                completed(e.iterative_deepening_search(options, |_, _, _, _| {}))
            } else {
                completed(e.search(depth))
            }
        }

        for deepened in [false, true] {
            let mut warm = remembering(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
            run(&mut warm, deepened);
            warm.board = Board::new();
            warm.clear_transpositions();
            let result = run(&mut warm, deepened);

            let mut cold = remembering(Board::new());
            let expected = run(&mut cold, deepened);
            assert_eq!(result.nodes, expected.nodes, "deepened: {deepened}");
            assert_eq!(result.score, expected.score, "deepened: {deepened}");
        }
    }

    /// Whether `played` is worth `score` to the side to move in `fen`, asked
    /// a ply below the root of an engine with nothing in its table.
    fn worth(fen: &str, played: &Play, score: Score) -> bool {
        let mut board = Board::from_fen(fen).unwrap();
        assert!(
            board.make_move(played),
            "{} is not legal in {}",
            played,
            fen
        );
        -completed(reference(board).search(5)).score == score
    }

    #[test]
    fn a_warm_cache_matches_cold_across_draw_context() {
        // the key ignores the fifty move counter, so a search a few plies
        // from the draw fills the table with scores true of that path only.
        // Six of white's moves are worth the answer and the near-draw table
        // reorders the search, so the move is asserted by what it is worth
        // rather than by name
        let near_draw = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 96 112";
        let fresh = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 0 1";
        let mut warm = reference(Board::from_fen(near_draw).unwrap());
        completed(warm.search(6));
        warm.parse_fen(fresh).unwrap();
        let result = completed(warm.search(6));

        let mut cold = reference(Board::from_fen(fresh).unwrap());
        let expected = completed(cold.search(6));
        assert_eq!(result.score, expected.score);
        assert!(
            worth(fresh, &result.best_move, expected.score),
            "the warm search returned {}, which is not worth {}",
            result.best_move,
            expected.score
        );
    }

    #[test]
    fn a_skipping_search_matches_cold_across_draw_context_with_nothing_to_refuse() {
        // the skip policy keeps tainted scores out of the table, so it owes
        // the same answer warm as cold without the refusal firing. On the
        // reference, since the answer is only owed there
        let near_draw = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 96 112";
        let fresh = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 0 1";
        let skipping = SearchConfig {
            taint: TaintPolicy::Skip,
            ..SearchConfig::reference()
        };
        let mut warm =
            AlphaBeta::with_config(Board::from_fen(near_draw).unwrap(), TABLE_BYTES, skipping);
        completed(warm.search(6));
        assert!(warm.ghi().skipped_stores > 0, "nothing was ever skipped");
        // the root's answer slot is stored whatever its taint
        assert!(
            warm.ghi().tainted_stores <= 1,
            "a tainted score was kept beyond the root's answer slot"
        );
        warm.parse_fen(fresh).unwrap();
        let result = completed(warm.search(6));

        let mut cold =
            AlphaBeta::with_config(Board::from_fen(fresh).unwrap(), TABLE_BYTES, skipping);
        let expected = completed(cold.search(6));
        assert_eq!(result.score, expected.score);
        assert!(
            worth(fresh, &result.best_move, expected.score),
            "the warm search returned {}, which is not worth {}",
            result.best_move,
            expected.score
        );
    }

    #[test]
    fn the_root_stores_a_tainted_answer_under_the_skipping_policy() {
        // one half move short of the fifty move draw with no capture on
        // the board, so every reply is the tainted draw and nothing under
        // the root stores. The root's answer is stored past the policy,
        // since the reported line is read back from its slot
        let fen = "4k3/8/8/8/8/8/8/R3K3 w - - 99 120";
        let skipping = SearchConfig {
            taint: TaintPolicy::Skip,
            ..SearchConfig::reference()
        };
        let mut e = AlphaBeta::with_config(Board::from_fen(fen).unwrap(), TABLE_BYTES, skipping);
        assert_eq!(completed(e.search(3)).score, 0);
        let ghi = e.ghi();
        assert_eq!((ghi.stores, ghi.tainted_stores), (1, 1));
        assert_eq!(ghi.skipped_stores, 0, "a store under the root was offered");
    }

    #[test]
    fn a_warm_cache_matches_a_cold_search() {
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        ];
        let mut warm = reference(Board::new());
        for fen in fens {
            warm.parse_fen(fen).unwrap();
            completed(warm.search(5));
        }
        for fen in fens {
            let game = Board::from_fen(fen).unwrap();
            let mut cold = reference(game);
            let expected = completed(cold.search(5));
            warm.parse_fen(fen).unwrap();
            let result = completed(warm.search(5));
            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
        }
    }

    #[test]
    fn a_small_table_matches_a_large_table() {
        let fen = SHARP_MIDDLEGAME;
        let mut big = reference(Board::from_fen(fen).unwrap());
        let expected = completed(big.search(5));
        let mut small = AlphaBeta::with_config(
            Board::from_fen(fen).unwrap(),
            8 * 1024,
            SearchConfig::reference(),
        );
        let result = completed(small.search(5));
        assert_eq!(result.score, expected.score);
    }

    #[test]
    fn a_search_at_a_repetition_still_returns_a_move() {
        let game = Board::from_fen(fens::SHUFFLE).unwrap();
        let mut e = engine(game);
        for m in [
            "a8b8", "a1b1", "b8a8", "b1a1", "a8b8", "a1b1", "b8a8", "b1a1",
        ] {
            assert_eq!(e.make_move_str(m), Ok(()), "failed to play {}", m);
        }
        assert!(e.board.is_repetition());
        assert!(matches!(e.search(3), SearchOutcome::Complete(_, _)));
    }

    #[test]
    fn a_new_game_forgets_the_previous_game() {
        let fen = SHARP_MIDDLEGAME;
        let mut e = engine(Board::from_fen(fen).unwrap());
        completed(e.search(4));
        assert!(
            e.transpositions.ordering_play(&e.board).is_some(),
            "nothing was stored"
        );
        assert_ne!(format!("{}", e.pv_line()), "");

        e.new_game();
        assert!(e.transpositions.ordering_play(&e.board).is_none());
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_is_empty_without_a_cache_entry() {
        let game = Board::new();
        let e = engine(game);
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_stops_at_a_repetition() {
        // a shuffle leaves the table holding a line that goes round for ever
        let game = Board::from_fen(fens::SHUFFLE).unwrap();
        let mut e = engine(game);
        let cycle = ["a8b8", "a1b1", "b8a8", "b1a1"];
        let mut board = e.board.clone();
        for name in cycle.iter().cycle().take(16) {
            let play = play_named(&board, name);
            assert!(e.transpositions.record_best(
                &board,
                play,
                Value::clean(0),
                SEEDED_DEPTH,
                crate::transposition::NO_EVAL
            ));
            assert!(board.make_move(&play), "failed to play {}", name);
        }

        assert_eq!(format!("{}", e.pv_line()), "a8b8 a1b1 b8a8 b1a1");
    }

    #[test]
    fn the_pv_line_stops_when_the_fifty_move_counter_runs_out() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let mut board = e.board.clone();
        for name in ["c3d4", "f8g8"] {
            let play = play_named(&board, name);
            assert!(e.transpositions.record_best(
                &board,
                play,
                Value::clean(0),
                SEEDED_DEPTH,
                crate::transposition::NO_EVAL
            ));
            assert!(board.make_move(&play), "failed to play {}", name);
        }
        assert!(board.fifty_move_expired());

        assert_eq!(format!("{}", e.pv_line()), "c3d4");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_move_which_is_illegal_here() {
        // a colliding entry's move need not belong to this position
        let mut e = engine(Board::new());
        let a2 = 8;
        let a5 = 32;
        let colliding = Play::new(a2, a5, None, None, false, false);
        assert!(e.transpositions.record_best(
            &e.board,
            colliding,
            Value::clean(0),
            SEEDED_DEPTH,
            crate::transposition::NO_EVAL
        ));

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_quiescence_entry() {
        // a depth zero entry's move is fit for ordering and not for saying
        // what the engine means to play
        let mut e = engine(Board::new());
        let play = play_named(&e.board, "e2e4");
        assert!(e.transpositions.record_best(
            &e.board,
            play,
            Value::clean(0),
            0,
            crate::transposition::NO_EVAL
        ));

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_move_which_leaves_the_king_in_check() {
        // a pinned piece's move is in the pseudo legal list and still cannot
        // be played
        let board = Board::from_fen("4r2k/8/8/8/8/8/4N3/4K3 w - - 0 1").unwrap();
        let mut e = engine(board);
        let pinned = play_named(&e.board, "e2d4");
        assert!(e.transpositions.record_best(
            &e.board,
            pinned,
            Value::clean(0),
            SEEDED_DEPTH,
            crate::transposition::NO_EVAL
        ));

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_is_bounded_by_the_ply_rail() {
        // a line longer than the rail that never repeats and never runs the
        // fifty move counter out, so nothing but the bound can end it. Pawn
        // moves first, since they reset the counter
        let mut e = engine(Board::new());
        let mut board = e.board.clone();
        let wanted = crate::engine::MAX_PLY as usize + 4;
        for ply in 0..wanted {
            let moves = board.generate_moves();
            let mut chosen: Option<Play> = None;
            for pawns_first in [true, false] {
                for m in &moves {
                    if (board.get_piece_index(m.from) == Some(Piece::Pawn)) != pawns_first {
                        continue;
                    }
                    if !board.make_move(m) {
                        continue;
                    }
                    let carries_on = !board.has_repeated() && !board.fifty_move_expired();
                    board.undo_move();
                    if carries_on {
                        chosen = Some(*m);
                        break;
                    }
                }
                if chosen.is_some() {
                    break;
                }
            }
            let play =
                chosen.unwrap_or_else(|| panic!("nothing carries the line on at ply {}", ply));
            assert!(e.transpositions.record_best(
                &board,
                play,
                Value::clean(0),
                SEEDED_DEPTH,
                crate::transposition::NO_EVAL
            ));
            assert!(board.make_move(&play), "failed to play {}", play);
        }

        assert_eq!(e.pv_line().line.len(), crate::engine::MAX_PLY as usize);
    }

    #[test]
    fn quiescence_resolves_captures_past_the_depth_it_used_to_stop_at() {
        // the old cap was twenty plies from the root. Which search depth
        // clears it with room moves with every pruning change
        let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        let result = completed(e.search(10));
        assert!(
            result.selective_depth > 20,
            "quiescence stopped at {} plies",
            result.selective_depth
        );
    }

    #[test]
    fn a_stopped_search_does_not_poison_the_cache() {
        let fen = SHARP_MIDDLEGAME;
        let game = Board::from_fen(fen).unwrap();
        let mut cold = reference(game);
        let expected = completed(cold.search(6));

        let game = Board::from_fen(fen).unwrap();
        let mut e = reference(game);
        assert!(matches!(
            e.search_within(6, already_spent()),
            SearchOutcome::Aborted(_)
        ));

        let result = completed(e.search(6));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }
}

/// The residual sampler seen from the search: off unless asked for, and
/// recording the nodes the shortcuts answered and the candidates the margin
/// was measured against.
mod sampling {
    use crate::board::fens::SHARP_MIDDLEGAME;
    use crate::engine::{
        AlphaBeta, Board, Engine, REVERSE_FUTILITY_MARGIN, REVERSE_FUTILITY_MAX_DEPTH, RootBounds,
        Score, SearchConfig, SearchParameters, Taint,
    };
    use crate::recorder::{Sampled, Sampler, Window};
    use crate::residual::{Sample, Shortcut, sample_key};
    use pretty_assertions::assert_eq;

    const TABLE_BYTES: usize = 1024 * 1024;

    fn engine(fen: &str) -> AlphaBeta {
        AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES)
    }

    /// The shortcut frame driven on its own with the bounds named, and what
    /// the sampler recorded of it. The only way to hold the beta column to
    /// the beta the gate read: a sample's evaluation column is measured
    /// against the beta column, so every identity between them survives a
    /// wrong bound.
    fn shortcut_at(config: SearchConfig, alpha: Score, beta: Score, depth: u8) -> Vec<Sample> {
        let mut e = AlphaBeta::with_config(
            Board::from_fen(SHARP_MIDDLEGAME).unwrap(),
            TABLE_BYTES,
            config,
        );
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            alpha,
            beta,
            depth,
            false,
            true,
            RootBounds::Neither,
            &mut taint,
            &mut None,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_some(), "no shortcut fired at depth {}", depth);
        collected(&mut e).taken
    }

    /// An engine nobody asked samples of holds no sampler.
    #[test]
    fn an_engine_samples_nothing_until_it_is_asked_to() {
        let mut e = engine(SHARP_MIDDLEGAME);
        assert!(e.sampler.is_none());
        let reference =
            AlphaBeta::with_config(Board::new(), TABLE_BYTES, SearchConfig::reference());
        assert!(reference.sampler.is_none());
        e.search(5);
        assert!(e.disarm::<Sample>().is_none());
    }

    fn collected(e: &mut AlphaBeta) -> Sampled<Sample> {
        e.disarm::<Sample>()
            .expect("a sampler was installed")
            .drain()
    }

    /// The one sample of a kind in what was taken.
    fn one_of(taken: &[Sample], kind: Shortcut) -> &Sample {
        let mut of_kind = taken.iter().filter(|s| s.kind == kind);
        let sample = of_kind
            .next()
            .unwrap_or_else(|| panic!("nothing taken for {}", kind.word()));
        assert!(
            of_kind.next().is_none(),
            "more than one {} sample",
            kind.word()
        );
        sample
    }

    #[test]
    fn every_sample_describes_a_node_a_hook_offered() {
        const DEPTH: u8 = 5;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(DEPTH);
        let sampled = collected(&mut e);
        assert!(!sampled.taken.is_empty(), "the hooks offered nothing");
        for sample in &sampled.taken {
            Board::from_fen(&sample.fen)
                .unwrap_or_else(|e| panic!("{} does not parse: {}", sample.fen, e));
            assert!(Shortcut::KINDS.contains(&sample.kind), "{:?}", sample);
            // the root deepens by one when it is in check
            assert!(sample.depth >= 1, "{:?}", sample);
            assert!(sample.depth <= DEPTH + 1, "{:?}", sample);
        }
    }

    /// The decision columns, taken from the node the shortcut answered. A
    /// live claim clears its beta; a shadow claim need not, but its
    /// evaluation stands at or above beta.
    ///
    /// Reverse futility claims `eval - margin * depth`, so `claimed - beta`
    /// and `eval_beta - margin * depth` are the same number. That catches a
    /// column built from the wrong evaluation or depth, not the wrong bound,
    /// which `the_recorded_beta_is_the_one_the_gate_cleared` holds.
    #[test]
    fn every_sample_carries_the_decision_it_was_taken_at() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(5);
        for sample in collected(&mut e).taken {
            match sample.kind {
                Shortcut::ReverseFutility | Shortcut::NullMove => {
                    assert!(sample.claimed >= sample.beta, "{:?}", sample);
                }
                Shortcut::ShadowFutility => {
                    assert!(sample.eval_beta >= 0, "{:?}", sample);
                }
            }
            let board = Board::from_fen(&sample.fen).expect("the fen parses");
            assert_eq!(sample.halfmove, board.halfmove_clock(), "{:?}", sample);
            if matches!(
                sample.kind,
                Shortcut::ReverseFutility | Shortcut::ShadowFutility
            ) {
                assert_eq!(
                    i32::from(sample.claimed) - i32::from(sample.beta),
                    sample.eval_beta - i32::from(REVERSE_FUTILITY_MARGIN) * i32::from(sample.depth),
                    "{:?}",
                    sample
                );
            }
        }
    }

    /// The rate is a key and not a counter: every node recorded at a rate of
    /// four keys into the first quarter of the range.
    #[test]
    fn a_rate_records_only_the_nodes_its_keys_fall_under() {
        const EVERY: u32 = 4;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(EVERY));
        e.search(5);
        let taken = collected(&mut e).taken;
        assert!(!taken.is_empty(), "the rate turned everything away");
        for sample in taken {
            let board = Board::from_fen(&sample.fen).expect("the fen parses");
            assert!(
                sample_key(board.key, sample.kind, sample.depth) <= u64::MAX / u64::from(EVERY),
                "{:?}",
                sample
            );
        }
    }

    /// The beta a row states is the beta the gate cleared, and the window
    /// beside it is read from the node's real bounds, which are a zero
    /// window, since an open one is exempt. Both shortcuts, because they
    /// are two call sites. The evaluation is read from the engine, so the
    /// test knows the number the columns are held to before the shortcut
    /// runs.
    #[test]
    fn the_recorded_beta_is_the_one_the_gate_cleared() {
        let eval = engine(SHARP_MIDDLEGAME).eval();

        // the margin at depth one claims `eval - 100`, which clears a beta
        // two hundred under the evaluation
        let beta = eval - 200;
        let taken = shortcut_at(SearchConfig::default(), beta - 1, beta, 1);
        assert_eq!(taken.len(), 2);
        let fired = one_of(&taken, Shortcut::ReverseFutility);
        assert_eq!(fired.beta, beta);
        assert_eq!(fired.eval_beta, 200);
        assert_eq!(fired.claimed, eval - REVERSE_FUTILITY_MARGIN);
        assert_eq!(fired.window, Window::Zero);

        // the pass, with the margin off so nothing answers the node first
        let passing = SearchConfig {
            reverse_futility: false,
            ..SearchConfig::default()
        };
        let beta = eval - 600;
        let taken = shortcut_at(passing, beta - 1, beta, 3);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].kind, Shortcut::NullMove);
        assert_eq!(taken[0].beta, beta);
        assert_eq!(taken[0].eval_beta, 600);
        assert_eq!(taken[0].window, Window::Zero);
    }

    /// The same beta answers the node at a zero window with neither bound
    /// marked, and nothing when it is still the root's own or the window is
    /// open, before the eval is read.
    #[test]
    fn an_exempt_node_answers_no_shortcut() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        // past the margin's depth, so the pass is what answers
        let beta = eval - 600;
        let taken = shortcut_at(SearchConfig::default(), beta - 1, beta, 5);
        assert!(taken.iter().any(|s| s.kind == Shortcut::NullMove));

        // the root's beta at a zero window, then the proof's bounds at an
        // open one (its alpha is the root's and its beta is not), then one
        // under the open window and the other side of it
        for (alpha, root_bounds) in [
            (beta - 1, RootBounds::Beta),
            (beta - 500, RootBounds::Alpha),
            (beta - 2, RootBounds::Neither),
        ] {
            let mut e = engine(SHARP_MIDDLEGAME);
            e.arm(Sampler::<Sample>::every(1));
            let mut taint = Taint::default();
            let Ok(answered) = e.shortcuts(
                alpha,
                beta,
                5,
                false,
                true,
                root_bounds,
                &mut taint,
                &mut None,
            ) else {
                panic!("nothing here searches under a limit, so nothing can abort");
            };
            assert!(
                answered.is_none(),
                "a shortcut answered {root_bounds:?} at alpha {alpha}"
            );
            assert_eq!(e.nodes, 0, "the refusal searched something");
            assert!(collected(&mut e).taken.is_empty());
        }
    }

    /// A candidate the margin declines is recorded all the same: only the
    /// shadow sees the candidates a smaller margin would add.
    #[test]
    fn a_candidate_under_the_margin_is_shadowed_and_not_answered() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        // a beta fifty under the evaluation is a candidate the margin
        // declines at depth one
        let beta = eval - 50;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            beta - 1,
            beta,
            1,
            false,
            true,
            RootBounds::Neither,
            &mut taint,
            &mut None,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_none(), "the margin fired under its floor");
        let taken = collected(&mut e).taken;
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].kind, Shortcut::ShadowFutility);
        assert_eq!(taken[0].beta, beta);
        assert_eq!(taken[0].eval_beta, 50);
        assert_eq!(taken[0].claimed, eval - REVERSE_FUTILITY_MARGIN);
        assert!(taken[0].claimed < taken[0].beta);
    }

    /// A node the evaluation leaves below beta is no candidate, since no
    /// non-negative margin can fire on it.
    #[test]
    fn a_node_below_beta_is_not_a_candidate() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval + 50;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            beta - 1,
            beta,
            1,
            false,
            true,
            RootBounds::Neither,
            &mut taint,
            &mut None,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_none());
        assert!(collected(&mut e).taken.is_empty());
    }

    /// The evaluation the shortcut frame hands back to the move loop comes
    /// off the memoised door, and the late move decision's own read is
    /// `eval::eval`; a node would decide on a different number if the two
    /// ever parted.
    #[test]
    fn the_evaluation_handed_to_the_loop_is_the_direct_one() {
        let mut e =
            AlphaBeta::with_table_bytes(Board::from_fen(SHARP_MIDDLEGAME).unwrap(), TABLE_BYTES);
        assert!(e.config.quiet_futility, "the default carries the rule");
        let direct = crate::eval::eval(&e.board);
        // beta at the evaluation, so the gates pass and the margin does not
        // answer
        let beta = direct;
        let mut taint = Taint::default();
        let mut eval = None;
        let Ok(answered) = e.shortcuts(
            beta - 1,
            beta,
            1,
            false,
            true,
            RootBounds::Neither,
            &mut taint,
            &mut eval,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_none(), "the margin answered the node");
        assert_eq!(eval, Some(direct));
    }

    /// A cut the guard declines is searched instead. At depth four with the
    /// eval exactly the margin above beta the guard declines the cut, so the
    /// pass is searched in its place and no reverse futility row is taken.
    /// With the guard off the margin answers the same node and searches
    /// nothing.
    #[test]
    fn a_cut_the_guard_declines_goes_on_to_the_pass() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let depth = REVERSE_FUTILITY_MAX_DEPTH;
        let beta = eval - REVERSE_FUTILITY_MARGIN * depth as Score;
        let run = |config: SearchConfig| {
            let mut e = AlphaBeta::with_config(
                Board::from_fen(SHARP_MIDDLEGAME).unwrap(),
                TABLE_BYTES,
                config,
            );
            let declines = e.guard_declines(eval, beta, depth);
            e.arm(Sampler::<Sample>::every(1));
            let mut taint = Taint::default();
            let Ok(answered) = e.shortcuts(
                beta - 1,
                beta,
                depth,
                false,
                true,
                RootBounds::Neither,
                &mut taint,
                &mut None,
            ) else {
                panic!("nothing here searches under a limit, so nothing can abort");
            };
            (declines, answered, e.nodes, collected(&mut e).taken)
        };

        let unguarded = SearchConfig {
            reverse_futility_guard: false,
            ..SearchConfig::default()
        };
        let (declines, answered, nodes, taken) = run(unguarded);
        assert!(!declines, "the guard declined with the switch off");
        assert_eq!(answered.map(|value| value.score), Some(beta));
        assert_eq!(nodes, 0, "the margin's answer searched something");
        assert_eq!(one_of(&taken, Shortcut::ReverseFutility).claimed, beta);

        let (declines, answered, nodes, taken) = run(SearchConfig::default());
        assert!(declines, "the guard kept the cut");
        assert!(nodes > 0, "the declined cut was not searched");
        assert!(
            taken.iter().all(|s| s.kind != Shortcut::ReverseFutility),
            "a declined cut was sampled as a cut: {taken:?}"
        );
        one_of(&taken, Shortcut::ShadowFutility);
        // what answers now is the pass or, when it fails, the node's moves
        // (which the frame hands back to the loop as no answer)
        let passed = taken.iter().any(|s| s.kind == Shortcut::NullMove);
        assert_eq!(answered.is_some(), passed, "{taken:?}");
    }

    /// A fired candidate is two rows, the live kind's and the shadow's,
    /// claiming the same number against the same beta.
    #[test]
    fn a_fired_candidate_is_shadowed_with_the_same_claim() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval - 200;
        let taken = shortcut_at(SearchConfig::default(), beta - 1, beta, 1);
        assert_eq!(taken.len(), 2);
        let live = one_of(&taken, Shortcut::ReverseFutility);
        let shadow = one_of(&taken, Shortcut::ShadowFutility);
        assert_eq!(shadow.claimed, live.claimed);
        assert_eq!(shadow.beta, live.beta);
        assert_eq!(shadow.eval_beta, live.eval_beta);
        assert_eq!(shadow.window, live.window);
        assert_eq!(shadow.fen, live.fen);
    }

    /// The margin's depth gate bounds the shadow too. The shallower samples
    /// come from the pass's reduced search.
    #[test]
    fn the_shadow_keeps_to_the_margins_depths() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval - 600;
        let taken = shortcut_at(SearchConfig::default(), beta - 1, beta, 5);
        assert!(
            taken
                .iter()
                .any(|s| s.kind == Shortcut::NullMove && s.depth == 5),
            "the pass did not answer the node"
        );
        for sample in &taken {
            if sample.kind != Shortcut::NullMove {
                assert!(sample.depth <= REVERSE_FUTILITY_MAX_DEPTH, "{:?}", sample);
            }
        }
    }

    /// The window a real search hands the hook. An open window is exempt
    /// from both shortcuts, so every row a search records carries a zero
    /// one, the shadow's among them.
    #[test]
    fn the_windows_a_search_records_are_the_zero_ones() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(6);
        let taken = collected(&mut e).taken;
        assert!(!taken.is_empty());
        let open: Vec<_> = taken.iter().filter(|s| s.window == Window::Open).collect();
        assert!(open.is_empty(), "an open window was sampled: {open:?}");
    }

    /// Every kind reaches the hook, not only whichever fires first.
    #[test]
    fn all_kinds_are_recorded() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(6);
        let sampled = collected(&mut e);
        for kind in Shortcut::KINDS {
            assert!(
                sampled.taken.iter().any(|s| s.kind == kind),
                "nothing recorded for {}",
                kind.word()
            );
        }
    }

    /// A key and not a draw, so a distribution is reproducible.
    #[test]
    fn two_runs_of_the_same_search_record_the_same_samples() {
        let run = || {
            let mut e = engine(SHARP_MIDDLEGAME);
            e.arm(Sampler::<Sample>::every(7));
            e.iterative_deepening_search(SearchParameters::to_depth(5), |_, _, _, _| {});
            collected(&mut e)
        };
        assert_eq!(run(), run());
    }

    /// The cap holds and says how much it dropped.
    #[test]
    fn a_search_past_the_cap_stops_growing_and_counts_the_rest() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::with_cap(1, 4));
        e.search(5);
        let sampled = collected(&mut e);
        assert_eq!(sampled.taken.len(), 4);
        assert!(sampled.overflowed > 0);
    }

    /// The margin each shortcut is betting on, recorded at the node it fired
    /// at: at least zero for the pass, at least the whole margin for
    /// reverse futility. The weaker bound would pass on a column that had
    /// lost its depth scaling.
    #[test]
    fn the_recorded_distance_is_the_evaluation_over_beta() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(5);
        for sample in collected(&mut e).taken {
            let floor = match sample.kind {
                Shortcut::ReverseFutility => {
                    i32::from(REVERSE_FUTILITY_MARGIN) * i32::from(sample.depth)
                }
                Shortcut::NullMove | Shortcut::ShadowFutility => 0,
            };
            assert!(sample.eval_beta >= floor, "{:?} under {}", sample, floor);
        }
    }
}

/// Moves for teaching the move memories by hand, shared by the census and
/// ledger tests.
mod taught {
    use crate::engine::AlphaBeta;
    use crate::play::Play;

    /// A from and to square pair no move in the list uses, for teaching
    /// the (butterfly indexed) history an entry the list cannot read.
    pub(super) fn unmade_journey(moves: &[Play]) -> Play {
        (0u8..64)
            .flat_map(|from| (0u8..64).map(move |to| (from, to)))
            .find(|(from, to)| from != to && !moves.iter().any(|m| m.from == *from && m.to == *to))
            .map(|(from, to)| Play::new(from, to, None, None, false, false))
            .expect("a list cannot hold every journey")
    }

    /// Two quiet moves of the position, for teaching the memories.
    pub(super) fn quiets(e: &AlphaBeta) -> (Play, Play) {
        let moves = e.board.generate_moves();
        let mut quiets = moves
            .iter()
            .filter(|m| m.capture.is_none() && m.promote.is_none());
        let first = *quiets.next().expect("a quiet move");
        let second = *quiets.next().expect("another quiet move");
        (first, second)
    }
}

/// The cutoff census seen from the search: off unless asked for, and a row
/// reads the node as it stood when it answered. The recorder is driven
/// directly, with the memories taught by hand, so a row's history column
/// can be held to a history the test chose.
mod cutoffs {
    use super::taught::{quiets, unmade_journey};
    use crate::board::fens::SHARP_MIDDLEGAME;
    use crate::census::{self, Class, Cutting, Table};
    use crate::engine::{AlphaBeta, Board, FailSoft, Node, RootBounds, Score, SearchConfig};
    use crate::play::Play;
    use crate::recorder::{Sampler, Window};
    use crate::value::{Taint, Value};
    use pretty_assertions::assert_eq;

    const TABLE_BYTES: usize = 1024 * 1024;

    fn engine(fen: &str) -> AlphaBeta {
        let mut e = AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES);
        e.arm(Sampler::<census::Event>::every(1));
        e
    }

    /// A node as the loop hands it to the recorder: opened at these facts
    /// with `searched` moves made and searched, out of check.
    fn node(
        depth: u8,
        (alpha, beta): (Score, Score),
        ply: Option<usize>,
        tt: Table,
        searched: usize,
        entered_at: u64,
    ) -> Node {
        let mut node = Node::open(
            depth,
            false,
            ply,
            tt,
            entered_at,
            FailSoft::open(alpha, beta, RootBounds::Neither, Taint::default()),
        );
        node.answer.searched = searched;
        node
    }

    /// An engine nobody asked a census of holds none.
    #[test]
    fn an_engine_records_no_census_until_it_is_asked_to() {
        let mut e =
            AlphaBeta::with_table_bytes(Board::from_fen(SHARP_MIDDLEGAME).unwrap(), TABLE_BYTES);
        assert!(e.census.is_none());
        let reference =
            AlphaBeta::with_config(Board::new(), TABLE_BYTES, SearchConfig::reference());
        assert!(reference.census.is_none());
        e.search(4);
        assert!(e.disarm::<census::Event>().is_none());
    }

    /// A killer cutting at index 1, with the memories taught by hand: the
    /// row says index 1 and class killer, its history is the table's entry
    /// for the move, and the largest history among the generated quiets
    /// stands beside it as the denominator.
    #[test]
    fn a_row_reads_the_memories_as_they_stood_at_the_cutoff() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (killer, cool) = quiets(&e);
        let color = e.board.active_color;
        // the first is taught at another ply with the larger history, so
        // only the killer slot makes the second the class
        e.ordering.cutoff(color, &cool, &[], 1, 5);
        e.ordering.cutoff(color, &killer, &[], 0, 4);
        let moves = e.board.generate_moves();
        let (alpha, beta): (Score, Score) = (10, 11);
        e.census_event(
            &node(3, (alpha, beta), Some(0), Table::Miss, 2, e.nodes),
            &moves,
            true,
            Some(Cutting {
                play: &killer,
                reduced: false,
                table: false,
            }),
        );
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        assert_eq!(sampled.taken.len(), 1);
        assert_eq!(sampled.events, 1);
        let row = &sampled.taken[0];
        let cut = row.cut.as_ref().expect("the node cut");
        assert_eq!(cut.index, 1);
        assert_eq!(cut.class, Class::Killer);
        assert_eq!(cut.history, 16);
        assert!(!cut.reduced);
        assert_eq!(row.history_max, 25);
        assert_eq!(row.generated, moves.len());
        assert_eq!(row.searched, 2);
        assert_eq!(row.window, Window::Zero);
        assert!(!row.in_check);
        assert!(row.quiets_scored);
        assert_eq!(row.tt, Table::Miss);
        assert_eq!(row.fen, e.board.to_fen());
        assert_eq!(
            row.eval_beta,
            i32::from(crate::eval::eval(&e.board)) - i32::from(beta)
        );
        assert_eq!(row.cost, 0);
    }

    /// A cutting move the table has marked down: the row prints the signed
    /// entry, and its denominator is the largest clamped at zero, which is
    /// zero when every quiet in the list has been marked down.
    #[test]
    fn a_marked_down_cutting_move_is_read_against_no_denominator() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (cut, _) = quiets(&e);
        let color = e.board.active_color;
        let moves = e.board.generate_moves();
        // the bonus lands on a journey no move here makes, and every quiet
        // in the list is marked down
        let elsewhere = unmade_journey(&moves);
        let marked: Vec<Play> = moves
            .iter()
            .filter(|m| m.capture.is_none() && m.promote.is_none())
            .copied()
            .collect();
        e.ordering.cutoff(color, &elsewhere, &marked, 1, 4);
        e.census_event(
            &node(3, (10, 11), Some(0), Table::Miss, 2, e.nodes),
            &moves,
            true,
            Some(Cutting {
                play: &cut,
                reduced: false,
                table: false,
            }),
        );
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        let row = &sampled.taken[0];
        let cutting = row.cut.as_ref().expect("the node cut");
        assert_eq!(cutting.class, Class::Quiet);
        assert_eq!(cutting.history, -16);
        assert_eq!(row.history_max, 0);
        assert!(cutting.history <= row.history_max);
    }

    /// The table's move cutting before anything was generated: index 0,
    /// class table whatever the move is, and a row that says the node
    /// holds no list.
    #[test]
    fn a_table_move_cutoff_is_recorded_with_nothing_generated() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let take = *e
            .board
            .generate_moves()
            .iter()
            .find(|m| m.capture.is_some())
            .expect("a capture");
        e.census_event(
            &node(4, (10, 11), None, Table::Move, 1, e.nodes),
            &[],
            false,
            Some(Cutting {
                play: &take,
                reduced: false,
                table: true,
            }),
        );
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        let row = &sampled.taken[0];
        let cut = row.cut.as_ref().expect("the node cut");
        assert_eq!(cut.index, 0);
        assert_eq!(cut.class, Class::Table);
        // a capture reads no history, however it is classed
        assert_eq!(cut.history, 0);
        assert_eq!(row.generated, 0);
        assert_eq!(row.searched, 1);
        assert_eq!(row.history_max, 0);
        assert_eq!(row.tt, Table::Move);
    }

    /// A node whose alpha rose records the window it opened with: an open
    /// window stays open when a move raises alpha to a point under beta.
    #[test]
    fn a_row_records_the_window_the_node_opened_with() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let moves = e.board.generate_moves();
        let mut node = node(2, (-50, 60), Some(0), Table::Miss, 0, e.nodes);
        node.answer.absorb(&m, Value::clean(59));
        assert_eq!((node.answer.alpha, node.answer.beta), (59, 60));
        e.census_event(&node, &moves, true, None);
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        let row = &sampled.taken[0];
        assert_eq!(row.window, Window::Open);
        assert_eq!(row.searched, 1);
    }

    /// A node the loop finished: no cutting move, and the rest of the
    /// portrait still there, the quiets' largest history included.
    #[test]
    fn a_held_node_records_no_cutting_move() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (taught, _) = quiets(&e);
        e.ordering.cutoff(e.board.active_color, &taught, &[], 0, 3);
        let moves = e.board.generate_moves();
        e.census_event(
            &node(
                2,
                (-50, 60),
                Some(0),
                Table::ScoreOnly,
                moves.len(),
                e.nodes,
            ),
            &moves,
            true,
            None,
        );
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        let row = &sampled.taken[0];
        assert!(row.cut.is_none());
        assert_eq!(row.searched, moves.len());
        assert_eq!(row.window, Window::Open);
        assert_eq!(row.history_max, 9);
        assert_eq!(row.tt, Table::ScoreOnly);
    }
}

/// The reduction ledger seen from the search: off unless asked for, and a
/// row reads the decision as the node made it. The staging at the parent
/// and `windowed` at the child are driven directly, with the memories
/// taught by hand.
mod reductions {
    use super::taught::{quiets, unmade_journey};
    use crate::board::fens::SHARP_MIDDLEGAME;
    use crate::census::Table;
    use crate::engine::{AlphaBeta, Board, Decision, FailSoft, Node, RootBounds, Score};
    use crate::late_move;
    use crate::play::Play;
    use crate::recorder::{Sampler, Window};
    use crate::reduction::{self, Scout};
    use crate::value::{Taint, Value};
    use pretty_assertions::assert_eq;

    const TABLE_BYTES: usize = 1024 * 1024;

    fn engine(fen: &str) -> AlphaBeta {
        let mut e = AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES);
        e.arm(Sampler::<reduction::Event>::every(1));
        e
    }

    /// The staging the move loop would hand the scout. The features read
    /// neither the depth nor the bounds, so both stand at nothing.
    fn staged(e: &AlphaBeta, m: &Play, searched: usize, ply: Option<usize>) -> reduction::Staged {
        let moves = e.board.generate_moves();
        let mut node = Node::open(
            0,
            false,
            ply,
            Table::Miss,
            0,
            FailSoft::open(0, 1, RootBounds::Neither, Taint::default()),
        );
        node.answer.searched = searched;
        let mut rules = late_move::Rules::new(&e.deciding(), &node, None);
        e.staged_reduction(m, &node, &mut rules, &moves)
    }

    /// A later move scouted `reduction` plies shallower with the ledger's
    /// staging, as the loop hands it to the scout.
    fn staging(reduction: u8, staged: reduction::Staged) -> Decision {
        Decision::Search {
            reduction,
            staged: Some(staged),
        }
    }

    /// An engine nobody asked a ledger of holds none.
    #[test]
    fn an_engine_records_no_ledger_until_it_is_asked_to() {
        let mut e =
            AlphaBeta::with_table_bytes(Board::from_fen(SHARP_MIDDLEGAME).unwrap(), TABLE_BYTES);
        assert!(e.ledger.is_none());
        e.search(4);
        assert!(e.disarm::<reduction::Event>().is_none());
    }

    /// A staged scout that fails low: the row carries the features the node
    /// knew, the fen of the position the move left, and the node's own eval
    /// against its bounds. The board comes back exactly as the recorder
    /// found it after stepping back for that eval.
    #[test]
    fn a_row_reads_the_decision_as_the_node_made_it() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (killer, cool) = quiets(&e);
        let color = e.board.active_color;
        // the first is taught at another ply with the larger history, so
        // only the killer slot makes the second the class
        e.ordering.cutoff(color, &cool, &[], 1, 5);
        e.ordering.cutoff(color, &killer, &[], 0, 4);
        let moves = e.board.generate_moves();
        let parent_eval = i32::from(crate::eval::eval(&e.board));
        let staged = staged(&e, &killer, 5, Some(0));
        assert!(e.board.make_move(&killer));
        let child_fen = e.board.to_fen();
        let child_key = e.board.key;
        let (alpha, beta): (Score, Score) = (5000, 5001);
        let Ok(value) = e.windowed(alpha, beta, 3, &staging(1, staged), RootBounds::Neither) else {
            panic!("an unlimited search aborted");
        };
        assert!(value.score <= alpha, "the scout did not fail low");
        assert_eq!(e.board.to_fen(), child_fen);
        assert_eq!(e.board.key, child_key);
        let sampled = e
            .disarm::<reduction::Event>()
            .expect("a ledger was installed")
            .drain();
        assert_eq!(sampled.events, 1);
        assert_eq!(sampled.taken.len(), 1);
        let row = &sampled.taken[0];
        assert_eq!(row.fen, child_fen);
        assert_eq!(row.depth, 3);
        assert_eq!(row.window, Window::Zero);
        assert_eq!(row.index, 5);
        assert_eq!(row.searched, 6);
        assert_eq!(row.generated, moves.len());
        assert_eq!(row.history, 16);
        assert_eq!(row.history_max, 25);
        assert!(row.killer);
        assert_eq!(row.tt, Table::Miss);
        assert_eq!(row.eval_beta, parent_eval - i32::from(beta));
        assert_eq!(row.alpha_gap, i32::from(alpha) - parent_eval);
        assert_eq!(row.alpha, alpha);
        assert_eq!(row.scout, Scout::Low);
        assert!(row.cost >= 1);
        assert_eq!(row.reduction, 1);
    }

    /// A skipped move is recorded against the bounds as the move is
    /// reached: alpha as a move before it raised it, not the alpha the node
    /// opened with.
    #[test]
    fn a_skipped_row_reads_the_alpha_standing() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, raiser) = quiets(&e);
        let parent_eval = i32::from(crate::eval::eval(&e.board));
        let staged = staged(&e, &m, 5, Some(0));
        let mut node = Node::open(
            3,
            false,
            Some(0),
            Table::Miss,
            e.nodes,
            FailSoft::open(-50, 60, RootBounds::Neither, Taint::default()),
        );
        node.answer.absorb(&raiser, Value::clean(10));
        assert_eq!(node.answer.alpha, 10);
        let fen = e.board.to_fen();
        e.ledger_skip(staged, &node);
        assert_eq!(e.board.to_fen(), fen, "the board was not left as it was");
        let sampled = e
            .disarm::<reduction::Event>()
            .expect("a ledger was installed")
            .drain();
        assert_eq!(sampled.taken.len(), 1);
        let row = &sampled.taken[0];
        assert_eq!(row.scout, Scout::Skipped);
        assert_eq!(row.depth, 3);
        assert_eq!(row.alpha, 10);
        assert_eq!(row.window, Window::Open);
        assert_eq!(row.alpha_gap, 10 - parent_eval);
        assert_eq!(row.eval_beta, parent_eval - 60);
    }

    /// A reduced move the table has marked down stages the signed entry
    /// and a denominator clamped at zero.
    #[test]
    fn a_marked_down_move_stages_a_signed_history_and_no_denominator() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let color = e.board.active_color;
        let moves = e.board.generate_moves();
        let elsewhere = unmade_journey(&moves);
        let marked: Vec<Play> = moves
            .iter()
            .filter(|q| q.capture.is_none() && q.promote.is_none())
            .copied()
            .collect();
        e.ordering.cutoff(color, &elsewhere, &marked, 1, 4);
        let staged = staged(&e, &m, 5, Some(0));
        assert_eq!(staged.features.history, -16);
        assert_eq!(staged.features.history_max, 0);
    }

    /// The reduction column reads what `windowed` was handed.
    #[test]
    fn the_row_carries_the_reduction_the_scout_ran_at() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let staged = staged(&e, &m, 6, None);
        assert!(e.board.make_move(&m));
        let (alpha, beta): (Score, Score) = (5000, 5001);
        let Ok(value) = e.windowed(alpha, beta, 4, &staging(2, staged), RootBounds::Neither) else {
            panic!("an unlimited search aborted");
        };
        assert!(value.score <= alpha, "the scout did not fail low");
        let sampled = e
            .disarm::<reduction::Event>()
            .expect("a ledger was installed")
            .drain();
        assert_eq!(sampled.taken.len(), 1);
        let row = &sampled.taken[0];
        assert_eq!(row.depth, 4);
        assert_eq!(row.reduction, 2);
        // the depth the move was denied is the node's less one, however
        // short the scout ran
        assert_eq!(row.replay_depth(), 3);
    }

    /// A scout that fails high is recorded so, and its cost counts the
    /// scout alone rather than the full depth search it asked for.
    #[test]
    fn a_scout_that_fails_high_is_recorded_as_high() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let staged = staged(&e, &m, 4, None);
        assert!(e.board.make_move(&m));
        // the tree under the scout records skip rows of its own, so the
        // staged row is picked out by the position it left
        let left = e.board.to_fen();
        let (alpha, beta): (Score, Score) = (-5000, -4999);
        let Ok(_) = e.windowed(alpha, beta, 3, &staging(1, staged), RootBounds::Neither) else {
            panic!("an unlimited search aborted");
        };
        let sampled = e
            .disarm::<reduction::Event>()
            .expect("a ledger was installed")
            .drain();
        let rows: Vec<&reduction::Event> =
            sampled.taken.iter().filter(|row| row.fen == left).collect();
        assert_eq!(rows.len(), 1);
        let row = rows[0];
        assert_eq!(row.scout, Scout::High);
        assert_eq!(row.index, 4);
        assert!(!row.killer);
        assert_eq!(row.history, 0);
        assert!(
            row.cost < e.nodes,
            "the cost {} counts more than the scout of a search of {}",
            row.cost,
            e.nodes
        );
    }

    /// The exemption threaded through the recursion. The bounds sit inside
    /// the mate scores, so only the root bounds can keep a scout off the
    /// root's beta, and a state dropped or a turn forgotten shows up as an
    /// open node reducing against a beta of twenty thousand. A state kept
    /// where it should clear only adds refusals and is invisible here;
    /// `what_a_child_carries_and_what_a_raise_leaves` pins that.
    ///
    /// Twenty thousand is above anything the evaluation produces, so an
    /// open window carrying it can only have the root's beta. A zero window
    /// can carry it under a node whose first child was mated, which is why
    /// the count is of open rows. The second half shows the same search
    /// with neither bound marked does reduce against that beta.
    #[test]
    fn no_open_node_reduces_against_a_beta_that_is_still_the_roots() {
        const ALPHA: Score = -20_000;
        const BETA: Score = 20_000;

        // every row, since a count of none is a claim about all of them
        fn rows_at_the_roots_beta(root_bounds: RootBounds) -> (usize, usize) {
            let mut e = AlphaBeta::with_table_bytes(
                Board::from_fen(SHARP_MIDDLEGAME).unwrap(),
                TABLE_BYTES,
            );
            e.arm(Sampler::<reduction::Event>::with_cap(1, usize::MAX));
            let Ok(_) = e.alpha_beta(ALPHA, BETA, 6, true, root_bounds) else {
                panic!("an unlimited search aborted");
            };
            let sampled = e
                .disarm::<reduction::Event>()
                .expect("a ledger was installed")
                .drain();
            assert_eq!(sampled.overflowed, 0, "the ledger described a share");
            let at_beta = sampled
                .taken
                .iter()
                .filter(|row| {
                    row.window == Window::Open
                        && i32::from(row.alpha) - row.alpha_gap - row.eval_beta == i32::from(BETA)
                })
                .count();
            (at_beta, sampled.taken.len())
        }

        let (marked, rows) = rows_at_the_roots_beta(RootBounds::Both);
        assert!(rows > 0, "the tree held no reduction to read either way");
        assert_eq!(marked, 0, "a scout was reduced against the root's beta");

        let (unmarked, _) = rows_at_the_roots_beta(RootBounds::Neither);
        assert!(
            unmarked > 0,
            "nothing reduced against that beta with neither bound marked, \
             so the count above was no claim"
        );
    }
}

/// A search's fail soft answer as its moves come back, read with no board.
mod fail_soft {
    use crate::engine::{FailSoft, Reached, RootBounds, Score};
    use crate::play::Play;
    use crate::value::{Taint, Value};
    use pretty_assertions::assert_eq;

    fn open(alpha: Score, beta: Score, root_bounds: RootBounds) -> FailSoft {
        FailSoft::open(alpha, beta, root_bounds, Taint::default())
    }

    /// A move for the answer to name. It reads the value and not the move.
    fn any_move() -> Play {
        Play::new(12, 28, None, None, false, false)
    }

    /// Alpha and the root bounds move together at a rise, and a later
    /// move scoring less lowers neither. Started from the two states where
    /// alpha is still the root's, so a rise that forgot the bounds shows.
    #[test]
    fn a_rise_moves_alpha_and_the_root_bounds_together_and_nothing_lowers_them() {
        let m = any_move();
        for (from, to) in [
            (RootBounds::Both, RootBounds::Beta),
            (RootBounds::Alpha, RootBounds::Neither),
        ] {
            let mut answer = open(-100, 100, from);
            // under alpha, and at it, raise nothing
            assert_eq!(answer.absorb(&m, Value::clean(-150)), Reached::Neither);
            assert_eq!(answer.absorb(&m, Value::clean(-100)), Reached::Neither);
            assert_eq!((answer.alpha, answer.root_bounds), (-100, from));
            assert_eq!(answer.absorb(&m, Value::clean(20)), Reached::Alpha);
            assert_eq!((answer.alpha, answer.root_bounds), (20, to));
            assert_eq!(answer.absorb(&m, Value::clean(10)), Reached::Neither);
            assert_eq!((answer.alpha, answer.root_bounds), (20, to));
            assert_eq!(answer.absorb(&m, Value::clean(50)), Reached::Alpha);
            assert_eq!((answer.alpha, answer.root_bounds), (50, to));
            // a cutoff answers the search and leaves the bounds where they
            // stood
            assert_eq!(answer.absorb(&m, Value::clean(100)), Reached::Beta);
            assert_eq!(
                (answer.alpha, answer.beta, answer.root_bounds),
                (50, 100, to)
            );
            assert_eq!(answer.searched, 6);
        }
    }

    /// The answer is a ceiling until a move beats the alpha it opened
    /// with, and a score or a floor from then on.
    #[test]
    fn alpha_is_raised_after_a_rise_and_not_before() {
        let m = any_move();
        let mut answer = open(-50, 50, RootBounds::Neither);
        assert!(!answer.raised_alpha());
        answer.absorb(&m, Value::clean(-60));
        assert!(!answer.raised_alpha(), "a move under alpha");
        answer.absorb(&m, Value::clean(-50));
        assert!(!answer.raised_alpha(), "a move at alpha");
        answer.absorb(&m, Value::clean(0));
        assert!(answer.raised_alpha());
        answer.absorb(&m, Value::clean(-60));
        assert!(answer.raised_alpha(), "a move after the rise");
    }

    /// The stand pat is the best so far and a floor under alpha, with no
    /// move and no count. It is not a rise: the answer stays a ceiling
    /// until a capture beats it. A stand pat under alpha leaves alpha.
    #[test]
    fn the_stand_pat_sets_best_and_alpha_and_counts_no_move() {
        let m = any_move();
        let mut answer = open(-50, 50, RootBounds::Neither);
        answer.stand_pat(20);
        assert_eq!((answer.best, answer.alpha), (20, 20));
        assert_eq!((answer.best_move, answer.searched), (None, 0));
        assert!(!answer.raised_alpha(), "the stand pat read as a rise");
        assert_eq!(answer.absorb(&m, Value::clean(10)), Reached::Neither);
        assert_eq!((answer.best, answer.best_move), (20, None));
        assert!(!answer.raised_alpha(), "a capture under the stand pat");
        assert_eq!(answer.absorb(&m, Value::clean(30)), Reached::Alpha);
        assert!(answer.raised_alpha());

        let mut low = open(-50, 50, RootBounds::Neither);
        low.stand_pat(-80);
        assert_eq!((low.best, low.alpha, low.searched), (-80, -50, 0));
        assert!(!low.raised_alpha());
    }

    /// A fresh answer has searched nothing until a move is absorbed, and
    /// one move absorbed counts however badly it scored. Quiescence in
    /// check and the root read a side with no legal move off that count.
    #[test]
    fn an_answer_counts_nothing_searched_until_a_move_is_absorbed() {
        let mut answer = open(-50, 50, RootBounds::Neither);
        assert_eq!((answer.searched, answer.best_move), (0, None));
        answer.absorb(&any_move(), Value::mated(3));
        assert_eq!(answer.searched, 1);
    }
}

/// A full width node's answer as its moves come back, read off the node
/// with no search behind it but the one the table's move asks for.
mod node {
    use crate::board::play_named;
    use crate::census::Table;
    use crate::engine::{AlphaBeta, Board, FailSoft, Node, RootBounds, Score};
    use crate::value::Taint;
    use pretty_assertions::assert_eq;

    fn open(depth: u8, alpha: Score, beta: Score, root_bounds: RootBounds) -> Node {
        Node::open(
            depth,
            false,
            Some(0),
            Table::Miss,
            0,
            FailSoft::open(alpha, beta, root_bounds, Taint::default()),
        )
    }

    /// The searched count is what the reductions read as a move's index:
    /// the table's move is counted when it was made, and a table move that
    /// turns out illegal is not. The knight is pinned to its king, so its
    /// move is pseudo legal and refused when made.
    #[test]
    fn the_searched_count_takes_the_tables_move_and_not_an_illegal_one() {
        let board = Board::from_fen("4r1k1/8/8/8/8/8/4N3/4K3 w - - 0 1").unwrap();
        let mut e = AlphaBeta::with_table_bytes(board, 1024 * 1024);
        let pinned = play_named(&e.board, "e2c3");
        let step = play_named(&e.board, "e1d1");
        assert!(e.board.is_pseudo_legal(&pinned) && e.board.is_pseudo_legal(&step));
        let mut node = open(2, -20_000, 20_000, RootBounds::Neither);
        assert!(matches!(
            e.search_table_move(pinned, &mut node, crate::transposition::NO_EVAL),
            Ok(None)
        ));
        assert_eq!(node.answer.searched, 0, "the illegal move was counted");
        assert!(!node.answer.raised_alpha());
        assert!(matches!(
            e.search_table_move(step, &mut node, crate::transposition::NO_EVAL),
            Ok(None)
        ));
        assert_eq!(node.answer.searched, 1, "the table's move was not counted");
    }
}
