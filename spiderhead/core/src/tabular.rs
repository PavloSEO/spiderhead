//! Formula-safe text for CSV and XLSX cells, shared by every tabular writer.
//!
//! A string that starts with a formula lead character gets one leading apostrophe, so
//! spreadsheet importers read it as text.

/// Characters that spreadsheet importers treat as the start of a formula.
const FORMULA_LEADS: &[char] = &['=', '+', '-', '@', '\t', '\r'];

/// Defuse CSV/XLSX formula injection (CWE-1236) in one cell string.
pub fn neutralize_formula(value: &str) -> String {
    if value.starts_with(FORMULA_LEADS) {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_leads_are_prefixed_with_apostrophe() {
        for lead in ["=", "+", "-", "@", "\t", "\r"] {
            assert_eq!(
                neutralize_formula(&format!("{lead}SUM(A1)")),
                format!("'{lead}SUM(A1)")
            );
        }
    }

    #[test]
    fn safe_strings_pass_through_unchanged() {
        for value in ["plain", "", "a=b", " =leading space"] {
            assert_eq!(neutralize_formula(value), value);
        }
    }
}
