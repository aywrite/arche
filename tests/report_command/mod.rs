// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Running one of the report commands against the real binary and splitting
//! what it printed into a header, rows and a summary. Each file beside this
//! one says what its own rows mean.
//!
//! Effort states an events count a side, read by `paired_events`, so a header
//! that lost its second count fails rather than being read as the first.

use std::io::Read;
use std::process::{Command, Stdio};

pub struct Printed {
    /// Everything it said, for a failure message to quote.
    pub all: String,
    pub header: String,
    pub rows: Vec<String>,
    pub summary: Vec<String>,
}

/// A missing part fails here rather than in the caller.
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
        // asserted, since a printer that stopped writing the blank line
        // would otherwise drop the last row here in silence
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

// each test binary compiles this module afresh and calls only some of these
#[allow(dead_code)]
impl Printed {
    pub fn events(&self) -> u64 {
        self.number_after("events")
    }

    /// `events on <a> off <b>`: the candidate side's and the baseline's.
    pub fn paired_events(&self) -> (u64, u64) {
        let words: Vec<&str> = self.header.split(' ').collect();
        let at = words
            .iter()
            .position(|word| *word == "events")
            .unwrap_or_else(|| panic!("no events in header: {}", self.header));
        assert_eq!(
            words.get(at + 1).copied(),
            Some("on"),
            "header does not state an events count a side: {}",
            self.header
        );
        assert_eq!(words.get(at + 3).copied(), Some("off"), "{}", self.header);
        let read = |at: usize| -> u64 {
            words[at]
                .parse()
                .unwrap_or_else(|e| panic!("events is not a number in {}: {}", self.header, e))
        };
        (read(at + 2), read(at + 4))
    }

    pub fn number_after(&self, word: &str) -> u64 {
        self.header
            .split(' ')
            .skip_while(|had| *had != word)
            .nth(1)
            .unwrap_or_else(|| panic!("no {} in header: {}", word, self.header))
            .parse()
            .unwrap_or_else(|e| panic!("{} is not a number in {}: {}", word, self.header, e))
    }
}
