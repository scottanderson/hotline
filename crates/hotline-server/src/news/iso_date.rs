//! Convert [`HlDate`] to and from an ISO 8601 string, e.g.
//! `"2024-01-15T12:34:56.789Z"`. Plug the read side in with
//! `#[serde(with = "iso_date")]`. [`to_string`] is the plain,
//! `serde`-free form [`deserialize`] builds on. Call `to_string`
//! directly on the write side.

use hotline_proto::types::{civil_from_days, days_from_civil, HlDate};
use serde::{Deserialize, Deserializer};

/// Format `date` as an ISO 8601 string.
pub(crate) fn to_string(date: &HlDate) -> String {
    let days = date.seconds / 86400;
    let secs_of_day = date.seconds % 86400;
    let (_, month, day) =
        civil_from_days(days_from_civil(date.year as i64, 1, 1).saturating_add(days as i64));
    let (hour, minute, second) = (secs_of_day / 3600, secs_of_day / 60 % 60, secs_of_day % 60);
    format!(
        "{:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        date.year, date.milliseconds,
    )
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<HlDate, D::Error> {
    let s = String::deserialize(deserializer)?;
    let err = || serde::de::Error::custom(format!("expected an ISO 8601 date-time, got {s:?}"));

    let (date_part, rest) = s.split_once('T').ok_or_else(err)?;
    let time_part = rest.strip_suffix('Z').ok_or_else(err)?;
    let mut date_fields = date_part.splitn(3, '-');
    let year: u16 = date_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let month: u32 = date_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let day: u32 = date_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;

    let (hms_part, millis_part) = time_part.split_once('.').ok_or_else(err)?;
    let mut hms_fields = hms_part.splitn(3, ':');
    let hour: u32 = hms_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let minute: u32 = hms_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let second: u32 = hms_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let milliseconds: u16 = millis_part.parse().map_err(|_| err())?;

    let days_since_jan1 =
        days_from_civil(year as i64, month, day).saturating_sub(days_from_civil(year as i64, 1, 1));
    let seconds = u32::try_from(days_since_jan1)
        .ok()
        .and_then(|days| days.checked_mul(86400))
        .and_then(|seconds| seconds.checked_add(hour.checked_mul(3600)?))
        .and_then(|seconds| seconds.checked_add(minute.checked_mul(60)?))
        .and_then(|seconds| seconds.checked_add(second))
        .ok_or_else(err)?;
    Ok(HlDate {
        year,
        milliseconds,
        seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::{to_string, HlDate};

    #[derive(serde::Deserialize)]
    struct Wrapper(#[serde(with = "super")] HlDate);

    #[test]
    fn date_round_trips_through_an_iso_8601_string() {
        // 2024-03-05 13:14:15, day 64 (0-indexed) into the leap year.
        let date = HlDate {
            year: 2024,
            milliseconds: 789,
            seconds: 64 * 86400 + 47655,
        };

        let text = to_string(&date);

        assert_eq!(text, "2024-03-05T13:14:15.789Z");
        let json = serde_json::Value::String(text);
        let Wrapper(round_tripped) = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, date);
    }

    #[test]
    fn date_with_too_many_seconds_is_an_error() {
        let json = serde_json::Value::String("2024-03-05T99999999:00:00.000Z".into());
        assert!(serde_json::from_value::<Wrapper>(json).is_err());
    }
}
