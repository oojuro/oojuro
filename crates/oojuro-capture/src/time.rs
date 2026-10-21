//! RFC 3339 timestamps (`2026-10-07T17:37:39.557Z`, offsets allowed) to µs.

/// Days from 1970-01-01 to the given civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn num(b: &[u8], from: usize, len: usize) -> Option<u32> {
    let s = b.get(from..from + len)?;
    let mut v = 0u32;
    for &c in s {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as u32;
    }
    Some(v)
}

/// Parse an RFC 3339 timestamp into µs since the Unix epoch.
pub fn parse_rfc3339_us(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d) = (num(b, 0, 4)? as i64, num(b, 5, 2)?, num(b, 8, 2)?);
    let (h, mi, se) = (num(b, 11, 2)?, num(b, 14, 2)?, num(b, 17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut p = 19;
    let mut frac_us = 0i64;
    if b.get(p) == Some(&b'.') {
        p += 1;
        let mut digits = 0;
        while let Some(c) = b.get(p).filter(|c| c.is_ascii_digit()) {
            if digits < 6 {
                frac_us = frac_us * 10 + (c - b'0') as i64;
            }
            digits += 1;
            p += 1;
        }
        if digits == 0 {
            return None;
        }
        for _ in digits..6 {
            frac_us *= 10;
        }
    }
    let off_s: i64 = match b.get(p) {
        None => 0, // no zone: treat as UTC
        Some(b'Z') | Some(b'z') => 0,
        Some(&c @ (b'+' | b'-')) => {
            let oh = num(b, p + 1, 2)? as i64;
            let om = if b.get(p + 3) == Some(&b':') { num(b, p + 4, 2)? } else { num(b, p + 3, 2)? } as i64;
            let o = oh * 3600 + om * 60;
            if c == b'+' { o } else { -o }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + (h * 3600 + mi * 60 + se) as i64 - off_s;
    Some(secs * 1_000_000 + frac_us)
}

/// Civil date (y, m, d) of a day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// µs since the epoch as RFC 3339 UTC with microseconds
/// (`2026-10-07T17:37:39.557000Z`); the inverse of [`parse_rfc3339_us`].
pub fn format_rfc3339_us(us: i64) -> String {
    let secs = us.div_euclid(1_000_000);
    let frac = us.rem_euclid(1_000_000);
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{frac:06}Z", sod / 3600, sod % 3600 / 60, sod % 60)
}

/// Wall clock now in µs.
pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse_rfc3339_us("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_us("1970-01-01T00:00:01.5Z"), Some(1_500_000));
        assert_eq!(parse_rfc3339_us("2000-03-01T00:00:00Z"), Some(951_868_800_000_000));
        // 2026-10-07T17:37:39.557Z
        assert_eq!(parse_rfc3339_us("2026-10-07T17:37:39.557Z"), Some(1_791_394_659_557_000));
        assert_eq!(parse_rfc3339_us("2026-10-07T19:37:39.557+02:00"), Some(1_791_394_659_557_000));
        assert_eq!(parse_rfc3339_us("2026-10-07T17:37:39.557123456Z"), Some(1_791_394_659_557_123));
        assert_eq!(parse_rfc3339_us("nope"), None);
        assert_eq!(parse_rfc3339_us("2026-13-07T17:37:39Z"), None);
    }

    #[test]
    fn formats_and_round_trips() {
        assert_eq!(format_rfc3339_us(0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(format_rfc3339_us(1_791_394_659_557_123), "2026-10-07T17:37:39.557123Z");
        assert_eq!(format_rfc3339_us(-1), "1969-12-31T23:59:59.999999Z");
        for us in [0i64, 951_868_800_000_000, 1_791_394_659_557_123, 4_102_444_800_000_001, 1_709_164_800_123_456] {
            assert_eq!(parse_rfc3339_us(&format_rfc3339_us(us)), Some(us));
        }
    }
}
