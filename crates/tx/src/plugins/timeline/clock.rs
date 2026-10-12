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
