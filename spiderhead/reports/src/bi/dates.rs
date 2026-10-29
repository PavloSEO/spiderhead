//! Minimal date handling for BI period windows: `YYYY-MM-DD` dates and one-day steps.
//!
//! Strict `YYYY-MM-DD` parsing with real calendar validation; zero-padded components only.

/// One ISO calendar date, comparable and offsettable by days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct IsoDate {
    /// Days since a fixed epoch (proleptic Gregorian serial).
    pub serial: i64,
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days from civil epoch (Howard Hinnant's algorithm).
fn serial_from_ymd(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

impl IsoDate {
    /// Parse strict `YYYY-MM-DD` with calendar validation.
    pub fn parse(value: &str) -> Option<IsoDate> {
        let bytes = value.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let digit = |b: u8| b.is_ascii_digit();
        if !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || digit(*b))
        {
            return None;
        }
        let year: i64 = value[0..4].parse().ok()?;
        let month: u32 = value[5..7].parse().ok()?;
        let day: u32 = value[8..10].parse().ok()?;
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(IsoDate {
            serial: serial_from_ymd(year, month, day),
        })
    }

    /// Next calendar day.
    pub fn next_day(self) -> IsoDate {
        IsoDate {
            serial: self.serial + 1,
        }
    }
}

/// Period window `(start, end)` when both parse and start <= end.
pub fn window(value: Option<&serde_json::Value>) -> Option<(IsoDate, IsoDate)> {
    let period = value?.as_object()?;
    let start = IsoDate::parse(period.get("start_date")?.as_str()?)?;
    let end = IsoDate::parse(period.get("end_date")?.as_str()?)?;
    (start <= end).then_some((start, end))
}
