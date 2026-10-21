//! Times and durations on the command line and in tool arguments.
//!
//! A time is `now`, an RFC 3339 timestamp (`2026-10-21T09:30:00Z`, offsets
//! allowed), a date (`2026-10-21`, midnight UTC), `@<unix seconds>`, or a
//! duration meaning that long ago (`90s`, `15m`, `1h`, `7d`, `2w`,
//! optionally `1h ago` or `-1h`). Everything is µs since the epoch.
use crate::Result;
use oojuro_capture::time::{now_us, parse_rfc3339_us};

/// A duration such as `90s`, `15m`, `1h`, `7d`, `2w`, `500ms`, in µs.
pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .ok_or_else(|| format!("duration {s:?} needs a unit (s, m, h, d, w)"))?;
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().map_err(|_| format!("bad duration {s:?}"))?;
    let unit_us: f64 = match unit.trim() {
        "us" => 1.0,
        "ms" => 1e3,
        "s" | "sec" | "secs" => 1e6,
        "m" | "min" | "mins" => 60e6,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600e6,
        "d" | "day" | "days" => 86_400e6,
        "w" | "week" | "weeks" => 604_800e6,
        u => return Err(format!("unknown duration unit {u:?} in {s:?}").into()),
    };
    let us = n * unit_us;
    if !us.is_finite() || us < 0.0 || us > 1e17 {
        return Err(format!("duration {s:?} out of range").into());
    }
    Ok(us as i64)
}

/// A point in time (see the module docs), relative to `now` (µs).
pub fn parse_time_at(s: &str, now: i64) -> Result<i64> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("now") {
        return Ok(now);
    }
    if let Some(secs) = t.strip_prefix('@') {
        let v: f64 = secs.parse().map_err(|_| format!("bad unix time {t:?}"))?;
        return Ok((v * 1e6) as i64);
    }
    if let Some(us) = parse_rfc3339_us(t) {
        return Ok(us);
    }
    if t.len() == 10
        && t.as_bytes()[4] == b'-'
        && t.as_bytes()[7] == b'-'
        && let Some(us) = parse_rfc3339_us(&format!("{t}T00:00:00Z"))
    {
        return Ok(us);
    }
    let rel = t.strip_suffix("ago").map(str::trim).or_else(|| t.strip_prefix('-')).unwrap_or(t);
    match parse_duration(rel) {
        Ok(d) => Ok(now - d),
        Err(_) => {
            Err(format!("cannot read time {t:?} (now, RFC 3339, YYYY-MM-DD, @unix, or a duration like 1h)").into())
        }
    }
}

pub fn parse_time(s: &str) -> Result<i64> {
    parse_time_at(s, now_us())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_times() {
        assert_eq!(parse_duration("90s").unwrap(), 90_000_000);
        assert_eq!(parse_duration("1.5h").unwrap(), 5_400_000_000);
        assert_eq!(parse_duration("7d").unwrap(), 604_800_000_000);
        assert!(parse_duration("7").is_err() && parse_duration("7y").is_err());
        let now = 1_791_394_659_557_000;
        assert_eq!(parse_time_at("now", now).unwrap(), now);
        assert_eq!(parse_time_at("1h", now).unwrap(), now - 3_600_000_000);
        assert_eq!(parse_time_at("1h ago", now).unwrap(), now - 3_600_000_000);
        assert_eq!(parse_time_at("-2d", now).unwrap(), now - 172_800_000_000);
        assert_eq!(parse_time_at("2026-10-07T17:37:39.557Z", now).unwrap(), now);
        assert_eq!(parse_time_at("1970-01-02", now).unwrap(), 86_400_000_000);
        assert_eq!(parse_time_at("@1.5", now).unwrap(), 1_500_000);
        assert!(parse_time_at("yesterday-ish", now).is_err());
    }
}
