//! Port of `utils/time.go` — `ToTime`, `ToFloat64Timestamp`, and the two
//! serde-visible time wrappers `CustomTime` and `NullTime`.

use chrono::{DateTime, TimeZone, Utc};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use super::errors::RagoError;
use super::result::Result;

const CUSTOM_TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// A "zero" `time.Time` sentinel. Go's zero value is year 1; using the chrono
/// minimum preserves the ordering semantics the code relies on (e.g. an event
/// with a zero `IngestedAt` is treated as very old).
fn zero_time() -> DateTime<Utc> {
    DateTime::<Utc>::MIN_UTC
}

fn from_unix_parts(seconds: i64, nanoseconds: i64) -> DateTime<Utc> {
    // time.Unix(sec, nsec).In(UTC).Truncate(millisecond)
    let total_nanos = seconds as i128 * 1_000_000_000 + nanoseconds as i128;
    let total_millis = total_nanos.div_euclid(1_000_000); // floor division == Truncate for ms
    Utc.timestamp_millis_opt(total_millis as i64)
        .single()
        .unwrap_or_else(zero_time)
}

/// `ToTime(timestamp any) Result[time.Time]`.
pub fn to_time(timestamp: &serde_json::Value) -> Result<DateTime<Utc>> {
    match timestamp {
        serde_json::Value::String(s) => {
            if let Ok(float_timestamp) = s.parse::<f64>() {
                let seconds = float_timestamp.trunc() as i64;
                let nanoseconds = ((float_timestamp - seconds as f64) * 1e9) as i64;
                Result::success(from_unix_parts(seconds, nanoseconds))
            } else {
                match DateTime::parse_from_rfc3339(s) {
                    Ok(t) => Result::success(t.with_timezone(&Utc)),
                    Err(_) => Result::failed(RagoError::Message(format!(
                        "strconv.ParseFloat: parsing {s:?}: invalid syntax"
                    ))),
                }
            }
        }
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(f) => {
                let seconds = f.trunc() as i64;
                let nanoseconds = ((f - seconds as f64) * 1e9) as i64;
                Result::success(from_unix_parts(seconds, nanoseconds))
            }
            None => Result::failed(RagoError::Message("Unsupported timestamp type".into())),
        },
        other => Result::failed(RagoError::Message(format!(
            "Unsupported timestamp type: {other}"
        ))),
    }
}

/// `ToFloat64Timestamp(timeValue any) Result[float64]`.
pub fn to_float64_timestamp(time_value: &serde_json::Value) -> Result<f64> {
    match time_value {
        serde_json::Value::String(s) => {
            if let Ok(float_timestamp) = s.parse::<f64>() {
                Result::success((float_timestamp * 1000.0).trunc() / 1000.0)
            } else {
                match DateTime::parse_from_rfc3339(s) {
                    Ok(t) => Result::success(t.timestamp_millis() as f64 / 1e3),
                    Err(_) => Result::failed(RagoError::Message(format!(
                        "strconv.ParseFloat: parsing {s:?}: invalid syntax"
                    ))),
                }
            }
        }
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(f) => Result::success(f),
            None => Result::failed(RagoError::Message("Unsupported timestamp type".into())),
        },
        other => Result::failed(RagoError::Message(format!(
            "Unsupported timestamp type: {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// CustomTime — marshals as "2006-01-02T15:04:05", zero value marshals as null.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CustomTime(Option<DateTime<Utc>>);

impl CustomTime {
    pub fn new(t: DateTime<Utc>) -> Self {
        CustomTime(Some(t))
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_none()
    }

    /// `Time()` — a zero CustomTime yields the zero-time sentinel.
    pub fn time(&self) -> DateTime<Utc> {
        self.0.unwrap_or_else(zero_time)
    }
}

impl Serialize for CustomTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            None => serializer.serialize_none(),
            Some(t) => serializer.serialize_str(&t.format(CUSTOM_TIME_FORMAT).to_string()),
        }
    }
}

impl<'de> Deserialize<'de> for CustomTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        let s = match opt {
            None => return Ok(CustomTime(None)),
            Some(s) => s,
        };
        if s == "null" || s.is_empty() {
            return Ok(CustomTime(None));
        }
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&s, CUSTOM_TIME_FORMAT) {
            return Ok(CustomTime(Some(Utc.from_utc_datetime(&naive))));
        }
        // Fall back on a Unix timestamp encoded as a string.
        let res = to_time(&serde_json::Value::String(s.clone()));
        if res.is_failure() {
            return Err(de::Error::custom(format!("unable to parse time: {s}")));
        }
        Ok(CustomTime(Some(res.into_value())))
    }
}

// ---------------------------------------------------------------------------
// NullTime — sql.NullTime; marshals as UnixMicro int64 (or null).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NullTime {
    time: Option<DateTime<Utc>>,
}

impl NullTime {
    pub fn new(t: DateTime<Utc>) -> Self {
        NullTime { time: Some(t) }
    }

    pub fn now() -> Self {
        NullTime {
            time: Some(Utc::now()),
        }
    }

    pub fn null() -> Self {
        NullTime { time: None }
    }

    pub fn from_option(t: Option<DateTime<Utc>>) -> Self {
        NullTime { time: t }
    }

    pub fn valid(&self) -> bool {
        self.time.is_some()
    }

    /// The wrapped time; a null value yields the zero-time sentinel, matching
    /// Go's zero `time.Time` when `.Time` is read on an invalid NullTime.
    pub fn time(&self) -> DateTime<Utc> {
        self.time.unwrap_or_else(zero_time)
    }

    pub fn time_opt(&self) -> Option<DateTime<Utc>> {
        self.time
    }

    pub fn unix_milli(&self) -> i64 {
        self.time().timestamp_millis()
    }
}

impl Serialize for NullTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.time {
            None => serializer.serialize_none(),
            Some(t) => serializer.serialize_i64(t.timestamp_micros()),
        }
    }
}

impl<'de> Deserialize<'de> for NullTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
        let value = match value {
            None | Some(serde_json::Value::Null) => return Ok(NullTime { time: None }),
            Some(v) => v,
        };

        // Try int64 microseconds first.
        if let Some(micros) = value.as_i64() {
            let dt = Utc
                .timestamp_micros(micros)
                .single()
                .ok_or_else(|| de::Error::custom("invalid micros timestamp"))?;
            return Ok(NullTime { time: Some(dt) });
        }

        // Then an RFC3339 string.
        if let Some(s) = value.as_str() {
            if s.is_empty() {
                return Ok(NullTime { time: None });
            }
            if let Ok(t) = DateTime::parse_from_rfc3339(s) {
                return Ok(NullTime {
                    time: Some(t.with_timezone(&Utc)),
                });
            }
            return Err(de::Error::custom(format!(
                "unable to parse timestamp string: {s}"
            )));
        }

        Err(de::Error::custom("unable to parse NullTime"))
    }
}
