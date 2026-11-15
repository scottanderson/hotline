//! [`HlDate`]: a Hotline date field.

use std::fmt;
#[cfg(feature = "chrono")]
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::wire::{ArrayReader, ArrayWriter};

/// A Hotline date: `year(2) | milliseconds(2) | seconds-since-midnight(4)`.
/// Used by [`myField_FileCreateDate`] (208), [`myField_FileModifyDate`] (209),
/// [`myField_NewsArtDate`] (330), and the
/// old-guide `datetime` object (base year 1904, Mac HFS style).
///
/// [`myField_FileCreateDate`]: crate::FieldId::FileCreateDate
/// [`myField_FileModifyDate`]: crate::FieldId::FileModifyDate
/// [`myField_NewsArtDate`]: crate::FieldId::NewsArtDate
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HlDate {
    /// Calendar year, e.g. `1998`.
    pub year: u16,
    /// Milliseconds component (rarely nonzero in practice).
    pub milliseconds: u16,
    /// Seconds since midnight Jan 1 of `year`.
    pub seconds: u32,
}

/// Days since 1970-01-01 for a proleptic Gregorian `(year, month, day)`.
/// Howard Hinnant's `days_from_civil`, a standard, well known algorithm.
/// Each step stays inside `i64` for a year within ±10¹².
#[expect(
    clippy::arithmetic_side_effects,
    reason = "each caller passes a year far inside the safe range"
)]
pub const fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: proleptic Gregorian `(year, month,
/// day)` for days since 1970-01-01. Each step stays inside `i64` for a
/// year within ±10¹².
#[expect(
    clippy::arithmetic_side_effects,
    reason = "each caller passes days far inside the safe range"
)]
pub const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

impl From<SystemTime> for HlDate {
    /// The UTC date and time of `time`. A time before 1970 gives 1970.
    fn from(time: SystemTime) -> Self {
        let since_epoch = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = since_epoch.as_secs();
        let days = (secs / 86400) as i64;
        let (year, _month, _day) = civil_from_days(days);
        let days_since_jan1 = days.saturating_sub(days_from_civil(year, 1, 1));
        Self {
            year: year as u16,
            milliseconds: since_epoch.subsec_millis() as u16,
            seconds: (days_since_jan1 as u32)
                .saturating_mul(86400)
                .saturating_add((secs % 86400) as u32),
        }
    }
}

impl HlDate {
    /// Wire length in bytes: `year(2) | milliseconds(2) | seconds(4)`.
    pub const LEN: usize = 8;

    /// Encode as the 8-byte wire representation.
    pub fn encode(&self) -> [u8; Self::LEN] {
        ArrayWriter::new()
            .u16(self.year)
            .u16(self.milliseconds)
            .u32(self.seconds)
            .finish()
    }

    /// Decode from the 8-byte wire representation.
    pub const fn decode(buf: [u8; Self::LEN]) -> Self {
        let mut r = ArrayReader::new(buf);
        let date = Self {
            year: r.u16(),
            milliseconds: r.u16(),
            seconds: r.u32(),
        };
        r.finish();
        date
    }
}

/// The locale of [`HlDate`]'s `Display`, from [`HlDate::set_locale`].
#[cfg(feature = "chrono")]
static LOCALE: OnceLock<chrono::Locale> = OnceLock::new();

#[cfg(feature = "chrono")]
impl HlDate {
    /// Show each date in `locale`. Only the first call has an effect.
    pub fn set_locale(locale: chrono::Locale) {
        let _ = LOCALE.set(locale);
    }

    /// The date and time, in no time zone. Out of range gives the Unix
    /// epoch.
    pub fn to_naive(&self) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(self.year.into(), 1, 1)
            .and_then(|jan1| jan1.and_hms_opt(0, 0, 0))
            .and_then(|start| {
                start.checked_add_signed(chrono::TimeDelta::seconds(self.seconds.into()))
            })
            .unwrap_or_default()
    }

    /// The date and time as `locale` writes them, with `%x %X`.
    pub fn localized(&self, locale: chrono::Locale) -> impl fmt::Display + use<> {
        self.to_naive().and_utc().format_localized("%x %X", locale)
    }
}

/// The date and time as the server sent them. With the `chrono` feature,
/// in the locale of [`HlDate::set_locale`], else as `YYYY-MM-DD HH:MM:SS`.
impl fmt::Display for HlDate {
    #[cfg(feature = "chrono")]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let locale = LOCALE.get().copied().unwrap_or(chrono::Locale::POSIX);
        write!(f, "{}", self.localized(locale))
    }

    #[cfg(not(feature = "chrono"))]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days =
            days_from_civil(self.year.into(), 1, 1).saturating_add(i64::from(self.seconds / 86400));
        let (year, month, day) = civil_from_days(days);
        let time = self.seconds % 86400;
        let (hour, minute, second) = (time / 3600, time / 60 % 60, time % 60);
        write!(
            f,
            "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAP_DAY: HlDate = HlDate {
        year: 2024,
        milliseconds: 0,
        seconds: 59 * 86400 + 13 * 3600 + 5 * 60 + 7,
    };

    #[cfg(not(feature = "chrono"))]
    #[test]
    fn display_writes_the_date_and_time() {
        assert_eq!(LEAP_DAY.to_string(), "2024-02-29 13:05:07");
    }

    #[cfg(feature = "chrono")]
    #[test]
    fn to_naive_adds_the_seconds_to_the_start_of_the_year() {
        assert_eq!(LEAP_DAY.to_naive().to_string(), "2024-02-29 13:05:07");
    }

    #[cfg(feature = "chrono")]
    #[test]
    fn localized_follows_the_locale() {
        let us = LEAP_DAY.localized(chrono::Locale::en_US).to_string();
        assert_eq!(us, "02/29/2024 01:05:07 PM");
        let de = LEAP_DAY.localized(chrono::Locale::de_DE).to_string();
        assert_eq!(de, "29.02.2024 13:05:07");
    }

    #[test]
    fn days_from_civil_round_trips_through_civil_from_days() {
        for days in [0, 1, 365, 366, 10957, 19723, -1, -365] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "for {days} days");
        }
    }

    #[test]
    fn epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn from_system_time_counts_seconds_from_january_1() {
        // 2024-03-01 00:00:01.5 UTC, 60 days into the leap year.
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_709_251_201_500);
        assert_eq!(
            HlDate::from(time),
            HlDate {
                year: 2024,
                milliseconds: 500,
                seconds: 60 * 86400 + 1,
            }
        );
    }
}
