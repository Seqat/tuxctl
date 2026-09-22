//! Local wall-clock time via `localtime_r`, formatted without chrono.

/// A timestamp broken down in the system's local time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Seconds east of UTC, including daylight saving time.
    pub utc_offset_seconds: i32,
}

impl LocalTime {
    /// Converts Unix seconds to local time. Call this off the render path: the
    /// first conversion may read the time zone database from disk.
    pub fn from_unix_seconds(seconds: i64) -> Option<Self> {
        let seconds = libc::time_t::try_from(seconds).ok()?;
        // SAFETY: `libc::tm` is plain old data, so the all-zero value is valid.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers refer to live, properly aligned locals for the
        // duration of the call; localtime_r is the thread-safe variant.
        let result = unsafe { libc::localtime_r(&seconds, &mut tm) };
        if result.is_null() {
            return None;
        }
        Some(Self {
            year: tm.tm_year.checked_add(1900)?,
            month: u8::try_from(tm.tm_mon.checked_add(1)?).ok()?,
            day: u8::try_from(tm.tm_mday).ok()?,
            hour: u8::try_from(tm.tm_hour).ok()?,
            minute: u8::try_from(tm.tm_min).ok()?,
            second: u8::try_from(tm.tm_sec).ok()?,
            utc_offset_seconds: i32::try_from(tm.tm_gmtoff).ok()?,
        })
    }

    /// `HH:MM:SS`
    pub fn clock(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// `YYYY-MM-DD HH:MM:SS +HHMM`
    pub fn full(&self) -> String {
        let sign = if self.utc_offset_seconds < 0 {
            '-'
        } else {
            '+'
        };
        let offset_minutes = self.utc_offset_seconds.unsigned_abs() / 60;
        format!(
            "{:04}-{:02}-{:02} {} {sign}{:02}{:02}",
            self.year,
            self.month,
            self.day,
            self.clock(),
            offset_minutes / 60,
            offset_minutes % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u8, minute: u8, second: u8, utc_offset_seconds: i32) -> LocalTime {
        LocalTime {
            year: 2026,
            month: 9,
            day: 22,
            hour,
            minute,
            second,
            utc_offset_seconds,
        }
    }

    #[test]
    fn formats_clock_and_full_time_with_offsets() {
        assert_eq!(at(21, 4, 5, 3 * 3600).clock(), "21:04:05");
        assert_eq!(at(21, 4, 5, 3 * 3600).full(), "2026-09-22 21:04:05 +0300");
        assert_eq!(at(0, 0, 0, 0).full(), "2026-09-22 00:00:00 +0000");
        assert_eq!(
            at(9, 30, 0, -(3 * 3600 + 1800)).full(),
            "2026-09-22 09:30:00 -0330"
        );
        assert_eq!(
            at(23, 59, 59, 5 * 3600 + 2700).full(),
            "2026-09-22 23:59:59 +0545"
        );
    }

    #[test]
    fn conversion_is_consistent_with_its_own_utc_offset() {
        // Whatever the machine's time zone, local time minus the reported offset
        // must be the UTC time of the input. Unix time 86_400 + 3_723 is
        // 1970-01-02 01:02:03 UTC.
        let local = LocalTime::from_unix_seconds(86_400 + 3_723).unwrap();
        let local_seconds =
            i64::from(local.hour) * 3600 + i64::from(local.minute) * 60 + i64::from(local.second);
        let utc_seconds = (local_seconds - i64::from(local.utc_offset_seconds)).rem_euclid(86_400);

        assert_eq!(utc_seconds, 3_723);
    }
}
