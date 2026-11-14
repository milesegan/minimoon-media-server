//! Conditional requests and byte ranges (spec §6, HTTP contract; RFC 9110 §13, §14).

use axum::http::{header, HeaderMap};

/// Whether an `If-None-Match` list matches an ETag, comparing weakly (RFC 9110 §13.1.2).
pub fn none_match(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let ours = opaque(etag);
    value
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || opaque(candidate) == ours)
}

/// An entity tag without its weakness prefix.
fn opaque(tag: &str) -> &str {
    tag.trim().strip_prefix("W/").unwrap_or(tag.trim())
}

/// Whether a range request may be honoured under `If-Range`: only on a strong ETag match.
///
/// A date never counts, though RFC 9110 §13.1.5 would allow one: `Last-Modified` can't show a
/// change that kept the mtime, which `rev` exists for (spec §6 allows only the ETag).
pub fn if_range_allows(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let value = value.trim();
    if value.starts_with("W/") {
        return false;
    }
    value.starts_with('"') && value == etag && !etag.starts_with("W/")
}

/// A parsed `Range` header against a representation of `size` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeRequest {
    /// No usable range: send the whole representation.
    Whole,
    /// One satisfiable range, inclusive.
    Range { start: u64, end: u64 },
    /// The range starts past the end: `416`.
    Unsatisfiable,
}

/// Parses a `Range` header. Several ranges, unknown units and malformed values all fall back
/// to the whole representation, which RFC 9110 §14.2 allows.
pub fn parse_range(headers: &HeaderMap, size: u64) -> RangeRequest {
    let Some(value) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
        return RangeRequest::Whole;
    };
    let value = value.trim();
    let Some((unit, specs)) = value.split_once('=') else {
        return RangeRequest::Whole;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return RangeRequest::Whole;
    }
    let specs: Vec<&str> = specs
        .split(',')
        .map(str::trim)
        .filter(|spec| !spec.is_empty())
        .collect();
    let [spec] = specs.as_slice() else {
        return RangeRequest::Whole;
    };
    let Some((first, last)) = spec.split_once('-') else {
        return RangeRequest::Whole;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // bytes=-n: the last n bytes.
        let Ok(suffix) = last.parse::<u64>() else {
            return RangeRequest::Whole;
        };
        if suffix == 0 || size == 0 {
            return RangeRequest::Unsatisfiable;
        }
        return RangeRequest::Range {
            start: size.saturating_sub(suffix),
            end: size - 1,
        };
    }
    let Ok(start) = first.parse::<u64>() else {
        return RangeRequest::Whole;
    };
    let end = if last.is_empty() {
        u64::MAX
    } else {
        match last.parse::<u64>() {
            Ok(end) if end >= start => end,
            _ => return RangeRequest::Whole,
        }
    };
    if start >= size {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Range {
        start,
        end: end.min(size - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(name: header::HeaderName, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn parses_the_three_range_forms() {
        let range = |v| parse_range(&headers(header::RANGE, v), 100);
        assert_eq!(range("bytes=0-9"), RangeRequest::Range { start: 0, end: 9 });
        assert_eq!(
            range("bytes=90-"),
            RangeRequest::Range { start: 90, end: 99 }
        );
        assert_eq!(
            range("bytes=-10"),
            RangeRequest::Range { start: 90, end: 99 }
        );
        assert_eq!(
            range("bytes=-500"),
            RangeRequest::Range { start: 0, end: 99 }
        );
        assert_eq!(
            range("bytes=50-500"),
            RangeRequest::Range { start: 50, end: 99 }
        );
        assert_eq!(range("bytes=100-"), RangeRequest::Unsatisfiable);
        assert_eq!(range("bytes=-0"), RangeRequest::Unsatisfiable);
        assert_eq!(range("bytes=0-1,5-6"), RangeRequest::Whole);
        assert_eq!(range("bytes=9-1"), RangeRequest::Whole);
        assert_eq!(range("items=0-1"), RangeRequest::Whole);
        assert_eq!(range("bytes=x-1"), RangeRequest::Whole);
        assert_eq!(
            parse_range(&headers(header::RANGE, "bytes=0-"), 0),
            RangeRequest::Unsatisfiable
        );
    }

    #[test]
    fn compares_etags_weakly_for_if_none_match() {
        assert!(none_match(
            &headers(header::IF_NONE_MATCH, "W/\"a\""),
            "\"a\""
        ));
        assert!(none_match(
            &headers(header::IF_NONE_MATCH, "\"b\", \"a\""),
            "W/\"a\""
        ));
        assert!(none_match(&headers(header::IF_NONE_MATCH, "*"), "\"a\""));
        assert!(!none_match(
            &headers(header::IF_NONE_MATCH, "\"b\""),
            "\"a\""
        ));
        assert!(!none_match(&HeaderMap::new(), "\"a\""));
    }

    #[test]
    fn if_range_needs_a_strong_match() {
        let date = "Sun, 10 Mar 2024 16:00:00 GMT";
        assert!(if_range_allows(
            &headers(header::IF_RANGE, "\"a\""),
            "\"a\""
        ));
        assert!(!if_range_allows(
            &headers(header::IF_RANGE, "\"b\""),
            "\"a\""
        ));
        assert!(!if_range_allows(
            &headers(header::IF_RANGE, "W/\"a\""),
            "\"a\""
        ));
        assert!(!if_range_allows(&headers(header::IF_RANGE, date), "\"a\""));
        assert!(if_range_allows(&HeaderMap::new(), "\"a\""));
    }
}
