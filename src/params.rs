// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Reading the parameters off a UCI command.
//!
//! A command is a line of whitespace separated words, and a parameter is a
//! keyword followed by its value: `go wtime 300000 winc 2000 movestogo 40`.
//!
//! Whole words throughout: a keyword only counts where it stands as a word of
//! its own, so `movetime 500` cannot be read as a value for `time`.
//!
//! A keyword with nothing after it is a reading of its own rather than the
//! keyword being absent. Which of the two a caller wants differs by side. The
//! binary's arguments refuse it, because a measurement run at a default
//! nobody asked for takes as long as the one that was asked for and answers a
//! different question. The protocol carries on either way: a `setoption` with
//! nothing to apply says so, and a clock or a move time with nothing after it
//! reads as spent rather than as never sent.

use std::num::IntErrorKind;
use std::str::FromStr;

/// The words of one command line.
// pub so a caller outside the crate can build one and hand it to a settings
// reader; the readers stay pub(crate) until something outside needs one
pub struct Params<'a> {
    words: Vec<&'a str>,
}

/// What reading one parameter found. Says what happened rather than deciding,
/// because the right reading differs by parameter: an unreadable clock is
/// safest read as spent, while an unreadable depth is better ignored than
/// obeyed as zero.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Param<'a, T> {
    /// The keyword is not among the words.
    Absent,
    /// The keyword is the last of the words, so the value it takes is not
    /// there to read. Apart from absent because the two were meant
    /// differently: a setting left off the line asks for its default, and one
    /// typed with nothing after it asks for a value that is missing.
    Bare,
    /// The keyword is there and what follows it is not a value. Carries the
    /// word, for a caller that wants to say which it was.
    Unreadable(&'a str),
    Read(T),
}

/// What stands where the word would in a refusal naming a keyword that was
/// typed with nothing after it. The refusals read `<setting>: <word>`, and
/// here there is no word to name.
pub(crate) const NO_VALUE: &str = "no value";

impl<'a> Params<'a> {
    /// The words of the line, in order, for the one caller that cares where
    /// a word stands (`Command::claim`).
    pub(crate) fn words(&self) -> &[&'a str] {
        &self.words
    }

    pub fn of(line: &'a str) -> Self {
        Self {
            words: line.split_whitespace().collect(),
        }
    }

    /// Whether `keyword` stands among the words on its own.
    pub(crate) fn flag(&self, keyword: &str) -> bool {
        self.words.contains(&keyword)
    }

    /// The word following `keyword`, or which of the two ways there is none.
    /// `Absent` or `Bare`, never `Unreadable`: any word at all is a word, so
    /// a setting whose value is one cannot fail to read it.
    pub(crate) fn value(&self, keyword: &str) -> Param<'a, &'a str> {
        match self.following(keyword) {
            Ok(word) => Param::Read(word),
            Err(missing) => missing,
        }
    }

    /// The word following `keyword`, or the reading that stands for there
    /// being none, for the readers that go on to judge the word themselves.
    fn following<T>(&self, keyword: &str) -> Result<&'a str, Param<'a, T>> {
        let Some(at) = self.words.iter().position(|word| *word == keyword) else {
            return Err(Param::Absent);
        };
        self.words.get(at + 1).copied().ok_or(Param::Bare)
    }

    /// The words between `keyword` and `until`, joined by a single space, or
    /// everything after `keyword` when `until` is not there. None when the
    /// keyword is absent or nothing stands between the two. An option name
    /// may run to several words (`Clear Hash`).
    pub(crate) fn phrase(&self, keyword: &str, until: &str) -> Option<String> {
        let at = self.words.iter().position(|word| *word == keyword)?;
        let rest = &self.words[at + 1..];
        let end = rest
            .iter()
            .position(|word| *word == until)
            .unwrap_or(rest.len());
        let words = &rest[..end];
        if words.is_empty() {
            return None;
        }
        Some(words.join(" "))
    }

