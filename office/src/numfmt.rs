//! How spreadsheet numbers are shown: Excel stores dates and times as
//! numbers (days since 1900) and only the cell's number format says so.

/// What a cell's number format means for showing it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Style {
    #[default]
    General,
    Date,
    Time,
    DateTime,
    /// A percentage with this many decimals.
    Percent(u8),
}

/// The style of number format `id`, whose format code is `code` for the
/// custom formats (ids 164 and up) and some built-in ones.
pub fn style(id: u32, code: Option<&str>) -> Style {
    match id {
        9 => Style::Percent(0),
        10 => Style::Percent(2),
        14..=17 | 27..=31 | 34..=36 | 50..=58 => Style::Date,
        18..=21 | 32 | 33 | 45..=47 => Style::Time,
        22 => Style::DateTime,
        _ => code.map_or(Style::General, analyze),
    }
}

/// Read a format code like `dd/mm/yyyy hh:mm`, `0.0%` or `#,##0 "kr"`.
fn analyze(code: &str) -> Style {
    if code.trim().get(..7).is_some_and(|g| g.eq_ignore_ascii_case("general")) {
        return Style::General;
    }
    let (mut date, mut time, mut percent, mut decimals, mut after_dot) = (false, false, false, 0u8, false);
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            // Only the first section is for positive numbers.
            ';' => break,
            // Literal text and escaped or padding characters.
            '"' => {
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            // [Red], [$-409], [>100]; but [h] and [mm] are elapsed time.
            '[' => {
                let inner: String = chars.by_ref().take_while(|&c| c != ']').collect();
                if !inner.is_empty() && inner.chars().all(|c| matches!(c.to_ascii_lowercase(), 'h' | 'm' | 's')) {
                    time = true;
                }
            }
            // (Not 'e': that is mostly scientific notation, 0.00E+00.)
            'y' | 'Y' | 'd' | 'D' => date = true,
            'h' | 'H' | 's' | 'S' => time = true,
            'm' | 'M' => date = true,
            'A' | 'a' if chars.clone().take(4).collect::<String>().eq_ignore_ascii_case("M/PM") => time = true,
            '%' => percent = true,
            '.' => after_dot = true,
            '0' | '#' | '?' if after_dot => decimals = decimals.saturating_add(1),
            _ => {}
        }
    }
    match (date, time) {
        // "mm" next to hours or seconds means minutes, not months.
        (true, true) if !code.to_ascii_lowercase().contains(['y', 'd']) => Style::Time,
        (true, true) => Style::DateTime,
        (true, false) => Style::Date,
        (false, true) => Style::Time,
        _ if percent => Style::Percent(decimals),
        _ => Style::General,
    }
}

/// Show a number in a style. `date1904`: the workbook counts days from
/// 1904 (old Mac Excel) instead of 1900.
pub fn show(v: f64, style: Style, date1904: bool) -> String {
    match style {
        Style::General => general(v),
        Style::Percent(d) => format!("{:.*}%", usize::from(d.min(10)), v * 100.0),
        Style::Date | Style::Time | Style::DateTime => match datetime(v, date1904) {
            Some((y, mo, d, h, mi, s)) => {
                let time = if s == 0 { format!("{h:02}:{mi:02}") } else { format!("{h:02}:{mi:02}:{s:02}") };
                match style {
                    Style::Date if (h, mi, s) == (0, 0, 0) => format!("{y:04}-{mo:02}-{d:02}"),
                    Style::Time => time,
                    _ => format!("{y:04}-{mo:02}-{d:02} {time}"),
                }
            }
            None => general(v),
        },
    }
}

/// Excel's "General": integers as they are, others with up to 10
/// significant digits, very large or small ones in exponent notation.
pub fn general(v: f64) -> String {
    if !v.is_finite() {
        return "#NUM!".into();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let a = v.abs();
    if !(1e-9..1e15).contains(&a) {
        let s = format!("{v:.5E}");
        // 1.50000E15 -> 1.5E15
        return match s.split_once('E') {
            Some((m, e)) if m.contains('.') => format!("{}E{e}", m.trim_end_matches('0').trim_end_matches('.')),
            _ => s,
        };
    }
    let digits = 10 - 1 - a.log10().floor() as i32;
    let s = format!("{v:.*}", digits.clamp(0, 15) as usize);
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// A day number (with the time as its fraction) as year, month, day, hour,
/// minute, second. None if out of Excel's range.
fn datetime(v: f64, date1904: bool) -> Option<(i64, u32, u32, u32, u32, u32)> {
    if !(0.0..2_958_466.0).contains(&v) {
        return None;
    }
    let mut days = v.floor() as i64;
    let mut secs = ((v - v.floor()) * 86_400.0).round() as i64;
    if secs >= 86_400 {
        days += 1;
        secs -= 86_400;
    }
    let (h, mi, s) = ((secs / 3600) as u32, (secs / 60 % 60) as u32, (secs % 60) as u32);
    if !date1904 && days == 60 {
        // Excel's 29 February 1900, which never was.
        return Some((1900, 2, 29, h, mi, s));
    }
    // Days since 1970-01-01. In the 1900 system day 1 is 1900-01-01, and
    // the made-up 29 February shifts the days after it by one.
    let unix = match (date1904, days) {
        (true, d) => d - 24_107,
        (false, d) if d < 60 => d - 25_568,
        (false, d) => d - 25_569,
    };
    let (y, mo, d) = civil(unix);
    Some((y, mo, d, h, mi, s))
}

/// Year, month, day of a day count since 1970-01-01 (proleptic Gregorian).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_recognised() {
        assert_eq!(style(14, None), Style::Date);
        assert_eq!(style(164, Some("dd/mm/yyyy")), Style::Date);
        assert_eq!(style(164, Some("[$-409]mmmm d, yyyy;@")), Style::Date);
        assert_eq!(style(164, Some("hh:mm:ss")), Style::Time);
        assert_eq!(style(164, Some("[h]:mm")), Style::Time);
        assert_eq!(style(164, Some("yyyy-mm-dd hh:mm")), Style::DateTime);
        assert_eq!(style(164, Some("0.0%")), Style::Percent(1));
        assert_eq!(style(164, Some("#,##0.00 \"days\"")), Style::General);
        assert_eq!(style(164, Some("[Red]0.00")), Style::General);
        assert_eq!(style(0, Some("General")), Style::General);
        assert_eq!(style(164, Some("0.00E+00")), Style::General);
    }

    #[test]
    fn dates_and_numbers_are_shown_like_excel() {
        assert_eq!(show(1.0, Style::Date, false), "1900-01-01");
        assert_eq!(show(59.0, Style::Date, false), "1900-02-28");
        assert_eq!(show(60.0, Style::Date, false), "1900-02-29");
        assert_eq!(show(61.0, Style::Date, false), "1900-03-01");
        assert_eq!(show(45_658.0, Style::Date, false), "2025-01-01");
        assert_eq!(show(45_658.75, Style::DateTime, false), "2025-01-01 18:00");
        assert_eq!(show(0.5 + 1.0 / 86_400.0, Style::Time, false), "12:00:01");
        assert_eq!(show(0.0, Style::Date, true), "1904-01-01");
        assert_eq!(show(0.256, Style::Percent(1), false), "25.6%");
        assert_eq!(general(42.0), "42");
        assert_eq!(general(0.1 + 0.2), "0.3");
        assert_eq!(general(-1234.5678), "-1234.5678");
        assert_eq!(general(1.5e20), "1.5E20");
        assert_eq!(general(f64::NAN), "#NUM!");
    }
}
