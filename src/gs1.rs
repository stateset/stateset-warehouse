//! GS1 barcode element strings (GS1-128, GS1 DataMatrix and QR), as sent
//! by scanners. One scan can carry the product (GTIN), lot, expiry and
//! serial. Values are returned exactly as encoded: nothing is trimmed,
//! padded or guessed, and dates stay dates, because turning a label date
//! into the exact expiry instant the WMS records needs a reviewed facility
//! policy.
use serde::Serialize;
use std::collections::BTreeMap;

/// Group separator (FNC1 in data) that ends a variable-length field.
const GS: char = '\u{1d}';

#[derive(Debug, Clone, Copy)]
struct Ai {
    code: &'static str,
    name: &'static str,
    /// Fixed data length, or `None` for variable length up to `max`.
    fixed: Option<usize>,
    max: usize,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Digits,
    Text,
    Date,
}

/// The application identifiers a warehouse needs; others are rejected so a
/// scan never half-parses.
const AIS: &[Ai] = &[
    Ai {
        code: "00",
        name: "sscc",
        fixed: Some(18),
        max: 18,
        kind: Kind::Digits,
    },
    Ai {
        code: "01",
        name: "gtin",
        fixed: Some(14),
        max: 14,
        kind: Kind::Digits,
    },
    Ai {
        code: "02",
        name: "content_gtin",
        fixed: Some(14),
        max: 14,
        kind: Kind::Digits,
    },
    Ai {
        code: "10",
        name: "lot",
        fixed: None,
        max: 20,
        kind: Kind::Text,
    },
    Ai {
        code: "11",
        name: "production_date",
        fixed: Some(6),
        max: 6,
        kind: Kind::Date,
    },
    Ai {
        code: "13",
        name: "packaging_date",
        fixed: Some(6),
        max: 6,
        kind: Kind::Date,
    },
    Ai {
        code: "15",
        name: "best_before",
        fixed: Some(6),
        max: 6,
        kind: Kind::Date,
    },
    Ai {
        code: "17",
        name: "expiry",
        fixed: Some(6),
        max: 6,
        kind: Kind::Date,
    },
    Ai {
        code: "21",
        name: "serial",
        fixed: None,
        max: 20,
        kind: Kind::Text,
    },
    Ai {
        code: "30",
        name: "count",
        fixed: None,
        max: 8,
        kind: Kind::Digits,
    },
    Ai {
        code: "37",
        name: "content_count",
        fixed: None,
        max: 8,
        kind: Kind::Digits,
    },
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Element {
    pub ai: String,
    pub name: String,
    /// The exact encoded value.
    pub value: String,
    /// For dates: the calendar date it denotes (ISO 8601), per the GS1
    /// century rule; day 00 means the last day of the month.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Parsed {
    pub elements: Vec<Element>,
    /// Convenience view keyed by name (gtin, lot, expiry, serial, ...).
    pub fields: BTreeMap<String, String>,
}

/// True when the scan looks like a GS1 element string rather than a plain
/// SKU or bin code.
pub fn looks_like_gs1(raw: &str) -> bool {
    let data = strip_symbology(raw);
    data.starts_with('(')
        || data.contains(GS)
        || raw.starts_with("]C1")
        || raw.starts_with("]d2")
        || raw.starts_with("]Q3")
}

fn strip_symbology(raw: &str) -> &str {
    for prefix in ["]C1", "]d2", "]Q3", "]e0"] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest;
        }
    }
    raw
}

/// Modulo-10 check digit used by GTIN and SSCC.
pub fn check_digit_ok(digits: &str) -> bool {
    let bytes = digits.as_bytes();
    if bytes.len() < 2 || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let body = &bytes[..bytes.len() - 1];
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    (10 - sum % 10) % 10 == u32::from(bytes[bytes.len() - 1] - b'0')
}

