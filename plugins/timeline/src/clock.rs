//! Local-time helpers (the reference's `datetime.combine(day, 0).astimezone()` and `strftime`),
//! on libc so the timeline needs no chrono. Day boundaries are LOCAL midnights, as in the
//! artifact-browser's `timeline.py`.

use std::ffi::CStr;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

fn local_tm(timestamp: f64) -> libc::tm {
    let seconds = timestamp.floor() as libc::time_t;
    // SAFETY: `tm` is plain data; localtime_r fills it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call.
    unsafe { libc::localtime_r(&seconds, &mut tm) };
    tm
}

/// Local midnight `days` days after the day containing `timestamp`, as epoch seconds.
pub fn local_midnight(timestamp: f64, days: i32) -> f64 {
    let mut tm = local_tm(timestamp);
    tm.tm_hour = 0;
    tm.tm_min = 0;
    tm.tm_sec = 0;
    tm.tm_mday += days;
    tm.tm_isdst = -1;
    // SAFETY: `tm` is a valid, normalisable tm.
    let seconds = unsafe { libc::mktime(&mut tm) };
    seconds as f64
}

/// `strftime(pattern)` of `timestamp` in local time.
pub fn format(timestamp: f64, pattern: &CStr) -> String {
    let tm = local_tm(timestamp);
    let mut buffer = [0u8; 64];
    // SAFETY: the buffer is 64 bytes and its length is passed; the pattern is NUL-terminated.
    let written = unsafe {
        libc::strftime(
            buffer.as_mut_ptr().cast::<libc::c_char>(),
            buffer.len(),
            pattern.as_ptr(),
            &tm,
        )
    };
    String::from_utf8_lossy(&buffer[..written]).into_owned()
}

/// Saturday or Sunday in local time.
pub fn is_weekend(timestamp: f64) -> bool {
    matches!(local_tm(timestamp).tm_wday, 0 | 6)
}

/// `%a %b %d` — the day-column label.
pub fn day_label(timestamp: f64) -> String {
    format(timestamp, c"%a %b %d")
}

/// `%b%d` — the `@MonDD` suffix of a same-name dead lane.
pub fn month_day(timestamp: f64) -> String {
    format(timestamp, c"%b%d")
}

/// `%a %H:%M` — the time cell of a dialogue row / the footer.
pub fn short_time(timestamp: f64) -> String {
    format(timestamp, c"%a %H:%M")
}

/// `%H:%M` — an hour tick.
pub fn clock_label(timestamp: f64) -> String {
    format(timestamp, c"%H:%M")
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn civil_days(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn number(text: &str, from: usize, length: usize) -> Option<i64> {
    let slice = text.get(from..from + length)?;
    slice
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| slice.parse().ok())?
}

/// Local-time epoch seconds of a naive wall-clock time.
fn local_epoch(year: i64, month: i64, day: i64, seconds_of_day: i64) -> Option<f64> {
    // SAFETY: `tm` is plain data; mktime normalises it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = i32::try_from(year - 1900).ok()?;
    tm.tm_mon = i32::try_from(month - 1).ok()?;
    tm.tm_mday = i32::try_from(day).ok()?;
    tm.tm_sec = i32::try_from(seconds_of_day).ok()?;
    tm.tm_isdst = -1;
    // SAFETY: `tm` is a valid, normalisable tm.
    let seconds = unsafe { libc::mktime(&mut tm) };
    Some(seconds as f64)
}

/// Epoch seconds of a transcript timestamp: `YYYY-MM-DD[T| ]HH:MM[:SS[.f…]]` with `Z` or
/// `±HH[:]MM`; a naive value is local time (`datetime.fromisoformat(...).timestamp()` for the
/// shapes engines write).
pub fn parse_iso(text: &str) -> Option<f64> {
    let year = number(text, 0, 4)?;
    let month = number(text, 5, 2)?;
    let day = number(text, 8, 2)?;
    if text.get(4..5)? != "-" || text.get(7..8)? != "-" || !(1..=12).contains(&month) {
        return None;
    }
    if !(1..=31).contains(&day) {
        return None;
    }
    let rest = &text[10..];
    if rest.is_empty() {
        return local_epoch(year, month, day, 0);
    }
    let rest = rest.get(1..)?;
    let hour = number(rest, 0, 2)?;
    let minute = number(rest, 3, 2)?;
    if rest.get(2..3)? != ":" || hour > 24 || minute > 59 {
        return None;
    }
    let mut at = 5;
    let mut second = 0;
    let mut fraction = 0.0;
    if rest.get(at..at + 1) == Some(":") {
        second = number(rest, at + 1, 2)?;
        at += 3;
        if matches!(rest.get(at..at + 1), Some("." | ",")) {
            let digits = rest[at + 1..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
            if digits == 0 {
                return None;
            }
            fraction = format!("0.{}", &rest[at + 1..at + 1 + digits])
                .parse()
                .ok()?;
            at += 1 + digits;
        }
    }
    let seconds_of_day = hour * 3600 + minute * 60 + second;
    let zone = &rest[at..];
    let offset = match zone {
        "" => return local_epoch(year, month, day, seconds_of_day).map(|at| at + fraction),
        "Z" | "z" => 0,
        _ => {
            let sign = match zone.get(..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            let zone = zone[1..].replace(':', "");
            if zone.len() != 4 {
                return None;
            }
            sign * (number(&zone, 0, 2)? * 3600 + number(&zone, 2, 2)? * 60)
        }
    };
    let days = civil_days(year, month, day);
    Some((days * 86_400 + seconds_of_day - offset) as f64 + fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_timestamps() {
        assert_eq!(parse_iso("2026-10-11T12:00:00Z"), Some(1_791_720_000.0));
        assert_eq!(
            parse_iso("2026-10-11T12:00:00.250Z"),
            Some(1_791_720_000.25)
        );
        assert_eq!(
            parse_iso("2026-10-11T14:00:00+02:00"),
            Some(1_791_720_000.0)
        );
        assert_eq!(parse_iso("2026-10-11T07:00:00-0500"), Some(1_791_720_000.0));
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0.0));
        assert!(parse_iso("2026-10-11T12:00:00").is_some());
        for bad in [
            "",
            "soon",
            "2026-13-01T00:00:00Z",
            "2026-10-11T12:00:00+2",
            "2026-10-11T12",
        ] {
            assert_eq!(parse_iso(bad), None, "{bad:?}");
        }
    }
}
