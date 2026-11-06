//! Money written the way people write it.
//!
//! Currency detection is a parsing problem, not a pattern-matching one. The
//! symbol sits before the amount in some places and after it in others,
//! thousands are grouped with a space, a dot, a comma or an apostrophe
//! depending on the country, and the same comma means "thousands" in one
//! convention and "decimal point" in another. A pattern that assumes one
//! arrangement does not merely miss the others: run across a page of prices
//! it matches the tail of one and the head of the next, and returns a number
//! that appears nowhere on the page.
//!
//! A wrong price is worse than no price — it becomes an `Offer` in a
//! suggested graph and ships to a client's structured data, so everything
//! ambiguous here resolves to nothing.

use std::sync::OnceLock;

use fancy_regex::Regex;
use serde::Serialize;

use spiderhead_parse::py_str::py_strip;

/// Currency markers people actually write, each with the ISO code it resolves
/// to. A marker that cannot name one currency — a bare "kr" is Swedish,
/// Norwegian, Danish or Icelandic — is recognised as money without claiming
/// which.
const CURRENCY_MARKERS: &[(&str, Option<&str>)] = &[
    // Multi-character first: "R$" must win over "$", "Kč" over "K".
    ("руб.", Some("RUB")),
    ("руб", Some("RUB")),
    ("рублей", Some("RUB")),
    ("р.", Some("RUB")),
    ("у.е.", None),
    ("R$", Some("BRL")),
    ("CA$", Some("CAD")),
    ("AU$", Some("AUD")),
    ("HK$", Some("HKD")),
    ("CN¥", Some("CNY")),
    ("Kč", Some("CZK")),
    ("zł", Some("PLN")),
    ("CHF", Some("CHF")),
    ("Br", Some("BYN")),
    ("kr", None),
    ("Fr", None),
    ("₽", Some("RUB")),
    ("$", Some("USD")),
    ("€", Some("EUR")),
    ("£", Some("GBP")),
    ("¥", Some("JPY")),
    ("₴", Some("UAH")),
    ("₸", Some("KZT")),
    ("₺", Some("TRY")),
    ("₹", Some("INR")),
    ("₩", Some("KRW")),
    ("₫", Some("VND")),
    ("₪", Some("ILS")),
    ("USD", Some("USD")),
    ("EUR", Some("EUR")),
    ("GBP", Some("GBP")),
    ("RUB", Some("RUB")),
    ("BYN", Some("BYN")),
    ("UAH", Some("UAH")),
    ("KZT", Some("KZT")),
    ("PLN", Some("PLN")),
    ("CZK", Some("CZK")),
    ("SEK", Some("SEK")),
    ("NOK", Some("NOK")),
    ("DKK", Some("DKK")),
    ("TRY", Some("TRY")),
    ("JPY", Some("JPY")),
    ("CNY", Some("CNY")),
    ("INR", Some("INR")),
    ("BRL", Some("BRL")),
    ("CAD", Some("CAD")),
    ("AUD", Some("AUD")),
];

/// Characters read as a thousand group when they stand between digit runs:
/// every space that appears between thousands in the wild (ordinary, no-break,
/// narrow no-break and thin — copy-pasted prices carry all four), plus dot,
/// comma and the typographic apostrophe.
const GROUP_SEPARATORS: &str = " \u{00a0}\u{202f}\u{2009}.,'’";

/// Escape one marker, blocking a bare $ or ¥ from matching inside a qualified
/// symbol.
///
/// "CA$", "AU$", "HK$" and "CN¥" are their own listed markers and win on their
/// own, since the leftmost successful match wins. Without this guard, a
/// country prefix the table does not recognise (any other "XX$") would still
/// let the bare symbol match starting one character later, silently recoding
/// an unknown currency as USD or JPY.
fn marker_pattern(marker: &str) -> String {
    let escaped = regex::escape(marker);
    if marker == "$" || marker == "¥" {
        format!(r"(?<![A-Za-z]){escaped}")
    } else {
        escaped
    }
}

fn marker_alternation() -> String {
    let mut markers: Vec<&str> = CURRENCY_MARKERS.iter().map(|(marker, _)| *marker).collect();
    // Longest first; the stable sort keeps equal-length markers in table order.
    markers.sort_by_key(|marker| std::cmp::Reverse(marker.chars().count()));
    markers
        .iter()
        .map(|marker| marker_pattern(marker))
        .collect::<Vec<_>>()
        .join("|")
}

