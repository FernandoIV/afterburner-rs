//! Little helpers: sysfs read/write and value parsing compatible
//! with the MSI Afterburner `.cfg` style (hex values like `7FFFFFFFh`).

use std::fs;
use std::io::Result as IoResult;

pub fn read_to_string(p: &str) -> IoResult<String> {
    fs::read_to_string(p)
}

pub fn read_string(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

pub fn read_i64(p: &str) -> Option<i64> {
    let s = fs::read_to_string(p).ok()?;
    riva_parse_i64(s.trim())
}

/// Parse a number in plain decimal, hexadecimal (`0x`, `h` suffix)
/// exactly like the Windows cfg parser (see MSIAfterburner.cfg in the
/// 4.6.6 distribution, e.g. `AudioCaptureFlags = 00000003h`).
pub fn riva_parse_i64(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(v) = s.strip_prefix("0x") {
        return i64::from_str_radix(v, 16).ok();
    }
    if s.len() > 1 && s.ends_with('h') || s.ends_with('H') {
        let v = &s[..s.len() - 1];
        if let Ok(n) = i64::from_str_radix(v, 16) {
            return Some(n);
        }
    }
    if s.starts_with('-') {
        return s.parse::<i64>().ok();
    }
    if s.chars().all(|c| c.is_ascii_hexdigit()) && !s.is_empty() && s.len() > 1 {
        // Afterburner treats long digit runs without suffix as decimal;
        // pure hex-looking plain digits like "0000" still parse as decimal.
        return s.parse::<i64>().ok();
    }
    s.parse::<i64>().ok()
}

pub fn write_string(p: &str, val: &str) -> IoResult<()> {
    fs::write(p, val)
}

/// Best-effort write that ignores failure (fan curve packets, etc.).
pub fn write_lossy(p: &str, val: &str) {
    let _ = fs::write(p, val);
}

pub fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or("").to_string()
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
