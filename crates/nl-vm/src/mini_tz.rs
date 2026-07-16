//! Calendar math and IANA timezone lookups for `system.time.*` (stdlib.md
//! § system.time.DateTime, system.time.TimeZone) — no external crate, same
//! "read the OS-provided data directly" stance already taken for
//! `system.SecureRandom`/`Uuid` (`/dev/urandom`) and `system.ps.Process`
//! (`/proc`, `/etc/passwd`): timezone rules are read straight out of the
//! system's `/usr/share/zoneinfo` TZif database (RFC 8536) instead of
//! vendoring one.
//!
//! Only the binary transition table is parsed (the v2/v3 64-bit block when
//! present, falling back to the v1 32-bit block otherwise) — the trailing
//! POSIX TZ footer string that extrapolates DST rules *beyond* the last
//! explicit transition is not parsed. Documented gap: a `DateTime` far
//! enough in the future to run past a zone's last recorded transition (most
//! zones' tables extend past 2037) keeps that last transition's offset
//! instead of applying the zone's perpetual DST rule.

pub fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The process default timezone (stdlib.md: `TimeZone.getDefault()`) — the
/// `TZ` environment variable if set, else the target of the `/etc/localtime`
/// symlink (the usual `tzdata` convention: a symlink into
/// `/usr/share/zoneinfo`), else `"UTC"`.
pub fn default_zone_id() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() {
            return tz;
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        if let Some(s) = target.to_str() {
            if let Some(idx) = s.find("zoneinfo/") {
                return s[idx + "zoneinfo/".len()..].to_string();
            }
        }
    }
    "UTC".to_string()
}

/// UTC offset in seconds for `zone_id` at instant `epoch` (UTC seconds since
/// the Unix epoch) — `Err` if `zone_id` isn't a recognized fixed offset
/// (`"+HH:MM"`/`"-HH:MM"`) and doesn't name a readable/parseable
/// `/usr/share/zoneinfo` entry (also `TimeZone.get`'s validation: stdlib.md
/// declares it `throws IllegalArgumentException` on an unknown id).
pub fn zone_offset_seconds(zone_id: &str, epoch: i64) -> Result<i32, String> {
    if matches!(zone_id, "UTC" | "Etc/UTC" | "GMT" | "Z") {
        return Ok(0);
    }
    if let Some(off) = parse_fixed_offset(zone_id) {
        return Ok(off);
    }
    // `zone_id` ultimately reaches `std::fs::read` below — reject path
    // traversal/absolute paths before ever touching the filesystem (a
    // dotted-path-shaped id like "../../etc/passwd" must not resolve
    // outside `/usr/share/zoneinfo`).
    if zone_id.is_empty() || zone_id.starts_with('/') || zone_id.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(format!("unknown timezone '{zone_id}'"));
    }
    let path = format!("/usr/share/zoneinfo/{zone_id}");
    let data = std::fs::read(&path).map_err(|e| format!("unknown timezone '{zone_id}': {e}"))?;
    tzif_offset(&data, epoch).map_err(|e| format!("unknown timezone '{zone_id}': {e}"))
}

struct TzHeader {
    isutcnt: u32,
    isstdcnt: u32,
    leapcnt: u32,
    timecnt: u32,
    typecnt: u32,
    charcnt: u32,
}

fn read_be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}

fn read_be_i32(b: &[u8]) -> i32 {
    i32::from_be_bytes(b.try_into().unwrap())
}

fn read_be_i64(b: &[u8]) -> i64 {
    i64::from_be_bytes(b.try_into().unwrap())
}

/// Parses the 44-byte TZif header (RFC 8536 § 3) at `pos`, returning it plus
/// the version byte (`0` for the legacy v1-only format, `b'2'`/`b'3'`/`b'4'`
/// when a second, 64-bit block follows).
fn parse_header(data: &[u8], pos: usize) -> Result<(TzHeader, u8), String> {
    if data.len() < pos + 44 || &data[pos..pos + 4] != b"TZif" {
        return Err("not a TZif file".to_string());
    }
    let version = data[pos + 4];
    Ok((
        TzHeader {
            isutcnt: read_be_u32(&data[pos + 20..pos + 24]),
            isstdcnt: read_be_u32(&data[pos + 24..pos + 28]),
            leapcnt: read_be_u32(&data[pos + 28..pos + 32]),
            timecnt: read_be_u32(&data[pos + 32..pos + 36]),
            typecnt: read_be_u32(&data[pos + 36..pos + 40]),
            charcnt: read_be_u32(&data[pos + 40..pos + 44]),
        },
        version,
    ))
}

