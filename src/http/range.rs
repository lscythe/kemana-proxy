/// An inclusive span of bytes that lies inside the file it was parsed for.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ByteSpan {
    pub first: u64,
    pub last: u64,
}

impl ByteSpan {
    pub fn len(&self) -> u64 {
        self.last - self.first + 1
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RangeRequest {
    /// The header is unusable, so the client gets the whole representation.
    Ignore,
    Satisfiable(ByteSpan),
    Unsatisfiable,
}

/// Reads a `Range` header value against a file of `length` bytes. Only a single
/// range in the `bytes` unit is honoured: RFC 9110 lets a server ignore any
/// other Range header, and a plain full response is always a correct reply.
pub fn parse(header: &str, length: u64) -> RangeRequest {
    let Some((unit, ranges)) = header.split_once('=') else {
        return RangeRequest::Ignore;
    };
    if !unit.eq_ignore_ascii_case("bytes") || ranges.contains(',') {
        return RangeRequest::Ignore;
    }
    let Some((start, end)) = ranges.split_once('-') else {
        return RangeRequest::Ignore;
    };
    match (start, end) {
        ("", "") => RangeRequest::Ignore,
        ("", suffix_length) => {
            number(suffix_length).map_or(RangeRequest::Ignore, |n| last_bytes(n, length))
        }
        (first, "") => number(first).map_or(RangeRequest::Ignore, |first| {
            from_first_byte(first, u64::MAX, length)
        }),
        (first, last) => match (number(first), number(last)) {
            (Some(first), Some(last)) if first <= last => from_first_byte(first, last, length),
            _ => RangeRequest::Ignore,
        },
    }
}

/// Plain decimal digits only: `str::parse` would also accept a leading `+`.
fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn from_first_byte(first: u64, requested_last: u64, length: u64) -> RangeRequest {
    if first >= length {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Satisfiable(ByteSpan {
        first,
        last: requested_last.min(length - 1),
    })
}

fn last_bytes(count: u64, length: u64) -> RangeRequest {
    if count == 0 || length == 0 {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Satisfiable(ByteSpan {
        first: length.saturating_sub(count),
        last: length - 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(first: u64, last: u64) -> RangeRequest {
        RangeRequest::Satisfiable(ByteSpan { first, last })
    }

    #[test]
    fn a_closed_range_selects_its_bytes() {
        assert_eq!(parse("bytes=2-4", 10), span(2, 4));
    }

    #[test]
    fn an_open_ended_range_runs_to_the_last_byte() {
        assert_eq!(parse("bytes=2-", 10), span(2, 9));
    }

    #[test]
    fn a_suffix_range_selects_the_last_bytes() {
        assert_eq!(parse("bytes=-3", 10), span(7, 9));
    }

    #[test]
    fn the_unit_is_case_insensitive() {
        assert_eq!(parse("BYTES=2-4", 10), span(2, 4));
        assert_eq!(parse("Bytes=2-4", 10), span(2, 4));
    }

    #[test]
    fn single_byte_ranges_at_both_ends_of_the_file() {
        assert_eq!(parse("bytes=0-0", 10), span(0, 0));
        assert_eq!(parse("bytes=9-9", 10), span(9, 9));
        assert_eq!(parse("bytes=9-", 10), span(9, 9));
        assert_eq!(parse("bytes=-1", 10), span(9, 9));
    }

    #[test]
    fn the_whole_file_as_a_range() {
        assert_eq!(parse("bytes=0-9", 10), span(0, 9));
        assert_eq!(parse("bytes=0-", 10), span(0, 9));
        assert_eq!(parse("bytes=-10", 10), span(0, 9));
    }

    #[test]
    fn an_end_at_or_past_the_file_is_clamped_to_the_last_byte() {
        assert_eq!(parse("bytes=5-9", 10), span(5, 9));
        assert_eq!(parse("bytes=5-10", 10), span(5, 9));
        assert_eq!(parse("bytes=5-100", 10), span(5, 9));
        assert_eq!(parse("bytes=0-18446744073709551615", 10), span(0, 9));
    }

    #[test]
    fn a_suffix_longer_than_the_file_selects_the_whole_file() {
        assert_eq!(parse("bytes=-11", 10), span(0, 9));
        assert_eq!(parse("bytes=-100", 10), span(0, 9));
        assert_eq!(parse("bytes=-18446744073709551615", 10), span(0, 9));
    }

    #[test]
    fn a_suffix_one_shorter_than_the_file_skips_only_the_first_byte() {
        assert_eq!(parse("bytes=-9", 10), span(1, 9));
    }

    #[test]
    fn a_start_at_or_past_the_end_is_unsatisfiable() {
        for header in [
            "bytes=10-",
            "bytes=10-10",
            "bytes=10-20",
            "bytes=11-",
            "bytes=18446744073709551615-",
            "bytes=18446744073709551615-18446744073709551615",
        ] {
            assert_eq!(parse(header, 10), RangeRequest::Unsatisfiable, "{header}");
        }
    }

    #[test]
    fn an_empty_suffix_is_unsatisfiable() {
        assert_eq!(parse("bytes=-0", 10), RangeRequest::Unsatisfiable);
        assert_eq!(parse("bytes=-00", 10), RangeRequest::Unsatisfiable);
    }

    #[test]
    fn every_range_on_an_empty_file_is_unsatisfiable() {
        for header in ["bytes=0-", "bytes=0-0", "bytes=0-5", "bytes=-1", "bytes=-0"] {
            assert_eq!(parse(header, 0), RangeRequest::Unsatisfiable, "{header}");
        }
    }

    #[test]
    fn several_ranges_are_ignored() {
        for header in [
            "bytes=0-1,3-4",
            "bytes=0-1, 3-4",
            "bytes=0-1,",
            "bytes=,0-1",
            "bytes=0-1,-2",
        ] {
            assert_eq!(parse(header, 10), RangeRequest::Ignore, "{header}");
        }
    }

    #[test]
    fn an_unknown_unit_is_ignored() {
        for header in ["items=0-1", "byte=0-1", "bytes0-1", "0-1", "=0-1"] {
            assert_eq!(parse(header, 10), RangeRequest::Ignore, "{header}");
        }
    }

    #[test]
    fn malformed_syntax_is_ignored() {
        for header in [
            "",
            "bytes",
            "bytes=",
            "bytes=-",
            "bytes=1",
            "bytes=a-b",
            "bytes=1-b",
            "bytes=a-2",
            "bytes=+1-2",
            "bytes=1-+2",
            "bytes=+-2",
            "bytes=1--2",
            "bytes=--2",
            "bytes= 1-2",
            "bytes=1 -2",
            "bytes=1-2x",
            "bytes=0x1-2",
            "bytes=1.5-2",
        ] {
            assert_eq!(parse(header, 10), RangeRequest::Ignore, "{header}");
        }
    }

    #[test]
    fn a_start_after_the_end_is_ignored_even_past_the_file() {
        assert_eq!(parse("bytes=3-2", 10), RangeRequest::Ignore);
        assert_eq!(parse("bytes=20-10", 10), RangeRequest::Ignore);
        assert_eq!(parse("bytes=5-3", 0), RangeRequest::Ignore);
    }

    #[test]
    fn numbers_that_overflow_u64_are_ignored() {
        for header in [
            "bytes=18446744073709551616-",
            "bytes=0-18446744073709551616",
            "bytes=-18446744073709551616",
            "bytes=99999999999999999999999999-",
        ] {
            assert_eq!(parse(header, 10), RangeRequest::Ignore, "{header}");
        }
    }

    #[test]
    fn leading_zeros_are_plain_decimal() {
        assert_eq!(parse("bytes=002-004", 10), span(2, 4));
    }

    #[test]
    fn the_largest_possible_file_does_not_overflow() {
        let length = u64::MAX;
        assert_eq!(parse("bytes=0-", length), span(0, u64::MAX - 1));
        assert_eq!(parse("bytes=-1", length), span(u64::MAX - 1, u64::MAX - 1));
        assert_eq!(
            parse("bytes=-18446744073709551615", length),
            span(0, u64::MAX - 1)
        );
        assert_eq!(
            parse("bytes=18446744073709551615-", length),
            RangeRequest::Unsatisfiable
        );
        let whole = ByteSpan {
            first: 0,
            last: u64::MAX - 1,
        };
        assert_eq!(whole.len(), u64::MAX);
    }

    #[test]
    fn a_span_reports_its_length() {
        assert_eq!(ByteSpan { first: 0, last: 0 }.len(), 1);
        assert_eq!(ByteSpan { first: 7, last: 9 }.len(), 3);
    }
}
