// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the policy argument prints, run against the real binary.

use std::process::Command;

fn printed(arguments: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(arguments)
        .output()
        .expect("the binary cargo built should start");
    let text = String::from_utf8(output.stdout).expect("the run prints text");
    assert!(
        output.status.success(),
        "exit {:?}: {}",
        output.status,
        text
    );
    text
}

/// A line a root, under no rule and under one read from a file, the rule's
/// lines echoed under the header so the run can be told apart and rerun.
#[test]
fn the_policy_argument_prints_a_header_the_rule_and_a_line_a_root() {
    let none = printed(&["policy", "nodes", "20000"]);
    let mut lines = none.lines();
    let header = lines.next().expect("a header");
    assert!(
        header.starts_with("policy rule none nodes 20000 epd bench positions "),
        "{none}"
    );
    let roots: Vec<&str> = lines.collect();
    assert!(!roots.is_empty(), "{none}");
    for root in &roots {
        let words: Vec<&str> = root.split(' ').collect();
        // sixteen words, then the root's id, which may hold spaces
        assert!(words.len() > 16, "{root}");
        let labels: Vec<&str> = words[..16].iter().step_by(2).copied().collect();
        assert_eq!(
            labels,
            [
                "root", "best", "score", "depth", "swapped", "nodes", "scouts", "nulls"
            ],
            "{root}"
        );
        // no rule inverts nothing
        assert_eq!((words[13], words[15]), ("0", "0"), "{root}");
    }

    let path = std::env::temp_dir().join(format!("arche-policy-{}.rule", std::process::id()));
    std::fs::write(&path, "trusted_scout -1 depth=3: # every scout\n").unwrap();
    let ruled = printed(&["policy", "rule", path.to_str().unwrap(), "nodes", "20000"]);
    let _ = std::fs::remove_file(&path);
    let mut lines = ruled.lines();
    lines.next();
    assert_eq!(
        lines.next(),
        Some("rule trusted_scout -1 depth=3:"),
        "{ruled}"
    );
    let scouts: u64 = lines
        .map(|line| line.split(' ').nth(13).unwrap().parse::<u64>().unwrap())
        .sum();
    assert!(scouts > 0, "{ruled}");
}
