use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, NaiveDateTime, Timelike};

#[cfg(target_os = "windows")]
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Win32SystemTime {
    w_year: u16,
    w_month: u16,
    w_day_of_week: u16,
    w_day: u16,
    w_hour: u16,
    w_minute: u16,
    w_second: u16,
    w_milliseconds: u16,
}

#[cfg(target_os = "windows")]
const LOCALE_USER_DEFAULT: u32 = 0x0400;
#[cfg(target_os = "windows")]
const DATE_SHORTDATE: u32 = 0x0000_0001;

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn GetDateFormatW(
        locale: u32,
        dw_flags: u32,
        lp_date: *const Win32SystemTime,
        lp_format: *const u16,
        lp_date_str: *mut u16,
        cch_date: i32,
    ) -> i32;

    fn GetTimeFormatW(
        locale: u32,
        dw_flags: u32,
        lp_time: *const Win32SystemTime,
        lp_format: *const u16,
        lp_time_str: *mut u16,
        cch_time: i32,
    ) -> i32;
}

#[cfg(target_os = "windows")]
fn datetime_to_win32_systemtime(dt: &DateTime<Local>) -> Win32SystemTime {
    Win32SystemTime {
        w_year: dt.year() as u16,
        w_month: dt.month() as u16,
        w_day_of_week: dt.weekday().num_days_from_sunday() as u16,
        w_day: dt.day() as u16,
        w_hour: dt.hour() as u16,
        w_minute: dt.minute() as u16,
        w_second: dt.second() as u16,
        w_milliseconds: (dt.nanosecond() / 1_000_000).min(999) as u16,
    }
}

/// Formats the date according to Windows preferences (short date) or local standard.
pub fn format_local_date(dt: &DateTime<Local>) -> String {
    #[cfg(target_os = "windows")]
    {
        let st = datetime_to_win32_systemtime(dt);
        let mut buf = [0u16; 64];
        let len = unsafe {
            GetDateFormatW(
                LOCALE_USER_DEFAULT,
                DATE_SHORTDATE,
                &st,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len() as i32,
            )
        };
        if len > 1 {
            return String::from_utf16_lossy(&buf[..len as usize - 1]);
        }
    }
    dt.format("%d/%m/%Y").to_string()
}

/// Formats the time according to Windows preferences (HH:mm:ss) or local standard.
pub fn format_local_time(dt: &DateTime<Local>) -> String {
    #[cfg(target_os = "windows")]
    {
        let st = datetime_to_win32_systemtime(dt);
        let mut buf = [0u16; 64];
        let len = unsafe {
            GetTimeFormatW(
                LOCALE_USER_DEFAULT,
                0,
                &st,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len() as i32,
            )
        };
        if len > 1 {
            return String::from_utf16_lossy(&buf[..len as usize - 1]);
        }
    }
    dt.format("%H:%M:%S").to_string()
}

/// Formats the full date and time synchronized with Windows.
pub fn format_local_datetime(dt: &DateTime<Local>) -> String {
    let date = format_local_date(dt);
    let time = format_local_time(dt);
    format!("{date} {time}")
}

/// Converts a `SystemTime` to local timezone and formats in Windows style.
pub fn format_system_time(time: SystemTime) -> String {
    let dt = DateTime::<Local>::from(time);
    format_local_datetime(&dt)
}

/// Returns only the formatted local time for a `SystemTime`.
pub fn format_system_time_only(time: SystemTime) -> String {
    let dt = DateTime::<Local>::from(time);
    format_local_time(&dt)
}

/// Converts an RFC-3339 / ISO string to local system datetime.
/// If parsing fails, returns the fallback original string.
pub fn parse_to_local(iso_or_rfc3339: &str) -> Option<DateTime<Local>> {
    let trimmed = iso_or_rfc3339.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&Local));
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
        return dt.and_local_timezone(Local).earliest();
    }
    None
}

/// Formats an RFC-3339 / ISO timestamp in local date and time synchronized with Windows.
pub fn format_timestamp(iso_or_rfc3339: &str) -> String {
    match parse_to_local(iso_or_rfc3339) {
        Some(dt) => format_local_datetime(&dt),
        None => iso_or_rfc3339.to_string(),
    }
}

