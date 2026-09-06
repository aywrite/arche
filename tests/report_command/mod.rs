// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Running one of the report commands against the real binary.
//!
//! Cutoffs, reductions and residuals are arguments rather than uci commands,
//! so nothing in the protocol suite reaches them: what they print is only
//! ever printed by the program. All three print the same three parts, a
//! header naming what the run was asked for, a row a sample and a summary,
//! so the spawning and the splitting live here and each file beside this one
//! is left saying what its own rows mean.

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

/// Runs the binary with the arguments given and splits what it printed.
///
/// A missing part fails here rather than in the caller, since every
/// assertion a caller makes stands on the split having worked, and the
/// shape the split reads is asserted as it goes.
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

    // in a block of its own, so the borrow of `all` these take is over
    // before `all` is moved below
    let (rows, summary) = {
        let lines: Vec<&str> = all.lines().collect();
        let summary_at = lines
            .iter()
            .position(|line| *line == "summary")
            .unwrap_or_else(|| panic!("no summary in:\n{}", all));
        // the rows sit between the header and the blank line before the
        // summary, which is stated rather than assumed: a printer that
        // stopped writing it would otherwise drop the last row here in
        // silence
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
    /// The events the header states, which is the denominator: the rows say
    /// nothing about a rate without it.
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
