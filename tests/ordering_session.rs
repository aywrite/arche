// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the ordering argument prints, run against the real binary. The
//! spawning and the splitting are in `report_command`.

mod report_command;

/// A shallow run at a rate that still records plenty of groups, with the
/// exploration on.
const ARGUMENTS: [&str; 6] = ["ordering", "4", "every", "20", "seed", "1"];

#[test]
fn the_ordering_argument_prints_a_header_a_row_a_member_and_a_summary() {
    let printed = report_command::run(&ARGUMENTS);
    assert!(
        printed
            .header
            .starts_with("ordering depth 4 every 20 seed 1 positions "),
        "header: {}",
        printed.header
    );
    assert!(printed.events() > 0, "header: {}", printed.header);

    let mut outcomes = std::collections::BTreeSet::new();
    for row in &printed.rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert!(words.len() > 10, "row: {}", row);
        // depth, generated, searched, k, rank and place
        for at in [0, 2, 3, 4, 5, 6] {
            assert!(
                words[at].parse::<usize>().is_ok(),
                "field {} of {}",
                at,
                row
            );
        }
        assert!(words[1] == "zw" || words[1] == "open", "row: {}", row);
        let k: usize = words[4].parse().unwrap();
        assert!(words[5].parse::<usize>().unwrap() < k, "row: {}", row);
        assert!(words[6].parse::<usize>().unwrap() < k, "row: {}", row);
        assert!(
            ["cut", "no", "skip", "illegal", "-"].contains(&words[7]),
            "row: {}",
            row
        );
        outcomes.insert(words[7].to_string());
        // a move is two squares, four characters, and five with a promotion
        assert!(words[8].len() == 4 || words[8].len() == 5, "row: {}", row);
    }
    // a run that saw only one outcome has measured nothing
    for outcome in ["cut", "no", "-"] {
        assert!(
            outcomes.contains(outcome),
            "no {outcome} rows in:\n{}",
            printed.all
        );
    }

    for line in &printed.summary {
        assert!(line.starts_with("depth "), "summary line: {}", line);
        for word in [
            " nodes ",
            " reached ",
            " mean_k ",
            " first ",
            " skipped ",
            " first_cut ",
            " rate ",
            " bins ",
        ] {
            assert!(line.contains(word), "no {} in: {}", word.trim(), line);
        }
    }
}