/// Formats only the local time synchronized with Windows for an RFC-3339 timestamp.
pub fn format_timestamp_time_only(iso_or_rfc3339: &str) -> String {
    match parse_to_local(iso_or_rfc3339) {
        Some(dt) => format_local_time(&dt),
        None => iso_or_rfc3339.to_string(),
    }
}

/// Returns current system time formatted in Windows time (`HH:mm:ss`).
pub fn current_time_only() -> String {
    format_local_time(&Local::now())
}

/// Returns current system date and time formatted in Windows standard.
pub fn current_datetime() -> String {
    format_local_datetime(&Local::now())
}

/// Formats a time range (start and finish) in a readable, synchronized manner.
pub fn format_time_range(start: Option<&str>, end: Option<&str>, fallback: &str) -> String {
    match (start, end) {
        (Some(start_raw), Some(end_raw)) => {
            match (parse_to_local(start_raw), parse_to_local(end_raw)) {
                (Some(s_dt), Some(e_dt)) => {
                    let s_date = format_local_date(&s_dt);
                    let e_date = format_local_date(&e_dt);
                    if s_date == e_date {
                        format!(
                            "{} {} → {}",
                            s_date,
                            format_local_time(&s_dt),
                            format_local_time(&e_dt)
                        )
                    } else {
                        format!(
                            "{} → {}",
                            format_local_datetime(&s_dt),
                            format_local_datetime(&e_dt)
                        )
                    }
                }
                (Some(s_dt), None) => {
                    format!("{} → {}", format_local_datetime(&s_dt), end_raw)
                }
                (None, Some(e_dt)) => {
                    format!("{} → {}", start_raw, format_local_datetime(&e_dt))
                }
                (None, None) => format!("{start_raw} → {end_raw}"),
            }
        }
        (Some(start_raw), None) => match parse_to_local(start_raw) {
            Some(s_dt) => format!("since {}", format_local_datetime(&s_dt)),
            None => format!("since {start_raw}"),
        },
        (None, Some(end_raw)) => match parse_to_local(end_raw) {
            Some(e_dt) => format!("finished at {}", format_local_datetime(&e_dt)),
            None => format!("finished at {end_raw}"),
        },
        (None, None) => fallback.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_system_time_produces_non_empty_localized_string() {
        let now = SystemTime::now();
        let formatted = format_system_time(now);
        assert!(!formatted.is_empty());
        let time_only = format_system_time_only(now);
        assert!(!time_only.is_empty());
        assert!(time_only.contains(':'));
    }

    #[test]
    fn format_timestamp_parses_rfc3339_utc_correctly() {
        let rfc3339 = "2026-09-09T13:05:22Z";
        let formatted = format_timestamp(rfc3339);
        assert!(!formatted.is_empty());
        assert!(!formatted.contains('Z'));
        assert!(!formatted.contains("UTC"));

        let time_only = format_timestamp_time_only(rfc3339);
        assert!(!time_only.is_empty());
        assert!(time_only.contains(':'));
    }

    #[test]
    fn format_timestamp_gracefully_handles_invalid_or_mock_strings() {
        assert_eq!(format_timestamp("now"), "now");
        assert_eq!(format_timestamp(""), "");
        assert_eq!(format_timestamp_time_only("invalid"), "invalid");
    }

    #[test]
    fn format_time_range_formats_same_day_and_open_ended() {
        let start = "2026-09-09T10:00:00Z";
        let end = "2026-09-09T10:05:30Z";
        let range = format_time_range(Some(start), Some(end), "—");
        assert!(range.contains("→"));

        let ongoing = format_time_range(Some(start), None, "—");
        assert!(ongoing.starts_with("since "));

        let empty = format_time_range(None, None, "fallback");
        assert_eq!(empty, "fallback");
    }

    #[test]
    fn current_time_helpers_produce_valid_outputs() {
        let current_t = current_time_only();
        assert!(!current_t.is_empty());
        assert!(current_t.contains(':'));

        let current_dt = current_datetime();
        assert!(!current_dt.is_empty());
    }
}
