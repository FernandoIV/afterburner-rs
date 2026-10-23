//! `.cfg`-style INI parser and profile store.
//!
//! Configuration format intentionally matches the MSIAfterburner.cfg
//! dialect from the Windows distribution (sections + `Key<tab> = value`,
//! hex digits accepted with `h` suffix), so users can copy the tunables
//! they know. Profiles live in `~/.config/afterburner-rs/Profiles/`.

use crate::util::riva_parse_i64;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Clone)]
pub struct Ini {
    pub sections: BTreeMap<String, Vec<(String, String)>>,
}

impl Ini {
    pub fn parse(text: &str) -> Ini {
        let mut ini = Ini::default();
        let mut cur = String::from("");
        ini.sections.insert(String::new(), vec![]);
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                cur = line[1..line.len() - 1].trim().to_string();
                ini.sections.entry(cur.clone()).or_default();
                continue;
            }
            if let Some(eq) = line.find('=') {
                let key = line[..eq].trim().trim_end_matches('\t').trim().to_string();
                let val = line[eq + 1..].trim().to_string();
                ini.sections.entry(cur.clone()).or_default().push((key, val));
            } else {
                ini.sections.entry(cur.clone()).or_default().push((line.to_string(), String::new()));
            }
        }
        ini
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section)?.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }

    pub fn get_i64(&self, section: &str, key: &str) -> Option<i64> {
        self.get(section, key).and_then(riva_parse_i64)
    }

    pub fn get_f32(&self, s: &str, k: &str) -> Option<f32> {
        let raw = self.get(s, k)?;
        raw.parse::<f32>().ok().filter(|v| v.is_finite())
            .or_else(|| self.get_i64(s, k).map(|v| v as f32))
    }

    pub fn set(&mut self, section: &str, key: &str, val: &str) {
        let kv = self.sections.entry(section.to_string()).or_default();
        if let Some(v) = kv.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            v.1 = val.to_string();
        } else {
            kv.push((key.to_string(), val.to_string()));
        }
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (sec, kvs) in &self.sections {
            if !sec.is_empty() {
                out.push_str(&format!("[{sec}]\n"));
            }
            for (k, v) in kvs {
                if v.is_empty() {
                    out.push_str(&format!("{k}\n"));
                } else {
                    out.push_str(&format!("{k:<24}\t= {v}\n"));
                }
            }
            out.push('\n');
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct ProfileValues {
    pub core_clock_offset: Option<i32>, // MHz offset (boost)
    pub mem_clock_offset: Option<i32>,  // MHz offset
    pub power_limit_pct: Option<f32>,   // % of default
    pub power_limit_w: Option<f32>,     // absolute watts
    pub core_voltage_boost: Option<i32>,
    pub thermal_limit: Option<u32>,
    pub fan_pct: Option<u32>,
    pub fan_auto: bool,
    pub fan_curve_nodes: Vec<(f32, f32)>, //templ
    pub name: String,
}

impl Default for ProfileValues {
    fn default() -> Self {
        Self { core_clock_offset: None, mem_clock_offset: None, power_limit_pct: None, power_limit_w: None, core_voltage_boost: None, thermal_limit: None, fan_pct: None, fan_auto: true, fan_curve_nodes: vec![], name: "default".into() }
    }
}

#[derive(Debug, Clone)]
pub struct ProfileValuesOwned {
    pub v: ProfileValues,
}

pub fn config_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("afterburner-rs");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config").join("afterburner-rs")
}

pub fn profiles_dir() -> PathBuf {
    config_dir().join("Profiles")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("MSIAfterburner.cfg")
}

/// Key naming mirrors the dialog strings found in the Windows binary
/// (CoreClockBoostControl / MemoryClockBoostControl / PowerLimit /
/// CoreVoltageBoost / ThermalLimit / FanSpeed...).
pub fn save_profile(idx: u32, v: &ProfileValues, base_limits: &crate::drivers::Limits) -> std::io::Result<PathBuf> {
    let d = profiles_dir();
    std::fs::create_dir_all(&d)?;
    let p = d.join(format!("Profile{idx}.cfg"));
    let mut ini = Ini::default();
    ini.sections.insert(String::from("Profile2"), vec![]);
    let poww: Option<f32> = v.power_limit_w.or_else(|| {
        v.power_limit_pct.and_then(|pct| pct_limits(base_limits).1.map(|max| pct * max / 100.0))
    });
    let s = &mut ini.sections;
    let set = |s: &mut BTreeMap<String, Vec<(String, String)>>, k: &str, val: String| {
        s.entry("Settings".into()).or_default().push((k.to_string(), val));
    };
    if let Some(o) = v.core_clock_offset {
        set(s, "CoreClockBoost", o.to_string());
    }
    if let Some(o) = v.mem_clock_offset {
        set(s, "MemoryClockBoost", o.to_string());
    }
    if let Some(w) = poww {
        set(s, "PowerLimitW", format!("{:.1}", w));
    }
    if let Some(p) = v.power_limit_pct {
        set(s, "PowerLimit", format!("{:.1}", p));
    }
    if let Some(b) = v.core_voltage_boost {
        set(s, "CoreVoltageBoost", b.to_string());
    }
    if let Some(t) = v.thermal_limit {
        set(s, "ThermalLimit", t.to_string());
    }
    if v.fan_auto {
        set(s, "FanSpeed", String::from("AUTO"));
    } else if let Some(f) = v.fan_pct {
        set(s, "FanSpeed", f.to_string());
    }
    if !v.fan_curve_nodes.is_empty() {
        set(s, "SoftwareFanCurve", format_nodes(&v.fan_curve_nodes));
    }
    set(s, "Name", v.name.clone());
    std::fs::write(&p, ini.to_text())?;
    Ok(p)
}

