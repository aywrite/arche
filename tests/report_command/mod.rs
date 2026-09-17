// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Running one of the report commands against the real binary. Cutoffs,
//! reductions and residuals all print a header naming what the run was asked
//! for, a row a sample and a summary, so the spawning and the splitting live
//! here and each file beside this one says what its own rows mean.

use std::io::Read;
use std::process::{Command, Stdio};

/// What one run printed, in the three parts a reader parses.
pub struct Printed {
    /// Everything it said, for a failure message to quote.
    pub all: String,
    pub header: String,
    pub rows: Vec<String>,
    pub summary: Vec<String>,
}

/// Runs the binary with the arguments given and splits what it printed. A
/// missing part fails here rather than in the caller.
pub fn run(arguments: &[&str]) -> Printed {
    let mut child = Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the binary cargo built should start");
    let mut all = String::new();
    child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_string(&mut all)
        .expect("the run prints text");
    let status = child.wait().expect("the child can be waited on");
    assert!(status.success(), "exit {:?}: {}", status.code(), all);

    // in a block of its own, so the borrow of `all` ends before the move
    let (rows, summary) = {
        let lines: Vec<&str> = all.lines().collect();
        let summary_at = lines
            .iter()
            .position(|line| *line == "summary")
            .unwrap_or_else(|| panic!("no summary in:\n{}", all));
        // the blank line before the summary is asserted rather than assumed:
        // a printer that stopped writing it would otherwise drop the last
        // row here in silence
        assert!(
            summary_at >= 2 && lines[summary_at - 1].is_empty(),
            "no blank line before the summary in:\n{}",
            all
        );
        let owned = |lines: &[&str]| -> Vec<String> {
            lines.iter().map(|line| (*line).to_string()).collect()
        };
        (
            owned(&lines[1..summary_at - 1]),
            owned(&lines[summary_at + 1..]),
        )
    };
    let printed = Printed {
        header: all.lines().next().unwrap_or("").to_string(),
        all,
        rows,
        summary,
    };
    // one of each, or the run measured nothing
    assert!(!printed.rows.is_empty(), "no rows in:\n{}", printed.all);
    assert!(
        !printed.summary.is_empty(),
        "empty summary in:\n{}",
        printed.all
    );
    printed
}

impl Printed {
    /// The events the header states, the denominator of every rate.
    pub fn events(&self) -> u64 {
        self.header
            .split(' ')
            .skip_while(|word| *word != "events")
            .nth(1)
            .unwrap_or_else(|| panic!("no events in header: {}", self.header))
            .parse()
            .unwrap_or_else(|e| panic!("events is not a number in {}: {}", self.header, e))
    }
}