/// Byte length of the data block following a header with transition times
/// encoded on `time_size` bytes each (4 for v1, 8 for v2/v3).
fn data_block_len(h: &TzHeader, time_size: usize) -> usize {
    h.timecnt as usize * time_size
        + h.timecnt as usize
        + h.typecnt as usize * 6
        + h.charcnt as usize
        + h.leapcnt as usize * (time_size + 4)
        + h.isstdcnt as usize
        + h.isutcnt as usize
}

/// UTC offset in effect at `epoch`, per the transition table starting right
/// after the header at `header_pos` (RFC 8536 § 3: transition times, then
/// one type-index byte per transition, then `typecnt` 6-byte `ttinfo`
/// records — everything after that, abbreviation strings/leap
/// seconds/std-wall and UT-local indicators, is irrelevant to a plain offset
/// lookup and is not parsed).
fn find_offset(data: &[u8], header_pos: usize, h: &TzHeader, time_size: usize, epoch: i64) -> Result<i32, String> {
    let mut pos = header_pos + 44;
    let mut transitions = Vec::with_capacity(h.timecnt as usize);
    for _ in 0..h.timecnt {
        let t = if time_size == 4 {
            read_be_i32(&data[pos..pos + 4]) as i64
        } else {
            read_be_i64(&data[pos..pos + 8])
        };
        transitions.push(t);
        pos += time_size;
    }
    let mut types = Vec::with_capacity(h.timecnt as usize);
    for _ in 0..h.timecnt {
        types.push(data[pos]);
        pos += 1;
    }
    let mut ttinfo = Vec::with_capacity(h.typecnt as usize);
    for _ in 0..h.typecnt {
        let off = read_be_i32(&data[pos..pos + 4]);
        let isdst = data[pos + 4] != 0;
        ttinfo.push((off, isdst));
        pos += 6;
    }
    if ttinfo.is_empty() {
        return Err("no timezone type records".to_string());
    }
    let idx = match transitions.binary_search(&epoch) {
        Ok(i) => Some(i),
        Err(0) => None, // before the first transition
        Err(i) => Some(i - 1),
    };
    let type_idx = match idx {
        // RFC 8536 § 3.2: before the first transition, use the first
        // standard-time (non-DST) type, or type 0 if none is standard.
        None => ttinfo.iter().position(|t| !t.1).unwrap_or(0),
        Some(i) => types[i] as usize,
    };
    ttinfo.get(type_idx).map(|t| t.0).ok_or_else(|| "invalid transition type index".to_string())
}

fn tzif_offset(data: &[u8], epoch: i64) -> Result<i32, String> {
    let (h1, version) = parse_header(data, 0)?;
    if version == 0 {
        return find_offset(data, 0, &h1, 4, epoch);
    }
    let v2_header_pos = 44 + data_block_len(&h1, 4);
    let (h2, _) = parse_header(data, v2_header_pos)?;
    find_offset(data, v2_header_pos, &h2, 8, epoch)
}

