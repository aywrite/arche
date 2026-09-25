// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a command line argument is called and what words it takes.
//!
//! One declaration per argument, because the same list says which words may
//! stand where the depth would, which words are known at all, and how the
//! usage spells the line. `--help` used to be a string kept in step by hand,
//! guarded by a test written out by hand as well, so the two could drift
//! together and pass.
//!
//! The refusal is for arguments only. A uci interface is entitled to send an
//! option meant for another engine and the protocol says to carry on; a
//! person typing `hsah 16` at a shell would rather be told. The bench is both
//! and takes the strict reading: a measurement at settings nobody asked for
//! is worse than one not taken. A keyword typed with nothing after it is
//! refused for the same reason. The setting's own reader refuses it, not this
//! one, because that is what can name the setting rather than only the word.

use crate::params::{Param, Params};

/// A word that takes a value after it, and how the usage spells that value.
pub struct Keyword {
    pub word: &'static str,
    pub value: &'static str,
}

/// One argument the binary takes: `<name> [depth] [<keyword> <value>]... [<flag>]...`.
pub struct Command {
    pub name: &'static str,
    /// Whether a bare number after the name is a depth. When not, the
    /// spelling leaves `[depth]` out and a word standing there is refused.
    pub depth: bool,
    /// The words that take a value after them, in the order the usage spells
    /// them.
    pub keywords: &'static [Keyword],
    /// The words that stand alone.
    pub flags: &'static [&'static str],
    /// What it does, a line at a time, for the usage.
    pub summary: &'static [&'static str],
}

impl Command {
    /// The depth the line names, or `default` when it names none. The word
    /// after the command's own is the depth unless it is one of the
    /// command's keywords or flags, and a word that is neither is refused
    /// under the depth's name rather than run at the default.
    pub fn depth(&self, params: &Params, default: u8) -> Result<u8, String> {
        match params.parse::<u8>(self.name) {
            // the command's own word standing last is the line asking for no
            // depth, which is how the default is asked for, rather than a
            // setting left half typed
            Param::Absent | Param::Bare => Ok(default),
            Param::Read(depth) => Ok(depth),
            Param::Unreadable(word) if self.takes(word) => Ok(default),
            Param::Unreadable(word) => Err(format!("depth: {word}")),
        }
    }

    /// Whether the argument knows this word. A word it knows may stand where
    /// the depth would.
    pub fn takes(&self, word: &str) -> bool {
        self.keywords.iter().any(|k| k.word == word) || self.flags.contains(&word)
    }

    fn is_keyword(&self, word: &str) -> bool {
        self.keywords.iter().any(|k| k.word == word)
    }

    /// `Ok` unless the line names a word this argument does not know, or
    /// names one of its keywords twice. Walked rather than compared as a set,
    /// because a keyword claims the value after it: `hash 16` claims the
    /// `16`, and a `cap` standing where a file name goes is that file name.
    ///
    /// The first word is whatever invoked us. The second may be the depth,
    /// which the caller's own parse judges and refuses as `depth: abc`, so
    /// this runs after that parse. The refusals are shaped `<what>: <word>`
    /// and `<setting>: <what>` like the others, since the caller prints them
    /// all the same way.
    pub fn claim(&self, params: &Params) -> Result<(), String> {
        let words = params.words();
        let mut seen: Vec<&str> = Vec::new();
        let mut at = 1;
        while at < words.len() {
            let word = words[at];
            if self.is_keyword(word) {
                // one keyword cannot mean two things, and the second copy is
                // read by nobody: the setting's own reader takes the first,
                // so a repeat standing last would be a word given no value
                // that nothing refused
                if seen.contains(&word) {
                    return Err(format!("{word}: given twice"));
                }
                seen.push(word);
                // a keyword standing last claims a word that is not there,
                // which the setting's own reader refuses under its name
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

    /// The same, with no search to run to a depth.
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

    /// What the argument refuses the line for, if anything.
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

    /// The setting's own reader takes the first of them, so the second is
    /// read by nobody, and a second standing last is a keyword given no value
    /// that no reader is looking at.
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

    /// A keyword last on the line claims a word that is not there. It is
    /// refused, but by the reader of the setting it names, which can say
    /// which setting was left without a value where this would only say the
    /// word was unknown.
    #[test]
    fn a_keyword_with_no_value_left_is_left_to_its_own_reader() {
        assert_eq!(refused("probe 4 every"), None);
    }

    /// The command's own word is last on every line that names no depth,
    /// which is the usual spelling rather than a setting given no value.
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

    /// A number where the depth would be is a word the argument does not
    /// know, and gets the refusal any other unknown word gets.
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

    #[test]
    fn a_refusal_is_shaped_like_the_others() {
        assert_eq!(
            TAKES.claim(&Params::of("probe 4 evrey 50")).unwrap_err(),
            "word: evrey"
        );
        assert!(TAKES.claim(&Params::of("probe 4 every 50")).is_ok());
    }
}
