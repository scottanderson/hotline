//! [`HlDate`]: a Hotline date field.

use std::time::SystemTime;

use crate::wire::{PacketReader, PacketWriter};

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
/// day)` for days since 1970-01-01.
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
        let days_since_jan1 = days - days_from_civil(year, 1, 1);
        Self {
            year: year as u16,
            milliseconds: since_epoch.subsec_millis() as u16,
            seconds: days_since_jan1 as u32 * 86400 + (secs % 86400) as u32,
        }
    }
}

impl HlDate {
    /// Wire length in bytes: `year(2) | milliseconds(2) | seconds(4)`.
    pub const LEN: usize = 8;

    /// Encode as the 8-byte wire representation.
    pub fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .u16(self.year)
            .u16(self.milliseconds)
            .u32(self.seconds)
            .finish()
    }

    /// Decode from the 8-byte wire representation.
    pub const fn decode(buf: [u8; Self::LEN]) -> Self {
        let mut r = PacketReader::new(buf);
        let date = Self {
            year: r.u16(),
            milliseconds: r.u16(),
            seconds: r.u32(),
        };
        r.finish();
        date
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
