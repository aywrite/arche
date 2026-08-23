//! Reading the parameters off a UCI command.
//!
//! A command is a line of whitespace separated words, and a parameter is a
//! keyword followed by its value: `go wtime 300000 winc 2000 movestogo 40`.
//! The words are split once and asked for what the command takes, rather than
//! the line being searched again for every keyword.
//!
//! Whole words throughout. A keyword only counts where it stands as a word of
//! its own, so `movetime 500` cannot be read as a value for `time`, and a
//! position whose fen happens to contain a keyword cannot be read as carrying
//! one.

use std::num::IntErrorKind;
use std::str::FromStr;

/// The words of one command line.
pub(crate) struct Params<'a> {
    words: Vec<&'a str>,
}

/// What reading one parameter found.
///
/// Which of the three is worth acting on differs by parameter, so this says
/// what happened rather than deciding: an unreadable clock is safest read as a
/// spent one, since discarding it would leave the search unbounded at the
/// moment there is least time to spare, while an unreadable depth is better
/// ignored than obeyed as zero, which would return no move at all.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Param<'a, T> {
    /// The keyword is not among the words, or is the last of them and so has
    /// nothing after it to read.
    Absent,
    /// The keyword is there and what follows it is not a value. Carries the
    /// word, for a caller that wants to say which it was.
    Unreadable(&'a str),
    Read(T),
}

impl<'a> Params<'a> {
    pub(crate) fn of(line: &'a str) -> Self {
        Self {
            words: line.split_whitespace().collect(),
        }
    }

    /// Whether `keyword` stands among the words on its own.
    pub(crate) fn flag(&self, keyword: &str) -> bool {
        self.words.contains(&keyword)
    }

    /// The word following `keyword`, if the keyword is there and is not last.
    pub(crate) fn value(&self, keyword: &str) -> Option<&'a str> {
        let at = self.words.iter().position(|word| *word == keyword)?;
        self.words.get(at + 1).copied()
    }

    /// A count of milliseconds or nodes, read the way the protocol's unsigned
    /// parameters have to be read.
    ///
    /// Two values that are not counts are still meant rather than mistyped.
    /// Below zero is what the match tools send once their time margin has been
    /// eaten into, and says the clock is spent rather than that the line is
    /// bad. Too large to hold is a request for everything there is. Both are
    /// read; only a word that is no kind of number is `Unreadable`.
    pub(crate) fn count(&self, keyword: &str) -> Param<'a, u64> {
        let Some(word) = self.value(keyword) else {
            return Param::Absent;
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

    /// A parameter parsed as whatever the caller asks for, with none of the
    /// leniency `count` applies. What will not parse is `Unreadable`, which is
    /// what a command wants when the wrong value is worth refusing rather than
    /// rounding: `bench 300` is a typo for a depth, not a request for one.
    pub(crate) fn parse<T: FromStr>(&self, keyword: &str) -> Param<'a, T> {
        match self.value(keyword) {
            None => Param::Absent,
            Some(word) => match word.parse() {
                Ok(value) => Param::Read(value),
                Err(_) => Param::Unreadable(word),
            },
        }
    }
}

/// Whether the word is a negative number rather than something that is no
/// number at all. Anything after the sign has to be digits, and there has to
/// be at least one, so a bare `-` is not one.
fn is_negative(word: &str) -> bool {
    match word.strip_prefix('-') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

impl<'a, T> Param<'a, T> {
    /// The value read, with an absent or unreadable parameter alike discarded.
    pub(crate) fn read(self) -> Option<T> {
        match self {
            Param::Read(value) => Some(value),
            Param::Absent | Param::Unreadable(_) => None,
        }
    }

    /// The value read, with an unreadable parameter standing in as `instead`.
    /// An absent one is still absent: a parameter that was not sent is not the
    /// same as one that was sent wrong.
    pub(crate) fn read_or(self, instead: T) -> Option<T> {
        match self {
            Param::Read(value) => Some(value),
            Param::Unreadable(_) => Some(instead),
            Param::Absent => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Param, Params};

    #[test]
    fn a_value_is_the_word_after_its_keyword() {
        let params = Params::of("go wtime 300000 winc 2000");
        assert_eq!(params.value("wtime"), Some("300000"));
        assert_eq!(params.value("winc"), Some("2000"));
        assert_eq!(params.value("btime"), None);
    }

    #[test]
    fn a_keyword_only_counts_as_a_whole_word() {
        // the value belongs to movetime, and nothing here says time
        let params = Params::of("go movetime 500");
        assert_eq!(params.value("time"), None);
        assert_eq!(params.count("time"), Param::Absent);
        assert_eq!(params.count("movetime"), Param::Read(500));
        assert!(!params.flag("move"));
        assert!(params.flag("movetime"));
    }

    #[test]
    fn a_keyword_with_nothing_after_it_is_absent() {
        let params = Params::of("go depth");
        assert!(params.flag("depth"));
        assert_eq!(params.count("depth"), Param::Absent);
    }

    #[test]
    fn any_amount_of_space_separates_words() {
        let params = Params::of("  go   wtime\t300000  ");
        assert_eq!(params.count("wtime"), Param::Read(300_000));
    }

    #[test]
    fn a_count_below_zero_is_a_spent_one() {
        // what the match tools send once their time margin has been eaten into
        let params = Params::of("go wtime -5 btime -0 winc -99999999999999999999");
        assert_eq!(params.count("wtime"), Param::Read(0));
        assert_eq!(params.count("btime"), Param::Read(0));
        assert_eq!(params.count("winc"), Param::Read(0));
    }

    #[test]
    fn a_count_too_large_to_hold_is_the_largest_we_can() {
        let params = Params::of("go wtime 99999999999999999999999");
        assert_eq!(params.count("wtime"), Param::Read(u64::MAX));
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
        assert_eq!(Params::of("bench").parse::<u8>("bench"), Param::Absent);
    }

    #[test]
    fn read_discards_both_kinds_of_missing_value() {
        assert_eq!(Params::of("go").count("wtime").read(), None);
        assert_eq!(Params::of("go wtime x").count("wtime").read(), None);
        assert_eq!(Params::of("go wtime 5").count("wtime").read(), Some(5));
    }

    #[test]
    fn read_or_stands_in_for_an_unreadable_value_but_not_an_absent_one() {
        assert_eq!(Params::of("go").count("wtime").read_or(0), None);
        assert_eq!(Params::of("go wtime x").count("wtime").read_or(0), Some(0));
        assert_eq!(Params::of("go wtime 5").count("wtime").read_or(0), Some(5));
    }

    #[test]
    fn an_empty_line_has_no_parameters_and_does_not_panic() {
        let params = Params::of("");
        assert_eq!(params.value("wtime"), None);
        assert_eq!(params.count("wtime"), Param::Absent);
        assert!(!params.flag("infinite"));
    }

    #[test]
    fn a_keyword_used_twice_reads_the_first() {
        // the protocol does not send one twice; reading the first is what a
        // left to right scan does and is worth pinning either way
        let params = Params::of("go wtime 100 wtime 200");
        assert_eq!(params.count("wtime"), Param::Read(100));
    }

    #[test]
    fn a_value_may_be_a_keyword_of_its_own() {
        // nothing stops an interface sending them adjacent, and the scan must
        // not lose the second one
        let params = Params::of("go depth infinite");
        assert_eq!(params.count("depth"), Param::Unreadable("infinite"));
        assert!(params.flag("infinite"));
    }
}
