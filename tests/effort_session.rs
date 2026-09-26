// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the effort argument prints, run against the real binary. The
//! spawning and the splitting are in `report_command`.

mod report_command;

/// A shallow run with a switch off, at a rate that still records plenty:
/// the instrument offers an event at every full width node on each side, so
/// fifty keeps hundreds of rows at this depth, and the switch is one whose
/// rule acts at exactly these depths.
const ARGUMENTS: [&str; 6] = ["effort", "4", "every", "50", "off", "quiet_futility"];

#[test]
fn the_effort_argument_prints_a_header_rows_and_two_summaries() {
    let printed = report_command::run(&ARGUMENTS);
    assert!(
        printed
            .header
            .starts_with("effort depth 4 every 50 off quiet_futility positions "),
        "header: {}",
        printed.header
    );
    let (on, off) = printed.paired_events();
    assert!(on > 0 && off > 0, "header: {}", printed.header);
    // the switch removes nodes, so the baseline side answers more of them
    assert!(off > on, "header: {}", printed.header);
    // the null is not what ran, so the header does not say so
    assert!(!printed.header.contains(" off none "), "{}", printed.header);

    let mut outcomes = (0, 0, 0);
    for row in &printed.rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert!(words.len() > 9, "row: {}", row);
        // depth, the two visit counts, the two cut counts and the two costs
        for at in [0, 2, 3, 4, 5, 6, 7] {
            assert!(words[at].parse::<u64>().is_ok(), "field {} of {}", at, row);
        }
        // the delta is signed and is printed rather than left to the reader
        let (cost_on, cost_off) = (
            words[6].parse::<i64>().unwrap(),
            words[7].parse::<i64>().unwrap(),
        );
        assert_eq!(
            words[8].parse::<i64>().unwrap(),
            cost_on - cost_off,
            "{row}"
        );
        // a side that did not reach the node spent nothing there, which is a
        // zero and not a dash: the column has to be summable
        let (visits_on, visits_off) = (
            words[2].parse::<u64>().unwrap(),
            words[3].parse::<u64>().unwrap(),
        );
        match words[1] {
            "both" => {
                outcomes.0 += 1;
                assert!(visits_on > 0 && visits_off > 0, "row: {}", row);
            }
            "only_on" => {
                outcomes.1 += 1;
                assert_eq!((visits_off, cost_off), (0, 0), "row: {}", row);
            }
            "only_off" => {
                outcomes.2 += 1;
                assert_eq!((visits_on, cost_on), (0, 0), "row: {}", row);
            }
            other => panic!("outcome {} in: {}", other, row),
        }
        // cuts never outrun the visits they are a share of
        for at in [(4, 2), (5, 3)] {
            assert!(
                words[at.0].parse::<u64>().unwrap() <= words[at.1].parse::<u64>().unwrap(),
                "row: {}",
                row
            );
        }
    }
    assert!(outcomes.0 > 0, "no joined rows in:\n{}", printed.all);
    // a run that kept only the joined population has measured nothing this
    // instrument exists for: the two outright populations are the reading
    assert!(
        outcomes.1 > 0 || outcomes.2 > 0,
        "no key parted the two sides in:\n{}",
        printed.all
    );

    let mut depths = 0;
    let mut positions = 0;
    for line in &printed.summary {
        if line.starts_with("depth ") {
            depths += 1;
            for word in [
                " nodes on ",
                " off ",
                " delta ",
                " records ",
                " both ",
                " only_on ",
                " only_off ",
                " cost on ",
            ] {
                assert!(line.contains(word), "no {} in: {}", word.trim(), line);
            }
        } else if line.starts_with("position ") {
            positions += 1;
            for word in [
                " reached on ",
                " best on ",
                " agree ",
                " score on ",
                " nodes on ",
            ] {
                assert!(line.contains(word), "no {} in: {}", word.trim(), line);
            }
        } else {
            panic!("summary line: {}", line);
        }
    }
    assert!(depths > 0, "no depth lines in:\n{}", printed.all);
    assert_eq!(
        positions,
        printed.number_after("positions") as usize,
        "a line a position:\n{}",
        printed.all
    );
}

/// The null run is what says the instrument is reading the tree and not its
/// own buffer, so it is asserted here as well as in the module's own tests:
/// two identical configurations part company nowhere.
#[test]
fn the_null_run_parts_the_two_sides_nowhere() {
    let printed = report_command::run(&["effort", "4", "every", "50"]);
    assert!(
        printed.header.contains(" off none "),
        "header: {}",
        printed.header
    );
    let (on, off) = printed.paired_events();
    assert_eq!(on, off, "header: {}", printed.header);
    for row in &printed.rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert_eq!(words[1], "both", "row: {}", row);
        assert_eq!(words[8], "0", "row: {}", row);
    }
    for line in printed.summary.iter().filter(|l| l.starts_with("depth ")) {
        // `depth <d> nodes on <a> off <b>`, and the two counts are exact
        let words: Vec<&str> = line.split(' ').collect();
        assert_eq!(words[4], words[6], "depth line: {}", line);
        assert!(line.contains(" only_on 0 only_off 0 "), "{}", line);
    }
    for line in printed
        .summary
        .iter()
        .filter(|l| l.starts_with("position "))
    {
        assert!(line.contains(" agree yes "), "position line: {}", line);
    }
}

/// A switch the engine does not have is named rather than run as the null,
/// which would spend the minutes saying nothing. The refusal says what a
/// switch may be, since the usage line prints `<switch>` and the reader who
/// misspelled one is the reader who needs the names.
///
/// The line is read as a user reads it: the wording, the echoed misspelling
/// and the separator are literals here, and the count is fourteen. Which
/// names they are is the engine crate's own test, beside the table.
#[test]
fn a_switch_the_engine_does_not_have_is_refused() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(["effort", "4", "off", "quiet_futilty"])
        .output()
        .expect("the binary cargo built should start");
    assert_eq!(output.status.code(), Some(2));
    let printed = String::from_utf8_lossy(&output.stderr);
    let printed = printed.trim();
    let named = printed
        .strip_prefix("unrecognised effort off: quiet_futilty (a switch is one of ")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("stderr: {printed}"));
    assert_eq!(named.split(", ").count(), 15, "stderr: {printed}");
    assert!(output.stdout.is_empty());
}