/// GS1 YYMMDD, with the GS1 General Specifications century rule relative to
/// `today_year` (a two-digit year more than 50 years ahead belongs to the
/// previous century; more than 49 behind, to the next).
pub fn gs1_date(value: &str, today_year: i32) -> Result<String, String> {
    if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("Date {value} must be YYMMDD"));
    }
    let yy: i32 = value[0..2].parse().map_err(|_| "Bad year")?;
    let month: u32 = value[2..4].parse().map_err(|_| "Bad month")?;
    let day: u32 = value[4..6].parse().map_err(|_| "Bad day")?;
    let century = today_year - today_year.rem_euclid(100);
    let current = today_year.rem_euclid(100);
    let difference = yy - current;
    let year = if difference >= 51 {
        century - 100 + yy
    } else if difference <= -50 {
        century + 100 + yy
    } else {
        century + yy
    };
    if !(1..=12).contains(&month) {
        return Err(format!("Date {value} has no month {month}"));
    }
    let last = (28..=31)
        .rev()
        .find(|d| chrono::NaiveDate::from_ymd_opt(year, month, *d).is_some())
        .ok_or("Bad date")?;
    let day = if day == 0 { last } else { day };
    chrono::NaiveDate::from_ymd_opt(year, month, day)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .ok_or_else(|| format!("Date {value} is not a calendar date"))
}

fn valid_text(value: &str) -> bool {
    // GS1 AI encodable character set 82: printable ASCII except a few.
    !value.is_empty()
        && value.bytes().all(|b| {
            (0x21..=0x7e).contains(&b)
                && !matches!(
                    b,
                    b'#' | b'$'
                        | b'@'
                        | b'['
                        | b'\\'
                        | b']'
                        | b'^'
                        | b'`'
                        | b'{'
                        | b'|'
                        | b'}'
                        | b'~'
                )
        })
}

/// Parse a scanned GS1 element string. Accepts FNC1/GS-separated data (with
/// or without a symbology identifier such as `]C1`) and the bracketed human
/// readable form `(01)…(10)…`.
pub fn parse(raw: &str, today_year: i32) -> Result<Parsed, String> {
    let data = strip_symbology(raw.trim_end_matches(['\r', '\n']));
    if data.is_empty() || data.len() > 512 {
        return Err("Scan is empty or longer than 512 characters".into());
    }
    let pairs = if data.starts_with('(') {
        bracketed(data)?
    } else {
        unbracketed(data)?
    };
    let mut elements = vec![];
    let mut fields = BTreeMap::new();
    for (code, value) in pairs {
        let ai = AIS
            .iter()
            .find(|a| a.code == code)
            .ok_or_else(|| format!("Application identifier ({code}) is not supported"))?;
        let length_ok = match ai.fixed {
            Some(n) => value.len() == n,
            None => (1..=ai.max).contains(&value.len()),
        };
        if !length_ok {
            return Err(format!("({code}) {} has the wrong length", ai.name));
        }
        let mut date = None;
        match ai.kind {
            Kind::Digits if !value.bytes().all(|b| b.is_ascii_digit()) => {
                return Err(format!("({code}) {} must be digits", ai.name))
            }
            Kind::Text if !valid_text(&value) => {
                return Err(format!(
                    "({code}) {} has characters GS1 does not allow",
                    ai.name
                ))
            }
            Kind::Date => date = Some(gs1_date(&value, today_year)?),
            _ => {}
        }
        if matches!(ai.code, "00" | "01" | "02") && !check_digit_ok(&value) {
            return Err(format!(
                "({code}) {} {value} fails its check digit",
                ai.name
            ));
        }
        if fields.insert(ai.name.to_string(), value.clone()).is_some() {
            return Err(format!("({code}) {} appears twice", ai.name));
        }
        elements.push(Element {
            ai: code,
            name: ai.name.into(),
            value,
            date,
        });
    }
    Ok(Parsed { elements, fields })
}

fn bracketed(data: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = vec![];
    let mut rest = data;
    while !rest.is_empty() {
        let inner = rest
            .strip_prefix('(')
            .ok_or("Expected (AI) before each value")?;
        let close = inner.find(')').ok_or("Unclosed (AI)")?;
        let code = inner[..close].to_string();
        let after = &inner[close + 1..];
        let end = after.find('(').unwrap_or(after.len());
        out.push((code, after[..end].to_string()));
        rest = &after[end..];
    }
    Ok(out)
}

