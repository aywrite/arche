// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a command line argument is called and what words it takes.
//!
//! One declaration per argument, which both parses the line and spells the
//! usage, so `--help` cannot drift from what is accepted.
//!
//! Unknown words are refused for arguments only. A uci interface may send an
//! option meant for another engine and the protocol says to carry on; a
//! person typing `hsah 16` at a shell would rather be told. The bench is both
//! and takes the strict reading, since a measurement at settings nobody asked
//! for is worse than none. A keyword typed with nothing after it is refused
//! by the setting's own reader, which can name the setting.

use crate::params::{Param, Params};

/// A word that takes a value after it, and how the usage spells that value.
pub struct Keyword {
    pub word: &'static str,
    pub value: &'static str,
}

/// One argument the binary takes: `<name> [depth] [<keyword> <value>]... [<flag>]...`.
pub struct Command {
    pub name: &'static str,
    /// Whether a bare number after the name is a depth. When not, a word
    /// standing there is refused.
    pub depth: bool,
    /// The words that take a value, in usage order.
    pub keywords: &'static [Keyword],
    /// The words that stand alone.
    pub flags: &'static [&'static str],
    /// What it does, a line at a time, for the usage.
    pub summary: &'static [&'static str],
}

impl Command {
    /// The depth the line names, or `default` when it names none. A word in
    /// the depth's place that is not one of the command's own is refused
    /// under the depth's name rather than run at the default.
    pub fn depth(&self, params: &Params, default: u8) -> Result<u8, String> {
        match params.parse::<u8>(self.name) {
            // the command's word standing last asks for the default depth,
            // unlike a setting left without its value
            Param::Absent | Param::Bare => Ok(default),
            Param::Read(depth) => Ok(depth),
            Param::Unreadable(word) if self.takes(word) => Ok(default),
            Param::Unreadable(word) => Err(format!("depth: {word}")),
        }
    }

    /// Whether the argument knows this word, which may then stand where the
    /// depth would.
    pub fn takes(&self, word: &str) -> bool {
        self.keywords.iter().any(|k| k.word == word) || self.flags.contains(&word)
    }

    fn is_keyword(&self, word: &str) -> bool {
        self.keywords.iter().any(|k| k.word == word)
    }

    /// `Ok` unless the line names a word this argument does not know, or one
    /// of its keywords twice. Walked rather than compared as a set, because a
    /// keyword claims the word after it: in `epd cap` the `cap` is a file.
    ///
    /// The second word may be the depth, which the caller's own parse refuses
    /// as `depth: abc`, so this runs after that parse.
    pub fn claim(&self, params: &Params) -> Result<(), String> {
        let words = params.words();
        let mut seen: Vec<&str> = Vec::new();
        let mut at = 1;
        while at < words.len() {
            let word = words[at];
            if self.is_keyword(word) {
                // the setting's reader takes the first, so a second would be
                // read by nobody
                if seen.contains(&word) {
                    return Err(format!("{word}: given twice"));
                }
                seen.push(word);
                // one standing last is refused by the setting's own reader
                at += 2;
            } else if self.flags.contains(&word) || (self.depth && at == 1) {
                at += 1;
            } else {
                return Err(format!("word: {word}"));
            }
        }
        Ok(())
    }

    /// How the line is spelled, for the usage.
    pub fn spelling(&self) -> String {
        let mut out = self.name.to_string();
        if self.depth {
            out.push_str(" [depth]");
        }
        for keyword in self.keywords {
            out.push_str(&format!(" [{} {}]", keyword.word, keyword.value));
        }
        for flag in self.flags {
            out.push_str(&format!(" [{}]", flag));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAKES: Command = Command {
        name: "probe",
        depth: true,
        keywords: &[
            Keyword {
                word: "every",
                value: "<n>",
            },
            Keyword {
                word: "cap",
                value: "<n>",
            },
        ],
        flags: &["audit"],
        summary: &["a command that exists to be parsed"],
    };

    /// The same, with no depth.
    const DEPTHLESS: Command = Command {
        name: "read",
        depth: false,
        keywords: &[Keyword {
            word: "epd",
            value: "<file>",
        }],
        flags: &[],
        summary: &["a command that takes no depth"],
    };

    fn refused(line: &str) -> Option<String> {
        TAKES.claim(&Params::of(line)).err()
    }

    #[test]
    fn a_line_of_words_it_knows_has_nothing_unclaimed() {
        for line in [
            "probe",
            "probe 4",
            "probe 4 every 50",
            "probe 4 every 50 cap 20",
            "probe every 50",
            "probe 4 audit",
            "probe audit every 50",
        ] {
            assert_eq!(refused(line), None, "{line}");
        }
    }

    #[test]
    fn a_word_it_does_not_know_is_named() {
        assert_eq!(refused("probe 4 evrey 50"), Some("word: evrey".to_string()));
        assert_eq!(
            refused("probe 4 every 50 spare"),
            Some("word: spare".to_string())
        );
        assert_eq!(
            refused("probe 4 audit extra"),
            Some("word: extra".to_string())
        );
    }

    #[test]
    fn a_keywords_value_is_not_judged_on_its_own() {
        assert_eq!(refused("probe every 50"), None);
        assert_eq!(refused("probe cap 20"), None);
        // a value that is spelled like a keyword is still only a value
        assert_eq!(refused("probe cap 20 every cap"), None);
    }

    /// The second would be read by nobody, and standing last it would be a
    /// keyword given no value that no reader looks at.
    #[test]
    fn a_keyword_given_twice_is_refused_under_its_own_name() {
        for line in ["probe 4 every 50 every 60", "probe 4 every 50 every"] {
            assert_eq!(
                refused(line),
                Some("every: given twice".to_string()),
                "{line}"
            );
        }
    }

    /// Refused by the setting's reader, which can name the setting.
    #[test]
    fn a_keyword_with_no_value_left_is_left_to_its_own_reader() {
        assert_eq!(refused("probe 4 every"), None);
    }

    #[test]
    fn the_command_word_standing_last_asks_for_the_default_depth() {
        assert_eq!(TAKES.depth(&Params::of("probe"), 9), Ok(9));
        assert_eq!(TAKES.depth(&Params::of("probe 4"), 9), Ok(4));
        assert_eq!(TAKES.depth(&Params::of("probe every 50"), 9), Ok(9));
    }

    /// The caller's own parse judges the depth and says `depth: abc`.
    #[test]
    fn the_depths_place_is_left_to_the_caller() {
        assert_eq!(refused("probe abc"), None);
    }

    #[test]
    fn the_spelling_names_every_word_the_argument_takes() {
        assert_eq!(
            TAKES.spelling(),
            "probe [depth] [every <n>] [cap <n>] [audit]"
        );
    }

    #[test]
    fn an_argument_that_runs_no_search_spells_no_depth() {
        assert_eq!(DEPTHLESS.spelling(), "read [epd <file>]");
    }

    #[test]
    fn a_depthless_argument_refuses_a_word_where_the_depth_would_be() {
        assert_eq!(
            DEPTHLESS.claim(&Params::of("read 4")).unwrap_err(),
            "word: 4"
        );
        assert!(DEPTHLESS.claim(&Params::of("read")).is_ok());
        assert!(DEPTHLESS.claim(&Params::of("read epd suite.epd")).is_ok());
    }

    #[test]
    fn what_it_takes_is_what_may_stand_where_the_depth_would() {
        for word in ["every", "cap", "audit"] {
            assert!(TAKES.takes(word), "{word}");
        }
        for word in ["4", "hash", "evrey"] {
            assert!(!TAKES.takes(word), "{word}");
        }
    }
}