/// `"+HH:MM"`/`"-HH:MM"` fixed-offset pseudo zone id (not backed by any
/// `/usr/share/zoneinfo` entry) — how a `DateTime.parse`d instant with an
/// explicit numeric offset (rather than `Z` or a named zone) is represented.
fn parse_fixed_offset(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    if bytes.len() != 6 || bytes[3] != b':' {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hh: i32 = s.get(1..3)?.parse().ok()?;
    let mm: i32 = s.get(4..6)?.parse().ok()?;
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(sign * (hh * 3600 + mm * 60))
}

fn format_fixed_offset(offset_secs: i32) -> String {
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let abs = offset_secs.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

/// Days since the Unix epoch (1970-01-01) for the given Gregorian civil
/// date — Howard Hinnant's `days_from_civil`
/// (<https://howardhinnant.github.io/date_algorithms.html>), exact for any
/// proleptic Gregorian date, ported as-is (Rust's `/` truncates toward zero
/// like C++'s, which the algorithm relies on).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of `days_from_civil` — Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `epoch` (UTC seconds) shifted by `offset_secs` and broken down into wall
/// `(year, month, day, hour, minute, second)`.
pub fn epoch_to_local(epoch: i64, offset_secs: i32) -> (i64, u32, u32, u32, u32, u32) {
    let local = epoch + offset_secs as i64;
    let days = local.div_euclid(86400);
    let secs_of_day = local.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, (secs_of_day / 3600) as u32, ((secs_of_day / 60) % 60) as u32, (secs_of_day % 60) as u32)
}

/// Inverse of `epoch_to_local`: a wall-clock civil date/time at UTC offset
/// `offset_secs` back to the UTC instant it represents.
pub fn local_to_epoch(y: i64, m: u32, d: u32, hh: u32, mm: u32, ss: u32, offset_secs: i32) -> i64 {
    days_from_civil(y, m, d) * 86400 + hh as i64 * 3600 + mm as i64 * 60 + ss as i64 - offset_secs as i64
}

/// Parses `YYYY-MM-DDTHH:MM:SS` followed by either `Z` or an explicit
/// `±HH:MM` offset (stdlib.md § system.time.DateTime.parse's two examples:
/// `2025-03-01T14:30:00Z`, `2025-03-01T15:30:00+01:00`). Returns
/// `(epoch_seconds, zone_id)`, `zone_id` being `"UTC"` for `Z` or the
/// canonical `"±HH:MM"` form for an explicit offset. No fractional seconds,
/// no non-`T` separator, no bare (zone-less) local time — all rejected, same
/// as any other malformed input.
pub fn parse_iso8601(s: &str) -> Result<(i64, String), String> {
    let fail = || format!("invalid datetime '{s}'");
    let bytes = s.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(fail());
    }
    let digits_only = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    for part in [&s[0..4], &s[5..7], &s[8..10], &s[11..13], &s[14..16], &s[17..19]] {
        if !digits_only(part) {
            return Err(fail());
        }
    }
    let year: i64 = s[0..4].parse().map_err(|_| fail())?;
    let month: u32 = s[5..7].parse().map_err(|_| fail())?;
    let day: u32 = s[8..10].parse().map_err(|_| fail())?;
    let hour: u32 = s[11..13].parse().map_err(|_| fail())?;
    let minute: u32 = s[14..16].parse().map_err(|_| fail())?;
    let second: u32 = s[17..19].parse().map_err(|_| fail())?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return Err(fail());
    }
    let rest = &s[19..];
    let (offset_secs, zone_id) = if rest == "Z" {
        (0, "UTC".to_string())
    } else if let Some(off) = parse_fixed_offset(rest) {
        (off, format_fixed_offset(off))
    } else {
        return Err(fail());
    };
    Ok((local_to_epoch(year, month, day, hour, minute, second, offset_secs), zone_id))
}