/// One price found in a single run of text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PriceMatch {
    /// The amount as a number.
    pub value: f64,
    /// ISO code, or `None` for a marker that names no single currency.
    pub currency: Option<String>,
    /// The matched text, whitespace-trimmed.
    pub raw: String,
}

fn price_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alt = marker_alternation();
        let separators = GROUP_SEPARATORS;
        let amount = format!(r"\d{{1,3}}(?:[{separators}]\d{{1,3}})*|\d+");
        // The marker binds to the amount on one side or the other, and the
        // pair must be adjacent: allowing anything between them is what lets a
        // match span two prices.
        let pattern = format!(
            r"(?P<pre>{alt})\s?(?P<pre_amount>{amount})|(?P<post_amount>{amount})\s?(?P<post>{alt})"
        );
        fancy_regex::RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .expect("static price pattern")
    })
}

fn resolve_currency(marker: &str) -> Option<String> {
    let folded = marker.to_lowercase();
    CURRENCY_MARKERS
        .iter()
        .find(|(candidate, _)| candidate.to_lowercase() == folded)
        .and_then(|(_, code)| code.map(str::to_owned))
}

fn is_digit_run(run: &str) -> bool {
    !run.is_empty() && run.chars().all(|c| c.is_ascii_digit())
}

fn to_number(literal: &str) -> Option<f64> {
    literal.parse::<f64>().ok()
}

/// Turn a written amount into a number, or `None` when it is ambiguous.
///
/// Separators are read as a group: the last one decides whether it introduced
/// a decimal fraction or another thousand, and every earlier group has to be
/// exactly three digits or the string is not a number anyone wrote on purpose.
pub fn parse_amount(raw: &str) -> Option<f64> {
    let runs: Vec<&str> = raw.split(|c: char| GROUP_SEPARATORS.contains(c)).collect();
    if runs.iter().any(|run| !is_digit_run(run)) {
        return None;
    }
    if runs.len() == 1 {
        return to_number(runs[0]);
    }
    if runs[0].chars().count() > 3 {
        return None;
    }

    let tail = runs[runs.len() - 1];
    let raw_chars: Vec<char> = raw.chars().collect();
    let last_separator = raw_chars[raw_chars.len() - tail.chars().count() - 1];
    // A decimal fraction is one or two digits after a dot or a comma. Three
    // digits is a thousands group, whichever character precedes it, which is
    // what makes both 19,900 and 19.900 nineteen thousand nine hundred.
    let is_decimal = (last_separator == '.' || last_separator == ',') && tail.chars().count() <= 2;
    let groups: Vec<&str> = if is_decimal {
        runs[..runs.len() - 1].to_vec()
    } else {
        runs.clone()
    };
    if groups.iter().skip(1).any(|run| run.chars().count() != 3) {
        return None;
    }
    let whole: String = groups.concat();
    if is_decimal {
        to_number(&format!("{whole}.{tail}"))
    } else {
        to_number(&whole)
    }
}

