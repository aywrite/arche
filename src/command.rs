// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a command line argument is called and what words it takes.
//!
//! One declaration per argument, because the same list has three jobs and they
//! have to agree. It says which words may stand where the depth would, so a
//! keyword there reads as the depth being left out rather than mistyped. It
//! says which words are known at all, so one that is not can be refused rather
//! than ignored. And it spells the argument for the usage, so the help cannot
//! describe a line the parser does not take.
//!
//! The third of those is why the spelling is built here rather than written
//! out. `--help` used to be a string kept in step by hand, and the test that
//! guarded it held it against a list written out by hand as well, so the two
//! could drift together and pass.
//!
//! The refusal is for arguments only. A uci interface is entitled to send an
//! option meant for another engine and the protocol says to carry on; a person
//! typing `hsah 16` at a shell wanted `hash 16` and would rather be told. The
//! bench is both, and takes the strict reading, because it is a measurement
//! either way and one measured at settings nobody asked for is worse than one
//! not taken.

use crate::params::Params;

/// A word that takes a value after it, and how the usage spells that value.
pub struct Keyword {
    pub word: &'static str,
    pub value: &'static str,
}

/// One argument the binary takes: `<name> [depth] [<keyword> <value>]... [<flag>]...`.
pub struct Command {
    pub name: &'static str,
    /// The words that take a value after them, in the order the usage spells
    /// them.
    pub keywords: &'static [Keyword],
    /// The words that stand alone.
    pub flags: &'static [&'static str],
    /// What it does, a line at a time, for the usage.
    pub summary: &'static [&'static str],
}

impl Command {
    /// Whether the argument knows this word. A word it knows may stand where
    /// the depth would, which is how a line that names no depth is read.
    pub fn takes(&self, word: &str) -> bool {
        self.keywords.iter().any(|k| k.word == word) || self.flags.contains(&word)
    }

    fn is_keyword(&self, word: &str) -> bool {
        self.keywords.iter().any(|k| k.word == word)
    }

    /// The first word of the line that this argument does not know, if there
    /// is one.
    ///
    /// Walked rather than compared as a set, because a keyword's value is not
    /// itself a word to recognise: `hash 16` claims the `16` after it, and a
    /// set would have to decide whether `16` was known on its own.
    ///
    /// The first word is whatever invoked us and the second may be the depth,
    /// which is a number this cannot judge — `depth: abc` is the reading a
    /// caller's own parse gives it, and a better one, which is why this runs
    /// after that parse rather than before.
    pub fn unclaimed<'a>(&self, params: &Params<'a>) -> Option<&'a str> {
        let words = params.words();
        // from one, because the first word is whatever invoked us
        let mut at = 1;
        while at < words.len() {
            let word = words[at];
            if self.is_keyword(word) {
                // the keyword and the value it takes. A keyword standing last
                // claims a word that is not there, which leaves the setting
                // absent and at its default, the reading it already had
                at += 2;
            } else if self.flags.contains(&word) || at == 1 {
                // a flag stands alone, and the second word is the depth,
                // which the caller's own parse judges
                at += 1;
            } else {
                return Some(word);
            }
        }
        None
    }

    /// `Ok` unless the line names a word this argument does not know. Shaped
    /// like the other refusals, `<what>: <word>`, because the caller prints
    /// them all the same way.
    pub fn claim(&self, params: &Params) -> Result<(), String> {
        match self.unclaimed(params) {
            None => Ok(()),
            Some(word) => Err(format!("word: {word}")),
        }
    }

    /// How the line is spelled, for the usage.
    pub fn spelling(&self) -> String {
        let mut out = format!("{} [depth]", self.name);
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

    fn unclaimed(line: &str) -> Option<String> {
        TAKES.unclaimed(&Params::of(line)).map(str::to_string)
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
            assert_eq!(unclaimed(line), None, "{line}");
        }
    }

    #[test]
    fn a_word_it_does_not_know_is_named() {
        assert_eq!(unclaimed("probe 4 evrey 50"), Some("evrey".to_string()));
        assert_eq!(
            unclaimed("probe 4 every 50 spare"),
            Some("spare".to_string())
        );
        assert_eq!(unclaimed("probe 4 audit extra"), Some("extra".to_string()));
    }

    /// The value a keyword takes is claimed by the keyword, so a number that
    /// would mean nothing on its own does not read as unknown.
    #[test]
    fn a_keywords_value_is_not_judged_on_its_own() {
        assert_eq!(unclaimed("probe every 50"), None);
        assert_eq!(unclaimed("probe cap 20"), None);
    }

    /// A keyword last on the line claims a word that is not there. The setting
    /// stays absent and takes its default, which is the reading it had before
    /// anything was refused.
    #[test]
    fn a_keyword_with_no_value_left_is_not_a_refusal() {
        assert_eq!(unclaimed("probe 4 every"), None);
    }

    /// The second word is the depth, which this cannot judge: the caller's own
    /// parse says `depth: abc`, which is the better message, and it runs
    /// first.
    #[test]
    fn the_depths_place_is_left_to_the_caller() {
        assert_eq!(unclaimed("probe abc"), None);
    }

    #[test]
    fn the_spelling_names_every_word_the_argument_takes() {
        assert_eq!(
            TAKES.spelling(),
            "probe [depth] [every <n>] [cap <n>] [audit]"
        );
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

    #[test]
    fn a_refusal_is_shaped_like_the_others() {
        assert_eq!(
            TAKES.claim(&Params::of("probe 4 evrey 50")).unwrap_err(),
            "word: evrey"
        );
        assert!(TAKES.claim(&Params::of("probe 4 every 50")).is_ok());
    }
}