fn unbracketed(data: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = vec![];
    let mut rest = data;
    while !rest.is_empty() {
        let ai = AIS
            .iter()
            .find(|a| rest.starts_with(a.code))
            .ok_or_else(|| {
                format!(
                    "Unsupported application identifier at {}",
                    rest.chars().take(4).collect::<String>()
                )
            })?;
        let body = &rest[ai.code.len()..];
        let (value, next) = match ai.fixed {
            Some(n) => {
                if body.len() < n {
                    return Err(format!("({}) {} is truncated", ai.code, ai.name));
                }
                let next = body[n..].strip_prefix(GS).unwrap_or(&body[n..]);
                (&body[..n], next)
            }
            None => match body.find(GS) {
                Some(i) => (&body[..i], &body[i + 1..]),
                None => (body, ""),
            },
        };
        out.push((ai.code.to_string(), value.to_string()));
        rest = next;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_digits_match_gs1_examples() {
        assert!(check_digit_ok("09506000134352"));
        assert!(check_digit_ok("00012345600012"));
        assert!(!check_digit_ok("09506000134353"));
        assert!(check_digit_ok("106141411234567897"), "SSCC");
    }

    #[test]
    fn parses_fnc1_and_bracketed_forms_identically() {
        let gs = "]C1010950600013435217261231\u{1d}10LOT-7\u{1d}21SN 1"; // space not allowed
        assert!(parse(gs, 2026).is_err(), "space is not GS1 text");
        let gs = "]C10109506000134352172612311 0LOT-7";
        assert!(parse(gs, 2026).is_err(), "unknown AI after a fixed field");
        let gs = "]C1010950600013435217261231".to_string() + "10LOT-7\u{1d}21SN-1";
        let a = parse(&gs, 2026).expect("FNC1 form");
        let b = parse("(01)09506000134352(17)261231(10)LOT-7(21)SN-1", 2026).expect("bracketed");
        assert_eq!(a, b);
        assert_eq!(a.fields["gtin"], "09506000134352");
        assert_eq!(a.fields["lot"], "LOT-7");
        assert_eq!(a.fields["serial"], "SN-1");
        assert_eq!(a.elements[1].date.as_deref(), Some("2026-12-31"));
    }

    #[test]
    fn dates_follow_the_century_rule_and_day_zero() {
        assert_eq!(gs1_date("270200", 2026).expect("day 00"), "2027-02-28");
        assert_eq!(gs1_date("280200", 2026).expect("leap"), "2028-02-29");
        assert_eq!(
            gs1_date("990101", 2026).expect("past century"),
            "1999-01-01"
        );
        assert_eq!(gs1_date("760101", 2026).expect("50 ahead"), "2076-01-01");
        assert_eq!(gs1_date("770101", 2026).expect("51 ahead"), "1977-01-01");
        assert!(gs1_date("261301", 2026).is_err());
        assert!(gs1_date("260230", 2026).is_err());
    }

    #[test]
    fn rejects_bad_check_digits_lengths_and_duplicates() {
        assert!(parse("(01)09506000134353", 2026).is_err());
        assert!(parse("(01)0950600013435", 2026).is_err());
        assert!(parse("(10)A(10)B", 2026).is_err());
        assert!(parse("(10)ABCDEFGHIJKLMNOPQRSTU", 2026).is_err(), "21 > 20");
        assert!(parse("(99)X", 2026).is_err(), "unsupported AI");
        assert!(parse("", 2026).is_err());
    }

    #[test]
    fn recognizes_gs1_scans_but_not_plain_codes() {
        assert!(looks_like_gs1("]C10109506000134352"));
        assert!(looks_like_gs1("(01)09506000134352"));
        assert!(looks_like_gs1("010950600013435210A\u{1d}21B"));
        assert!(!looks_like_gs1("A-01-01"));
        assert!(!looks_like_gs1("SKU-123"));
    }
}