/// Find one price in a single run of text.
///
/// `text` must be one text node. Joined page text is what produced values
/// stitched from two neighbouring prices, so the caller keeps them apart.
pub fn parse_price(text: &str) -> Option<PriceMatch> {
    let re = price_re();
    for attempt in re.captures_iter(text) {
        let found = match attempt {
            Ok(found) => found,
            Err(_) => continue,
        };
        let group_str = |name: &str| found.name(name).map(|m| m.as_str());
        let marker = group_str("pre").or_else(|| group_str("post")).unwrap_or("");
        let amount = group_str("pre_amount")
            .or_else(|| group_str("post_amount"))
            .unwrap_or("");
        let Some(value) = parse_amount(amount) else {
            continue;
        };
        let whole = found.get(0).map(|m| m.as_str()).unwrap_or("");
        return Some(PriceMatch {
            value,
            currency: resolve_currency(marker),
            raw: py_strip(whole).to_owned(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // from tests/test_price_detection.py::test_the_amount_and_currency_are_read_correctly
    #[test]
    fn the_amount_and_currency_are_read_correctly() {
        // The table from the issue, plus the separators that arrive with
        // copied text (no-break and narrow no-break spaces included).
        let written: Vec<(&str, f64, &str)> = vec![
            ("Стол 19 900 ₽", 19900.0, "RUB"),
            ("Стол 19900 ₽", 19900.0, "RUB"),
            ("Стол 19 900 руб.", 19900.0, "RUB"),
            ("Кухня 1 500 000 ₽", 1_500_000.0, "RUB"),
            ("Table 19 900 €", 19900.0, "EUR"),
            ("Table $19,900", 19900.0, "USD"),
            ("Стол 19\u{00a0}900 ₽", 19900.0, "RUB"),
            ("Стол 19\u{202f}900 ₽", 19900.0, "RUB"),
            ("Cena 1 200 zł", 1200.0, "PLN"),
            ("Cena 500 Kč", 500.0, "CZK"),
            ("12'500 CHF", 12500.0, "CHF"),
            ("1.234.567 ₽", 1_234_567.0, "RUB"),
        ];
        for (text, value, currency) in written {
            let found = parse_price(text).unwrap_or_else(|| panic!("no price found in {text:?}"));
            assert_eq!(found.value, value, "{text:?}");
            assert_eq!(found.currency.as_deref(), Some(currency), "{text:?}");
        }
    }

    // from tests/test_price_detection.py::test_us_format_is_unchanged
    #[test]
    fn us_format_is_unchanged() {
        for (text, value) in [
            ("$19.99", 19.99),
            ("USD 19.99", 19.99),
            ("$1,299.50", 1299.5),
        ] {
            assert_eq!(parse_price(text).expect("price").value, value, "{text:?}");
        }
    }

    // from tests/test_price_detection.py::test_a_marker_that_names_no_single_currency_is_still_money
    #[test]
    fn a_marker_that_names_no_single_currency_is_still_money() {
        // "kr" is Swedish, Norwegian, Danish or Icelandic: an amount, no claim.
        let found = parse_price("99 kr").expect("price");
        assert_eq!(found.value, 99.0);
        assert_eq!(found.currency, None);
    }

    // from tests/test_price_detection.py::test_text_without_a_currency_is_not_a_price
    #[test]
    fn text_without_a_currency_is_not_a_price() {
        assert_eq!(parse_price("Артикул 19900"), None);
        assert_eq!(parse_price("Цена по запросу"), None);
    }

    // from tests/test_price_detection.py::test_an_amount_nobody_wrote_on_purpose_is_rejected
    #[test]
    fn an_amount_nobody_wrote_on_purpose_is_rejected() {
        for raw in ["1234,5678", "12,34,56", "1234 5678"] {
            assert_eq!(parse_amount(raw), None, "{raw:?}");
        }
    }

    // from tests/test_price_detection.py::test_a_qualified_symbol_resolves_to_its_own_currency_not_the_bare_symbols
    #[test]
    fn a_qualified_symbol_resolves_to_its_own_currency_not_the_bare_symbols() {
        // Before the guard, "CA$"/"AU$"/"HK$" matched on their inner bare "$"
        // and came back USD, and "CN¥" matched on its inner "¥" and came back
        // JPY.
        for (text, value, currency) in [
            ("CA$ 1,299.50", 1299.5, "CAD"),
            ("AU$ 1,299.50", 1299.5, "AUD"),
            ("HK$ 1,299.50", 1299.5, "HKD"),
            ("CN¥ 1,299.50", 1299.5, "CNY"),
        ] {
            let found = parse_price(text).unwrap_or_else(|| panic!("no price found in {text:?}"));
            assert_eq!(found.value, value, "{text:?}");
            assert_eq!(found.currency.as_deref(), Some(currency), "{text:?}");
        }
    }

    // from tests/test_price_detection.py::test_an_unrecognised_qualified_symbol_is_no_price_not_a_guessed_one
    #[test]
    fn an_unrecognised_qualified_symbol_is_no_price_not_a_guessed_one() {
        // "SG$" names no currency this table resolves unambiguously; guessing
        // USD from the inner bare "$" would be a wrong finding, which is worse
        // than none.
        assert_eq!(parse_price("SG$100"), None);
    }

    // from tests/test_price_detection.py::test_bare_dollar_and_yen_are_unaffected_by_the_qualifier_guard
    #[test]
    fn bare_dollar_and_yen_are_unaffected_by_the_qualifier_guard() {
        assert_eq!(
            parse_price("$19.99").expect("price").currency.as_deref(),
            Some("USD")
        );
        assert_eq!(
            parse_price("¥100").expect("price").currency.as_deref(),
            Some("JPY")
        );
        assert_eq!(
            parse_price("R$ 100").expect("price").currency.as_deref(),
            Some("BRL")
        );
    }
}