pub fn format_nodes(nodes: &[(f32, f32)]) -> String {
    nodes
        .iter()
        .map(|(t, d)| format!("{}:{}", fmtf(*t), fmtf(*d)))
        .collect::<Vec<_>>()
        .join(",")
}

fn fmtf(v: f32) -> String {
    if (v - v.round()).abs() < f32::EPSILON {
        format!("{}", v as i32)
    } else {
        format!("{}", v)
    }
}

/// Returns (min_w => None, max_w) helper pair from limits.
fn pct_limits(l: &crate::drivers::Limits) -> (Option<f32>, Option<f32>) {
    match (l.power_default_w, l.power_max_w) {
        (Some(d), Some(m)) if m > 0.0 && d > 0.0 => (Some(d * 0.5), Some(d + (m - d))),
        (_, Some(m)) => (Some(m * 0.5), Some(m)),
        _ => (None, None),
    }
}

pub fn load_profile(idx: u32) -> Option<ProfileValues> {
    let p = profiles_dir().join(format!("Profile{idx}.cfg"));
    load_profile_path(&p)
}

pub fn load_profile_path(p: &PathBuf) -> Option<ProfileValues> {
    let text = std::fs::read_to_string(p).ok()?;
    let ini = Ini::parse(&text);
    let mut v = ProfileValues::default();
    if let Some(x) = ini.get_i64("Settings", "CoreClockBoost") {
        v.core_clock_offset = Some(x as i32);
    }
    if let Some(x) = ini.get_i64("Settings", "MemoryClockBoost") {
        v.mem_clock_offset = Some(x as i32);
    }
    if let Some(x) = ini.get_f32("Settings", "PowerLimitW") {
        v.power_limit_w = Some(x);
    }
    if let Some(x) = ini.get_f32("Settings", "PowerLimit") {
        v.power_limit_pct = Some(x);
    }
    if let Some(x) = ini.get_i64("Settings", "CoreVoltageBoost") {
        v.core_voltage_boost = Some(x as i32);
    }
    if let Some(x) = ini.get_i64("Settings", "ThermalLimit") {
        v.thermal_limit = Some(x as u32);
    }
    match ini.get("Settings", "FanSpeed") {
        Some("AUTO") | Some("Auto") | Some("auto") => v.fan_auto = true,
        Some(s) => {
            let n: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
            if let Ok(f) = n.parse::<u32>() {
                v.fan_pct = Some(f);
                v.fan_auto = false;
            }
        }
        None => {}
    }
    if let Some(curve) = ini.get("Settings", "SoftwareFanCurve") {
        let mut nodes = vec![];
        for part in curve.split(',') {
            let ab: Vec<&str> = part.split(':').collect();
            if ab.len() == 2 {
                if let (Ok(t), Ok(d)) = (ab[0].trim().parse::<f32>(), ab[1].trim().parse::<f32>()) {
                    nodes.push((t, d));
                }
            }
        }
        if !nodes.is_empty() {
            v.fan_curve_nodes = nodes;
        }
    }
    if let Some(n) = ini.get("Settings", "Name") {
        v.name = n.to_string();
    }
    Some(v)
}

pub fn default_settings() -> String {
    // Keys and defaults mirroring the shipped MSIAfterburner.cfg [Settings].
    let mut ini = Ini::default();
    ini.set("Settings", "HwPollPeriod", "1000");
    ini.set("Settings", "FanSpeedReadbackDelay", "1000");
    ini.set("Settings", "EnableSharedMemory", "1");
    ini.set("Settings", "EnableLog", "0");
    ini.set("Settings", "LogPath", "");
    ini.set("Settings", "CurrentGpu", "0");
    ini.set("Settings", "StartupProfile", "none");
    ini.set("Settings", "RestoreAfterSuspendedMode", "1");
    ini.set("Properties", "ShowMonitoringTab", "1");
    ini.set("Properties", "ShowFanTab", "1");
    ini.to_text()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_profile_values_and_hex_settings() {
        let ini = Ini::parse("[Settings]\nPowerLimitW = 290.5\nFlags = 00000003h\nBad = NaN\n");
        assert_eq!(ini.get_f32("Settings", "PowerLimitW"), Some(290.5));
        assert_eq!(ini.get_f32("Settings", "Flags"), Some(3.0));
        assert_eq!(ini.get_f32("Settings", "Bad"), None);
    }
}
