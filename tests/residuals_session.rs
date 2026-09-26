// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the residuals argument prints, run against the real binary. The
//! spawning and the splitting are in `report_command`.

mod report_command;

/// A shallow run at a rate that records plenty without spending the
/// reference search on thousands of rows.
const ARGUMENTS: [&str; 4] = ["residuals", "4", "every", "50"];

#[test]
fn the_residuals_argument_prints_a_header_rows_and_a_summary() {
    let printed = report_command::run(&ARGUMENTS);
    assert!(
        printed
            .header
            .starts_with("residuals depth 4 every 50 taint rule50 positions "),
        "header: {}",
        printed.header
    );
    assert!(printed.events() > 0, "header: {}", printed.header);

    for row in &printed.rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert!(words.len() > 12, "row: {}", row);
        assert!(
            words[0] == "reverse_futility"
                || words[0] == "null_move"
                || words[0] == "shadow_futility",
            "row: {}",
            row
        );
        assert!(words[2] == "zw" || words[2] == "open", "row: {}", row);
        // the numeric columns: depth, the halfmove clock and the five scores
        for at in [1, 3, 4, 5, 6, 7, 8] {
            assert!(words[at].parse::<i32>().is_ok(), "field {} of {}", at, row);
        }
        // the derived columns, which checks the printer and not the
        // measurement
        let beta: i32 = words[4].parse().unwrap();
        let claimed: i32 = words[6].parse().unwrap();
        let reference: i32 = words[7].parse().unwrap();
        assert_eq!(
            words[8].parse::<i32>().unwrap(),
            reference - claimed,
            "{row}"
        );
        let crossed = if reference < beta { "crossed" } else { "clear" };
        assert_eq!(words[9], crossed, "{row}");
        let overstated = if claimed > reference {
            "overstated"
        } else {
            "held"
        };
        assert_eq!(words[10], overstated, "{row}");
    }

    let summary = &printed.summary;
    assert!(summary.len() >= 3, "summary: {:?}", summary);
    let kind_at = |line: &str| {
        if line.starts_with("reverse_futility") {
            0
        } else if line.starts_with("null_move") {
            1
        } else if line.starts_with("shadow_futility") {
            2
        } else {
            panic!("summary line names no kind: {}", line)
        }
    };
    let mut order: Vec<usize> = summary.iter().map(|line| kind_at(line)).collect();
    order.dedup();
    assert_eq!(order, vec![0, 1, 2], "kinds out of order: {:?}", summary);
    assert!(
        summary.iter().any(|line| line.contains(" median ")),
        "no percentiles in: {:?}",
        summary
    );
    // the per-depth crossing rate is the headline the command exists to print
    assert!(
        summary.iter().any(|line| line.contains(" depth ")
            && line.contains(" crossed ")
            && line.contains(" overstated ")
            && line.contains(" mates ")),
        "no per-depth crossing rate in: {:?}",
        summary
    );
}
