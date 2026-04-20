//! Best-effort parser for USGS Mineral Commodity Summaries PDFs.
//!
//! The USGS MCS has a consistent but not strict format. Each PDF has a
//! "World Mine Production, Reserves, and Resources" section with a table
//! of country rows. We scan for these rows and emit structured data.
//!
//! ## Extraction quality caveat
//!
//! `pdf-extract` is pure-Rust and its tabular output is imperfect —
//! column spacing varies, footnote markers leak in (`W`, `e`, `r`), and
//! occasionally numbers get concatenated with text. This parser is
//! **best-effort**: it emits what it can cleanly recognize and silently
//! skips lines it can't. Missing data is not an error.
//!
//! The robust extraction story is the Level-2 LLM recipe path
//! (ADR 0007). This parser exists so that Phase 3 can demonstrate the
//! end-to-end pipeline with zero LLM dependency.
//!
//! ## What we extract
//!
//! One [`ParsedRow`] per recognized country × year × metric. We look
//! for lines shaped like:
//!
//! ```text
//! Chile          49,000   53,000   9,300,000
//! ```
//!
//! Where the country is a known label, the two middle columns are
//! production in the current and prior year, and the last column is
//! reserves. Years come from a header row like `2023   2024   Reserves`.

use regex::Regex;
use std::sync::OnceLock;
use thiserror::Error;

/// One row extracted from a USGS MCS production table.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRow {
    /// Country label as it appeared in the PDF. May be a U.S. state
    /// name in some reports. Preserved verbatim for debugging; the
    /// caller is responsible for mapping to canonical identifiers.
    pub country_label: String,
    /// The year this value is for, if a header row gave us one.
    pub year: Option<u16>,
    /// Numeric value, stripped of commas and footnote markers.
    pub value: f64,
    /// Which column this came from.
    pub kind: RowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Production,
    Reserves,
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("pdf text extraction failed: {0}")]
    Extract(String),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract text from PDF bytes. Thin wrapper over `pdf-extract` so
/// callers don't have to know the crate's specific error type.
pub fn extract_text(bytes: &[u8]) -> Result<String, ParseError> {
    pdf_extract::extract_text_from_mem(bytes).map_err(|e| ParseError::Extract(e.to_string()))
}