    /// A count of milliseconds or nodes. Below zero is what the match tools
    /// send once their time margin has been eaten into, and reads as a spent
    /// clock; too large to hold reads as everything there is. Only a word
    /// that is no kind of number is `Unreadable`.
    pub(crate) fn count(&self, keyword: &str) -> Param<'a, u64> {
        let word = match self.following(keyword) {
            Ok(word) => word,
            Err(missing) => return missing,
        };
        match word.parse::<u64>() {
            Ok(count) => Param::Read(count),
            Err(error) => match error.kind() {
                IntErrorKind::PosOverflow => Param::Read(u64::MAX),
                // u64 rejects every negative, so this is the sign test
                IntErrorKind::InvalidDigit if is_negative(word) => Param::Read(0),
                _ => Param::Unreadable(word),
            },
        }
    }

    /// A parameter parsed strictly, with none of the leniency `count` applies:
    /// `bench 300` is a typo for a depth, not a request for one.
    pub(crate) fn parse<T: FromStr>(&self, keyword: &str) -> Param<'a, T> {
        let word = match self.following(keyword) {
            Ok(word) => word,
            Err(missing) => return missing,
        };
        match word.parse() {
            Ok(value) => Param::Read(value),
            Err(_) => Param::Unreadable(word),
        }
    }
}

