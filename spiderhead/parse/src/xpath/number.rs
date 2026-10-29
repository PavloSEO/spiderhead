//! libxml2's number handling: string to number (`xmlXPathStringEvalNumber`) and number to
//! string (`xmlXPathFormatNumber`).

fn is_blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// libxml2's `xmlXPathStringEvalNumber`: the digits are accumulated in doubles and the fraction
/// and exponent applied with `pow`, which is not always the correctly rounded value.
pub(crate) fn string_to_number(text: &str) -> f64 {
    const MAX_FRAC: usize = 20;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() && is_blank(chars[i]) {
        i += 1;
    }
    let negative = chars.get(i) == Some(&'-');
    if negative {
        i += 1;
    }
    let digit = |i: usize| chars.get(i).is_some_and(|c| c.is_ascii_digit());
    if chars.get(i) != Some(&'.') && !digit(i) {
        return f64::NAN;
    }
    let mut ret = 0.0f64;
    let mut ok = false;
    while digit(i) {
        ret = ret * 10.0 + f64::from(chars[i].to_digit(10).unwrap_or(0));
        ok = true;
        i += 1;
    }
    if chars.get(i) == Some(&'.') {
        i += 1;
        if !digit(i) && !ok {
            return f64::NAN;
        }
        let mut frac = 0usize;
        let mut fraction = 0.0f64;
        while chars.get(i) == Some(&'0') {
            frac += 1;
            i += 1;
        }
        let max = frac + MAX_FRAC;
        while digit(i) && frac < max {
            fraction = fraction * 10.0 + f64::from(chars[i].to_digit(10).unwrap_or(0));
            frac += 1;
            i += 1;
        }
        fraction /= 10f64.powf(frac as f64);
        ret += fraction;
        while digit(i) {
            i += 1;
        }
    }
    let mut exponent: i64 = 0;
    let mut negative_exponent = false;
    if matches!(chars.get(i), Some('e' | 'E')) {
        i += 1;
        match chars.get(i) {
            Some('-') => {
                negative_exponent = true;
                i += 1;
            }
            Some('+') => i += 1,
            _ => {}
        }
        while digit(i) {
            exponent = (exponent * 10 + i64::from(chars[i].to_digit(10).unwrap_or(0))).min(100_000);
            i += 1;
        }
    }
    while i < chars.len() && is_blank(chars[i]) {
        i += 1;
    }
    if i != chars.len() {
        return f64::NAN;
    }
    if negative {
        ret = -ret;
    }
    if negative_exponent {
        exponent = -exponent;
    }
    ret * 10f64.powf(exponent as f64)
}

/// libxml2's `xmlXPathFormatNumber`.
pub(crate) fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if x == 0.0 {
        return "0".to_owned();
    }
    if x.abs() < 2_147_483_647.0 && x == x.trunc() {
        return (x as i32).to_string();
    }
    let a = x.abs();
    if a > 1e9 || a < 1e-5 {
        let formatted = format!("{x:.14e}");
        let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
        let mantissa = if mantissa.contains('.') {
            mantissa.trim_end_matches('0').trim_end_matches('.')
        } else {
            mantissa
        };
        let exp: i32 = exponent.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exp.abs());
    }
    let integer_place = a.log10() as i32;
    let fraction_place = if integer_place > 0 {
        14 - integer_place
    } else {
        15 - integer_place
    };
    let text = format!("{:.*}", fraction_place.max(0) as usize, x);
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_libxml2_arithmetic() {
        // from libxml2 xmlXPathStringEvalNumber: fraction / pow(10, digits), not strtod
        // literals with up to 15 significant digits print identically to lxml; longer ones may
        // differ by one ulp (libxml2's accumulation order is not reproduced bit for bit)
        assert_eq!(number_to_string(string_to_number("0.1")), "0.1");
        assert_eq!(
            number_to_string(string_to_number("2147483647")),
            "2.147483647e+09"
        );
        assert_eq!(
            number_to_string(string_to_number("1000000001")),
            "1000000001"
        );
        assert_eq!(number_to_string(1.0 / 3.0), "0.333333333333333");
        assert!(string_to_number("1e3") == 1000.0);
        assert!(string_to_number(" 12 ") == 12.0);
        assert!(string_to_number("+5").is_nan());
        assert!(string_to_number(".").is_nan());
        assert!(string_to_number("5.") == 5.0);
    }
}
