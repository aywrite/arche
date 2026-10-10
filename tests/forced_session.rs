// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the forced argument prints, run against the real binary. The
//! spawning and the splitting are in `report_command`.

mod report_command;

/// Shallow, and at a rate that keeps a few dozen decisions from the bench's
/// suite, each of which costs a search of its root.
const ARGUMENTS: [&str; 4] = ["forced", "5", "every", "3000"];

#[test]
fn the_forced_argument_prints_a_header_rows_and_a_line_a_kind_and_a_root() {
    let printed = report_command::run(&ARGUMENTS);
    assert!(
        printed.header.starts_with(
            "forced depth 5 every 3000 cap 10000 epd bench \
             kinds reverse_futility,null_move,skip,trusted_scout from 0 positions "
        ),
        "header: {}",
        printed.header
    );
    assert!(printed.events() > 0, "header: {}", printed.header);
    for row in &printed.rows {
        let words: Vec<&str> = row.split(' ').collect();
        // twenty columns before a fen of six fields
        assert_eq!(words.len(), 26, "row: {row}");
        assert!(
            ["reverse_futility", "null_move", "skip", "trusted_scout"].contains(&words[0]),
            "row: {row}"
        );
        // depth, visits, root, the two scores and the two node counts
        for at in [1, 12, 13, 16, 17, 18, 19] {
            assert!(words[at].parse::<i64>().is_ok(), "field {at} of {row}");
        }
        // a kept decision the forced search never met is a failed row
        assert!(words[12] != "0", "row: {row}");
    }
    let kinds = printed
        .summary
        .iter()
        .filter(|line| line.starts_with("kind "))
        .count();
    let roots = printed
        .summary
        .iter()
        .filter(|line| line.starts_with("root "))
        .count();
    assert_eq!(kinds, 4, "{}", printed.all);
    assert!(roots > 0, "{}", printed.all);
}