/// Parse a USGS MCS text blob into production/reserves rows.
/// Returns an empty vec rather than an error when no rows match —
/// this parser is best-effort (see module docs).
pub fn parse_production_rows(text: &str) -> Vec<ParsedRow> {
    let (year_curr, year_prev) = detect_year_columns(text);
    let mut out = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Skip obvious non-data lines quickly.
        if line.len() > 200 {
            continue;
        }

        if let Some(row) = match_country_row(line, year_curr, year_prev) {
            out.extend(row);
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Look for a line like `Country   2023   2024   Reserves` and return
/// the year columns. Falls back to `(None, None)` if we can't find
/// recognizable 4-digit years.
fn detect_year_columns(text: &str) -> (Option<u16>, Option<u16>) {
    // Match two consecutive 4-digit years on the same line (loose —
    // years can be anywhere between 1990 and 2099 for realism).
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b(19\d{2}|20\d{2})\b\s+\b(19\d{2}|20\d{2})\b").unwrap());

    for line in text.lines() {
        if let Some(caps) = re.captures(line) {
            let a: u16 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
            let b: u16 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
            if a > 1990 && b > 1990 && (a as i32 - b as i32).abs() <= 2 {
                // Ordered as they appear: usually earlier-year, later-year.
                return (Some(a), Some(b));
            }
        }
    }
    (None, None)
}

/// Try to match a country-data line. Returns up to three rows (prior
/// year production, current year production, reserves) when the line
/// looks like table data.
fn match_country_row(
    line: &str,
    year_col_prev: Option<u16>,
    year_col_curr: Option<u16>,
) -> Option<Vec<ParsedRow>> {
    // Strategy: split on whitespace. If the line starts with a
    // recognizable country label and is followed by 2-4 numeric
    // columns, treat it as data.
    let trimmed = line.trim_end_matches(|c: char| c == '.' || c.is_whitespace());

    // Find the split point: country name (words) then numbers.
    // A country name is any sequence of word tokens (letters, spaces,
    // commas, periods, hyphens) followed by the first token that
    // parses as a number.
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }

    let mut split_idx: Option<usize> = None;
    for (i, tok) in tokens.iter().enumerate() {
        if looks_numeric(tok) {
            split_idx = Some(i);
            break;
        }
    }
    let split_idx = split_idx?;
    if split_idx == 0 {
        return None; // line starts with a number, not a country row
    }

    let label_tokens = &tokens[..split_idx];
    let label = label_tokens.join(" ");
    if !is_plausible_country_label(&label) {
        return None;
    }

    let number_tokens = &tokens[split_idx..];
    // We expect at least two numbers (two years of production) and
    // optionally a reserves column. Some rows have only one numeric
    // column (e.g. footnotes). Skip those.
    let numbers: Vec<f64> = number_tokens
        .iter()
        .filter_map(|t| clean_number(t))
        .collect();
    if numbers.len() < 2 {
        return None;
    }

    let mut rows = Vec::new();

    // Production columns: interpret first two numbers as
    // prev-year, curr-year when we know the header.
    match (year_col_prev, year_col_curr) {
        (Some(yp), Some(yc)) => {
            rows.push(ParsedRow {
                country_label: label.clone(),
                year: Some(yp),
                value: numbers[0],
                kind: RowKind::Production,
            });
            rows.push(ParsedRow {
                country_label: label.clone(),
                year: Some(yc),
                value: numbers[1],
                kind: RowKind::Production,
            });
        }
        _ => {
            // No header info — emit rows without a year.
            rows.push(ParsedRow {
                country_label: label.clone(),
                year: None,
                value: numbers[0],
                kind: RowKind::Production,
            });
            rows.push(ParsedRow {
                country_label: label.clone(),
                year: None,
                value: numbers[1],
                kind: RowKind::Production,
            });
        }
    }

    if numbers.len() >= 3 {
        rows.push(ParsedRow {
            country_label: label,
            year: None,
            value: numbers[2],
            kind: RowKind::Reserves,
        });
    }

    Some(rows)
}

/// Does this token look like a number (possibly with commas or a
/// single trailing footnote letter like `e` or `r`)?
fn looks_numeric(tok: &str) -> bool {
    clean_number(tok).is_some()
}

/// Strip commas, percentage signs, and single trailing footnote
/// letters. Parse what remains as f64.
fn clean_number(tok: &str) -> Option<f64> {
    let t = tok.trim_end_matches(|c: char| matches!(c, ',' | '.' | ';'));
    // Drop a single trailing footnote letter (case-insensitive).
    let t = if let Some(last) = t.chars().last() {
        if last.is_ascii_alphabetic() {
            &t[..t.len() - last.len_utf8()]
        } else {
            t
        }
    } else {
        t
    };
    let cleaned: String = t.chars().filter(|c| *c != ',').collect();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// Heuristic: a country label is mostly letters, is not too long, and
/// isn't one of a few obvious non-country strings that appear in
/// narrative text.
fn is_plausible_country_label(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    // At least 3 alphabetic chars
    if s.chars().filter(|c| c.is_alphabetic()).count() < 3 {
        return false;
    }
    // Reject lines that are clearly not country rows
    let lower = s.to_ascii_lowercase();
    const REJECT_PREFIXES: &[&str] = &[
        "events",
        "tariff",
        "world war",
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
        "production",
        "reserves",
        "consumption",
        "table ",
        "figure ",
        "source",
        "note",
    ];
    if REJECT_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Hand-crafted text that mimics post-pdf-extract output of a USGS
    // MCS commodity table. Whitespace is squashed because that's what
    // pdf-extract typically does.
    const SAMPLE_TEXT: &str = "\
LITHIUM

Salient Statistics

World Mine Production, Reserves, and Resources:
                                 Mine production
                                 2023    2024    Reserves
United States                   1,100    1,400    800,000
Argentina                       9,600   18,000  4,000,000
Australia                      86,000   88,000  8,400,000
Brazil                          4,900    7,000  1,400,000
Chile                          49,000   53,000  9,300,000
China                          33,000   41,000  3,000,000
Portugal                          380      380     60,000
Zimbabwe                        1,700    5,100    690,000
World total (rounded)         220,000  240,000 30,000,000
";

    #[test]
    fn detects_year_columns() {
        let (a, b) = detect_year_columns(SAMPLE_TEXT);
        assert_eq!(a, Some(2023));
        assert_eq!(b, Some(2024));
    }

    #[test]
    fn parses_chile_production_rows() {
        let rows = parse_production_rows(SAMPLE_TEXT);
        // Chile should appear with both year rows plus reserves
        let chile: Vec<_> = rows
            .iter()
            .filter(|r| r.country_label == "Chile")
            .collect();
        assert_eq!(chile.len(), 3);

        let prod_2023 = chile
            .iter()
            .find(|r| r.year == Some(2023) && r.kind == RowKind::Production)
            .expect("Chile 2023 production");
        assert_eq!(prod_2023.value, 49_000.0);

        let prod_2024 = chile
            .iter()
            .find(|r| r.year == Some(2024) && r.kind == RowKind::Production)
            .expect("Chile 2024 production");
        assert_eq!(prod_2024.value, 53_000.0);

        let reserves = chile
            .iter()
            .find(|r| r.kind == RowKind::Reserves)
            .expect("Chile reserves");
        assert_eq!(reserves.value, 9_300_000.0);
    }

    #[test]
    fn parses_multiple_countries() {
        let rows = parse_production_rows(SAMPLE_TEXT);
        let countries: std::collections::HashSet<_> =
            rows.iter().map(|r| r.country_label.clone()).collect();
        // Should pick up at least the major producers; "World total"
        // may or may not get parsed depending on numeric cleanup.
        assert!(countries.contains("Chile"));
        assert!(countries.contains("Australia"));
        assert!(countries.contains("China"));
        assert!(countries.contains("United States"));
    }

    #[test]
    fn skips_non_country_lines() {
        let rows = parse_production_rows(SAMPLE_TEXT);
        // None of the rows should have a label like "Mine production" or
        // "Salient Statistics".
        for row in &rows {
            let lower = row.country_label.to_lowercase();
            assert!(!lower.starts_with("mine production"));
            assert!(!lower.starts_with("salient"));
            assert!(!lower.starts_with("reserves"));
        }
    }

    #[test]
    fn clean_number_handles_commas_and_footnotes() {
        assert_eq!(clean_number("49,000"), Some(49_000.0));
        assert_eq!(clean_number("49,000e"), Some(49_000.0));
        assert_eq!(clean_number("1,400"), Some(1_400.0));
        assert_eq!(clean_number("NA"), None);
        assert_eq!(clean_number("W"), None); // withheld — single letter, no number
        assert_eq!(clean_number("380"), Some(380.0));
    }

    #[test]
    fn empty_text_returns_empty() {
        let rows = parse_production_rows("");
        assert!(rows.is_empty());
    }

    #[test]
    fn handles_missing_year_header_gracefully() {
        let text = "\
Chile          49,000   53,000   9,300,000
Australia      86,000   88,000   8,400,000
";
        let rows = parse_production_rows(text);
        // Without a year header, rows come back with year = None
        assert!(!rows.is_empty());
        let chile_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.country_label == "Chile")
            .collect();
        assert_eq!(chile_rows.len(), 3);
        for r in &chile_rows {
            if r.kind == RowKind::Production {
                assert!(r.year.is_none());
            }
        }
    }
}