/// Whether the word is a negative number. A bare `-` is not one.
fn is_negative(word: &str) -> bool {
    match word.strip_prefix('-') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

impl<'a, T> Param<'a, T> {
    /// The value read, with every way of having none discarded alike.
    pub(crate) fn read(self) -> Option<T> {
        match self {
            Param::Read(value) => Some(value),
            Param::Absent | Param::Bare | Param::Unreadable(_) => None,
        }
    }

    /// The value read, with a keyword carrying no value standing in as
    /// `instead`. Only a keyword that is not on the line at all is absent: a
    /// word typed with nothing after it was still typed, and every caller
    /// here reads a clock or a move time, where one that was sent and cannot
    /// be had is safer read as spent than as no time at all, since a `go`
    /// with no time searches without a limit.
    pub(crate) fn read_or(self, instead: T) -> Option<T> {
        match self {
            Param::Read(value) => Some(value),
            Param::Bare | Param::Unreadable(_) => Some(instead),
            Param::Absent => None,
        }
    }

    /// The value read, with both ways of naming a setting and giving it no
    /// value refused under the setting's own name. For the binary's
    /// arguments, where running the default in place of a value nobody typed
    /// takes minutes and explains nothing. An absent keyword is no refusal,
    /// since leaving one off the line is how its default is asked for.
    pub(crate) fn or_refuse(self, keyword: &str) -> Result<Option<T>, String> {
        match self {
            Param::Read(value) => Ok(Some(value)),
            Param::Absent => Ok(None),
            Param::Bare => Err(format!("{keyword}: {NO_VALUE}")),
            Param::Unreadable(word) => Err(format!("{keyword}: {word}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Param, Params};

    #[test]
    fn a_value_is_the_word_after_its_keyword() {
        let params = Params::of("go wtime 300000 winc 2000");
        assert_eq!(params.value("wtime"), Param::Read("300000"));
        assert_eq!(params.value("winc"), Param::Read("2000"));
        assert_eq!(params.value("btime"), Param::Absent);
    }

    #[test]
    fn a_keyword_only_counts_as_a_whole_word() {
        // movetime still reads its own value while it contains `time`
        let params = Params::of("go movetime 500");
        assert_eq!(params.count("time"), Param::Absent);
        assert_eq!(params.count("movetime"), Param::Read(500));
        assert!(!params.flag("move"));
        assert!(params.flag("movetime"));
    }

    #[test]
    fn a_phrase_is_every_word_up_to_the_one_that_ends_it() {
        let params = Params::of("setoption name Clear Hash value 1");
        assert_eq!(
            params.phrase("name", "value").as_deref(),
            Some("Clear Hash")
        );
        assert_eq!(
            Params::of("setoption name Hash value 1")
                .phrase("name", "value")
                .as_deref(),
            Some("Hash")
        );
        // a button carries no value, so the phrase runs to the end
        assert_eq!(
            Params::of("setoption name Clear Hash")
                .phrase("name", "value")
                .as_deref(),
            Some("Clear Hash")
        );
    }

    #[test]
    fn a_phrase_with_no_words_in_it_is_absent() {
        assert_eq!(Params::of("setoption").phrase("name", "value"), None);
        assert_eq!(Params::of("setoption name").phrase("name", "value"), None);
        assert_eq!(
            Params::of("setoption name value 1").phrase("name", "value"),
            None
        );
    }

    /// Apart from the keyword not being there at all, so that a setting given
    /// no value is refused rather than run at a default nobody asked for.
    #[test]
    fn a_keyword_with_nothing_after_it_is_bare_rather_than_absent() {
        let params = Params::of("go depth");
        assert!(params.flag("depth"));
        assert_eq!(params.count("depth"), Param::Bare);
        assert_eq!(params.value("depth"), Param::Bare);
        assert_eq!(params.parse::<u8>("depth"), Param::Bare);
        // the keyword before the last one still reads its own value
        let two = Params::of("go depth 5 nodes");
        assert_eq!(two.count("depth"), Param::Read(5));
        assert_eq!(two.count("nodes"), Param::Bare);
    }

    #[test]
    fn a_keyword_that_is_not_there_at_all_is_absent() {
        let params = Params::of("go depth");
        assert_eq!(params.count("nodes"), Param::Absent);
        assert_eq!(params.value("nodes"), Param::Absent);
        assert_eq!(params.parse::<u8>("nodes"), Param::Absent);
    }

    #[test]
    fn a_word_that_is_no_number_at_all_is_unreadable() {
        let params = Params::of("go wtime abc btime - winc 1e3 binc +");
        assert_eq!(params.count("wtime"), Param::Unreadable("abc"));
        assert_eq!(params.count("btime"), Param::Unreadable("-"));
        assert_eq!(params.count("winc"), Param::Unreadable("1e3"));
        assert_eq!(params.count("binc"), Param::Unreadable("+"));
    }

    #[test]
    fn a_leading_plus_is_a_count() {
        // rust's integer parser takes one, and nothing is gained by refusing
        let params = Params::of("go wtime +300000");
        assert_eq!(params.count("wtime"), Param::Read(300_000));
    }

    #[test]
    fn parse_refuses_what_will_not_fit_rather_than_rounding_it() {
        let params = Params::of("bench 300");
        assert_eq!(params.parse::<u8>("bench"), Param::Unreadable("300"));
        assert_eq!(Params::of("bench 7").parse::<u8>("bench"), Param::Read(7u8));
        assert_eq!(Params::of("bench").parse::<u8>("bench"), Param::Bare);
    }

    #[test]
    fn read_discards_every_kind_of_missing_value() {
        assert_eq!(Params::of("go").count("wtime").read(), None);
        assert_eq!(Params::of("go wtime").count("wtime").read(), None);
        assert_eq!(Params::of("go wtime x").count("wtime").read(), None);
        assert_eq!(Params::of("go wtime 5").count("wtime").read(), Some(5));
    }

    /// A keyword that was typed stands in, whether the word after it was no
    /// value or was not there: either way a clock was sent.
    #[test]
    fn read_or_stands_in_for_a_keyword_that_was_sent_but_not_for_an_absent_one() {
        assert_eq!(Params::of("go").count("wtime").read_or(0), None);
        assert_eq!(Params::of("go wtime").count("wtime").read_or(0), Some(0));
        assert_eq!(Params::of("go wtime x").count("wtime").read_or(0), Some(0));
        assert_eq!(Params::of("go wtime 5").count("wtime").read_or(0), Some(5));
    }

    #[test]
    fn or_refuse_names_the_setting_and_what_stood_where_its_value_would() {
        let refuse = |line: &str| Params::of(line).parse::<u32>("cap").or_refuse("cap");
        assert_eq!(refuse("cutoffs"), Ok(None));
        assert_eq!(refuse("cutoffs cap 500"), Ok(Some(500)));
        assert_eq!(refuse("cutoffs cap lots"), Err("cap: lots".to_string()));
        assert_eq!(refuse("cutoffs cap"), Err("cap: no value".to_string()));
    }

    #[test]
    fn an_empty_line_has_no_parameters_and_does_not_panic() {
        let params = Params::of("");
        assert_eq!(params.value("wtime"), Param::Absent);
        assert_eq!(params.count("wtime"), Param::Absent);
        assert!(!params.flag("infinite"));
    }

    #[test]
    fn a_keyword_used_twice_reads_the_first() {
        // the protocol does not send one twice, but the choice is pinned
        let params = Params::of("go wtime 100 wtime 200");
        assert_eq!(params.count("wtime"), Param::Read(100));
    }

    #[test]
    fn a_value_may_be_a_keyword_of_its_own() {
        // the scan must not lose the second one
        let params = Params::of("go depth infinite");
        assert_eq!(params.count("depth"), Param::Unreadable("infinite"));
        assert!(params.flag("infinite"));
    }
}

#[cfg(test)]
mod properties {
    use super::{Param, Params};
    use proptest::prelude::*;

    /// A keyword in the shape the protocol's are: letters, never a number, so
    /// it cannot be mistaken for a value.
    fn keyword() -> impl Strategy<Value = String> {
        "[a-z]{1,10}".prop_map(String::from)
    }

    /// One word: anything that survives a whitespace split as a single one.
    fn word() -> impl Strategy<Value = String> {
        r"[^\s]{1,16}".prop_map(String::from)
    }

    proptest! {
        /// Nothing the parser can be given may panic.
        #[test]
        fn nothing_read_out_of_anything_panics(line in any::<String>(), keyword in any::<String>()) {
            let params = Params::of(&line);
            let _ = params.flag(&keyword);
            let _ = params.value(&keyword);
            let _ = params.count(&keyword);
            let _ = params.parse::<u8>(&keyword);
            let _ = params.parse::<u64>(&keyword);
            let _ = params.parse::<usize>(&keyword);
        }

        /// Any count the protocol could send reads back as itself.
        #[test]
        fn a_count_reads_back_as_itself(keyword in keyword(), value in any::<u64>()) {
            let line = format!("{} {}", keyword, value);
            prop_assert_eq!(Params::of(&line).count(&keyword), Param::Read(value));
        }

        /// However far below zero, a clock reads as spent rather than as
        /// unreadable.
        #[test]
        fn any_negative_count_is_a_spent_one(keyword in keyword(), digits in "[0-9]{1,40}") {
            let line = format!("{} -{}", keyword, digits);
            prop_assert_eq!(Params::of(&line).count(&keyword), Param::Read(0));
        }

        /// However far above what a word holds, a count reads as the largest
        /// one rather than as unreadable.
        #[test]
        fn any_count_too_large_is_the_largest_one(keyword in keyword(), digits in "[1-9][0-9]{20,40}") {
            let line = format!("{} {}", keyword, digits);
            prop_assert_eq!(Params::of(&line).count(&keyword), Param::Read(u64::MAX));
        }

        /// Every keyword of a line reads its own value and none of its
        /// neighbours'.
        #[test]
        fn every_keyword_reads_its_own_value(
            pairs in prop::collection::vec((keyword(), any::<u32>()), 1..8),
        ) {
            let line = pairs
                .iter()
                .map(|(keyword, value)| format!("{} {}", keyword, value))
                .collect::<Vec<_>>()
                .join(" ");
            let params = Params::of(&line);
            // a keyword sent twice reads the first, so ask each once
            let mut asked: Vec<&str> = Vec::new();
            for (keyword, value) in &pairs {
                if asked.contains(&keyword.as_str()) {
                    continue;
                }
                asked.push(keyword);
                prop_assert_eq!(params.count(keyword), Param::Read(u64::from(*value)));
            }
        }

        /// Spacing does not change which word is the value of which.
        #[test]
        fn spacing_does_not_change_what_is_read(
            keyword in keyword(),
            value in any::<u64>(),
            before in "[ 	]{1,4}",
            between in "[ 	]{1,4}",
            after in "[ 	]{0,4}",
        ) {
            let line = format!("{}{}{}{}{}", before, keyword, between, value, after);
            prop_assert_eq!(Params::of(&line).count(&keyword), Param::Read(value));
        }

        /// A keyword buried inside a longer word is not there at all, which
        /// is what the regexes this replaced got wrong.
        #[test]
        fn a_keyword_inside_a_longer_word_is_absent(
            keyword in keyword(),
            prefix in "[a-z]{1,6}",
            value in any::<u64>(),
        ) {
            let line = format!("{}{} {}", prefix, keyword, value);
            prop_assert_eq!(Params::of(&line).count(&keyword), Param::Absent);
            prop_assert!(!Params::of(&line).flag(&keyword));
        }

        /// The strict read is the standard library's, and differs from it only
        /// in saying which word would not parse.
        #[test]
        fn the_strict_read_agrees_with_the_standard_library(
            keyword in keyword(),
            word in word(),
        ) {
            let line = format!("{} {}", keyword, word);
            let read: Param<u8> = Params::of(&line).parse(&keyword);
            match word.parse::<u8>() {
                Ok(value) => prop_assert_eq!(read, Param::Read(value)),
                Err(_) => prop_assert_eq!(read, Param::Unreadable(&word)),
            }
        }

        /// A parameter is absent exactly when its keyword is not a word of
        /// the line, and bare exactly when it is the last of them.
        #[test]
        fn absent_and_bare_are_the_two_ways_there_is_no_word_to_read(
            line in ".*",
            keyword in keyword(),
        ) {
            let params = Params::of(&line);
            let words = params.words();
            let at = words.iter().position(|word| *word == keyword);
            prop_assert_eq!(
                matches!(params.count(&keyword), Param::Absent),
                at.is_none()
            );
            prop_assert_eq!(
                matches!(params.count(&keyword), Param::Bare),
                at.is_some_and(|at| at + 1 == words.len())
            );
        }
    }
}
