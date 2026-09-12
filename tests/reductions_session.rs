// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the reductions argument prints, run against the real binary.
//!
//! The spawning and the splitting are in `report_command`; here is what the
//! ledger's own header, rows and summary say. The replay behind the fail
//! lows runs a ply under the sampled nodes, so at this depth it costs a
//! stream of shallow reference searches and stays quick.

mod report_command;

/// A shallow run at a rate that still records plenty. Only a late quiet
/// move at depth offers an event, so the stream is sparser than the
/// census's and the rate is lower for it.
const ARGUMENTS: [&str; 4] = ["reductions", "4", "every", "5"];

#[test]
fn the_reductions_argument_prints_a_header_rows_and_a_summary() {
    let printed = report_command::run(&ARGUMENTS);
    assert!(
        printed
            .header
            .starts_with("reductions depth 4 every 5 positions "),
        "header: {}",
        printed.header
    );
    assert!(printed.events() > 0, "header: {}", printed.header);

    let mut replayed = 0;
    let mut low = 0;
    for row in &printed.rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert!(words.len() > 16, "row: {}", row);
        // the depth, the counts, the history pair, the three distances,
        // and the cost are each a number, and the fen comes after them: a
        // row parses left to right
        for at in [0, 2, 3, 4, 5, 6, 9, 10, 11, 13] {
            assert!(words[at].parse::<i64>().is_ok(), "field {} of {}", at, row);
        }
        assert!(words[1] == "zw" || words[1] == "open", "row: {}", row);
        assert!(words[7] == "killer" || words[7] == "plain", "row: {}", row);
        assert!(
            words[8] == "miss" || words[8] == "move" || words[8] == "score_only",
            "row: {}",
            row
        );
        // a late move is what the ledger records, so no index is under
        // the threshold
        assert!(words[2].parse::<usize>().unwrap() >= 4, "row: {}", row);
        // a fail low carries the replay's answer and its label, and so does
        // a skip, which late move pruning records where the loop passes a
        // move over and whose counterfactual the replay builds the same
        // way. A fail high carries neither, said with dashes so the
        // columns stand still
        match words[12] {
            "low" | "skipped" => {
                replayed += 1;
                low += usize::from(words[12] == "low");
                assert!(words[14].parse::<i64>().is_ok(), "row: {}", row);
                assert!(
                    words[15] == "harmful" || words[15] == "harmless",
                    "row: {}",
                    row
                );
            }
            "high" => {
                assert_eq!(words[14], "-", "row: {}", row);
                assert_eq!(words[15], "-", "row: {}", row);
            }
            other => panic!("scout {} in: {}", other, row),
        }
    }
    // the replayed rows are the ones the ledger labels, so a run that kept
    // none has measured nothing. Both are counted, because a run of nothing
    // but skips would leave the scout's own labelling untested
    assert!(low > 0, "no fail low rows in:\n{}", printed.all);
    assert!(replayed > low, "no skipped rows in:\n{}", printed.all);

    // a line a depth, each carrying the whole shape
    for line in &printed.summary {
        assert!(line.starts_with("depth "), "summary line: {}", line);
        for word in [
            " scouts ",
            " low ",
            " share ",
            " replayed ",
            " harmful ",
            " rate ",
            " index4-7 ",
            " index8-15 ",
            " index16+ ",
            " hist0 ",
            " hist<0.1 ",
            " hist<0.5 ",
            " hist0.5+ ",
        ] {
            assert!(line.contains(word), "no {} in: {}", word.trim(), line);
        }
    }
}