/// Formats `epoch`/`offset_secs`'s wall-clock date/time against a small
/// Java-`SimpleDateFormat`-like token set (stdlib.md's own example:
/// `"yyyy-MM-dd HH:mm"`) — `y`/`M`/`d`/`H`/`m`/`s` runs are replaced by the
/// corresponding field, zero-padded to at least the run length (`yy` is
/// special-cased to the last two digits of the year, matching the common
/// convention); every other character (including unsupported letters) is
/// copied through literally — no quoting syntax.
pub fn format_datetime(epoch: i64, offset_secs: i32, pattern: &str) -> String {
    let (y, mo, d, hh, mi, ss) = epoch_to_local(epoch, offset_secs);
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let mut run = 1;
        while i + run < chars.len() && chars[i + run] == c {
            run += 1;
        }
        match c {
            'y' if run == 2 => out.push_str(&format!("{:02}", y.rem_euclid(100))),
            'y' => out.push_str(&format!("{:0width$}", y, width = run)),
            'M' => out.push_str(&format!("{:0width$}", mo, width = run)),
            'd' => out.push_str(&format!("{:0width$}", d, width = run)),
            'H' => out.push_str(&format!("{:0width$}", hh, width = run)),
            'm' => out.push_str(&format!("{:0width$}", mi, width = run)),
            's' => out.push_str(&format!("{:0width$}", ss, width = run)),
            _ => out.extend(std::iter::repeat_n(c, run)),
        }
        i += run;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2023, 1, 15), (2038, 1, 19), (1969, 12, 31), (1900, 3, 1)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m}-{d}");
        }
    }

    #[test]
    fn known_epoch_constants() {
        assert_eq!(local_to_epoch(1970, 1, 1, 0, 0, 0, 0), 0);
        assert_eq!(local_to_epoch(2000, 1, 1, 0, 0, 0, 0), 946684800);
        assert_eq!(local_to_epoch(2038, 1, 19, 3, 14, 7, 0), 2147483647);
    }

    #[test]
    fn epoch_local_roundtrip() {
        let (y, m, d, hh, mm, ss) = epoch_to_local(946684800, 0);
        assert_eq!((y, m, d, hh, mm, ss), (2000, 1, 1, 0, 0, 0));
    }

    #[test]
    fn parses_iso8601_with_z_and_offset() {
        let (epoch, zone) = parse_iso8601("2025-03-01T14:30:00Z").unwrap();
        assert_eq!(zone, "UTC");
        assert_eq!(epoch, local_to_epoch(2025, 3, 1, 14, 30, 0, 0));

        let (epoch, zone) = parse_iso8601("2025-03-01T15:30:00+01:00").unwrap();
        assert_eq!(zone, "+01:00");
        assert_eq!(epoch, local_to_epoch(2025, 3, 1, 15, 30, 0, 0) - 3600);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse_iso8601("not a date").is_err());
        assert!(parse_iso8601("2025-13-01T00:00:00Z").is_err());
        assert!(parse_iso8601("2025-03-01T14:30:00").is_err()); // no zone suffix
    }

    #[test]
    fn utc_offset_is_zero() {
        assert_eq!(zone_offset_seconds("UTC", 0).unwrap(), 0);
    }

    #[test]
    fn fixed_offset_zone() {
        assert_eq!(zone_offset_seconds("+05:30", 0).unwrap(), 5 * 3600 + 30 * 60);
        assert_eq!(zone_offset_seconds("-08:00", 0).unwrap(), -8 * 3600);
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(zone_offset_seconds("../../etc/passwd", 0).is_err());
        assert!(zone_offset_seconds("/etc/passwd", 0).is_err());
    }

    #[test]
    fn unknown_zone_errors() {
        assert!(zone_offset_seconds("Not/A_Real_Zone", 0).is_err());
    }

    /// Real `/usr/share/zoneinfo` DST transitions — this project's dev/CI
    /// environment is Linux with the standard `tzdata` package, same
    /// assumption already made by `system.ps.Process`'s `/proc` reads.
    #[test]
    fn europe_paris_dst_transitions() {
        let winter = local_to_epoch(2023, 1, 15, 12, 0, 0, 0);
        let summer = local_to_epoch(2023, 7, 15, 12, 0, 0, 0);
        assert_eq!(zone_offset_seconds("Europe/Paris", winter).unwrap(), 3600);
        assert_eq!(zone_offset_seconds("Europe/Paris", summer).unwrap(), 7200);
    }

    #[test]
    fn america_new_york_dst_transitions() {
        let winter = local_to_epoch(2023, 1, 15, 12, 0, 0, 0);
        let summer = local_to_epoch(2023, 7, 15, 12, 0, 0, 0);
        assert_eq!(zone_offset_seconds("America/New_York", winter).unwrap(), -5 * 3600);
        assert_eq!(zone_offset_seconds("America/New_York", summer).unwrap(), -4 * 3600);
    }

    #[test]
    fn formats_datetime() {
        let epoch = local_to_epoch(2025, 3, 1, 9, 5, 3, 0);
        assert_eq!(format_datetime(epoch, 0, "yyyy-MM-dd HH:mm"), "2025-03-01 09:05");
        assert_eq!(format_datetime(epoch, 0, "yy/M/d H:m:s"), "25/3/1 9:5:3");
    }
}
