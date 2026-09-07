// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the reductions argument prints, run against the real binary.
//!
//! The spawning and the splitting are in `report_command`; here is what the
//! ledger's own header, rows and summary say. The replay behind a row runs
//! a ply under the sampled node and the trials under that, so at this depth
//! it costs a stream of shallow searches and stays quick.

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

    let mut low = 0;
    for row in &printed.rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert!(words.len() > 17, "row: {}", row);
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
        // every row carries the replay's answer now; the label is the
        // search's own decision, so only a fail low has one and a fail
        // high says so with a dash rather than moving the columns
        assert!(words[14].parse::<i64>().is_ok(), "row: {}", row);
        match words[12] {
            "low" => {
                low += 1;
                assert!(
                    words[15] == "harmful" || words[15] == "harmless",
                    "row: {}",
                    row
                );
            }
            "high" => assert_eq!(words[15], "-", "row: {}", row),
            other => panic!("scout {} in: {}", other, row),
        }
        // one word a reduction, comma separated, and a reduction the
        // node had no room for is a dash rather than a missing word
        let trials: Vec<&str> = words[16].split(',').collect();
        assert_eq!(trials.len(), 5, "row: {}", row);
        assert!(
            trials.iter().all(|t| ["low", "high", "-"].contains(t)),
            "row: {}",
            row
        );
        // a depth of at least three is what the reduction floor allows,
        // so the reduction of none is always offered
        assert!(trials[0] != "-", "row: {}", row);
    }
    // the fail lows are what the replay labels, so a run that kept none
    // has measured nothing
    assert!(low > 0, "no fail low rows in:\n{}", printed.all);

    // the summary is two blocks: a line a depth, then a line for each
    // reduction the replay tried at each depth
    let trials_at = printed
        .summary
        .iter()
        .position(|line| line == "trials")
        .unwrap_or_else(|| panic!("no trials block in:\n{}", printed.all));
    let depths = &printed.summary[..trials_at - 1];
    let trials = &printed.summary[trials_at + 1..];
    assert!(!depths.is_empty(), "no depth lines in:\n{}", printed.all);
    assert!(!trials.is_empty(), "no trial lines in:\n{}", printed.all);

    for line in depths {
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

    for line in trials {
        assert!(line.starts_with("depth "), "trial line: {}", line);
        for word in [
            " r ",
            " offered ",
            " low ",
            " share ",
            " harmful ",
            " rate ",
            " band ",
            " bandharmful ",
            " bandrate ",
        ] {
            assert!(line.contains(word), "no {} in: {}", word.trim(), line);
        }
    }
}
