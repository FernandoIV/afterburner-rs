//! AMD amdgpu backend.
//!
//! Windows 4.6.6 drives `CATIADLWrapper` over atiadlxx.dll (Overdrive5/6/N/8);
//! on Linux the equivalent lives in the amdgpu kernel parameters:
//!   - clocks/voltage/nsík:   device/pp_od_clk_voltage (OD6/ODN/OD8 syntax)
//!   - fan:                   hwmon pwm1_enable mode 0 (manual) / pwm1 write
//!   - sensors:               hwmon temp*/in*/power*/fan*
//!   - dpm levels:            device/pp_dpm_sclk / pp_dpm_mclk

use super::{ApplySettings, AppliedReport, Caps, GpuDriver, Limits, Sample};
use crate::util;
use std::path::PathBuf;

pub struct AmdDriver {
    dev_dir: PathBuf,  // /sys/class/drm/cardX/device
    hwmon: Option<PathBuf>,
}

impl AmdDriver {
    pub fn new(dev_dir: PathBuf, hwmon: Option<PathBuf>) -> Self {
        Self { dev_dir, hwmon }
    }

    fn hw(&self, name: &str) -> Option<String> {
        let p = self.hwmon.as_ref()?;
        let v = util::read_string(&p.join(name).to_string_lossy());
        if v.is_empty() {
            None
        } else {
            Some(v.trim().to_string())
        }
    }

    fn hw_f(&self, name: &str) -> Option<f32> {
        self.hw(name).and_then(|v| v.parse::<f32>().ok())
    }

    fn pp_read(&self, name: &str) -> Option<String> {
        let p = self.dev_dir.join(name);
        let v = util::read_string(&p.to_string_lossy());
        if v.is_empty() { None } else { Some(v) }
    }

    fn pp_write(&self, name: &str, content: &str) -> Result<(), String> {
        let p = self.dev_dir.join(name);
        util::write_string(&p.to_string_lossy(), content)
            .map_err(|e| format!("{name}: {e}"))
    }

    /// Currents: pick from pp_dpm_sclk listing like "1: 2450Mhz *".
    fn dpm_current(&self, which: &str) -> Option<f32> {
        let txt = self.pp_read(which)?;
        for line in txt.lines() {
            let t = line.trim();
            if t.contains('*') {
                let colon = t.find(':')?;
                let rest = t[colon + 1..].trim().trim_end_matches('*').trim();
                let n: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                return n.parse::<f32>().ok();
            }
        }
        // last line fallback
        let t = txt.lines().next_back()?;
        let colon = t.find(':')?;
        let rest = t[colon + 1..].trim().trim_end_matches('*').trim();
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        n.parse::<f32>().ok()
    }

    fn hwattr(&self, prefix: &str) -> Option<(String, f32)> {
        let h = self.hwmon.as_ref()?;
        let rd = std::fs::read_dir(h).ok()?;
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for n in names {
            if n.starts_with(prefix) && !n.ends_with("_label") {
                if let Some(v) = self.hw_f(&n) {
                    return Some((n, v));
                }
            }
        }
        None
    }
}

impl GpuDriver for AmdDriver {
    fn sample(&mut self) -> Sample {
        let fan_rpm = self.hwattr("fan1_").filter(|p| p.0.starts_with("fan1_input")).map(|p| p.1);
        let pwm = self.hwattr("pwm1").filter(|p| p.0.starts_with("pwm1")).map(|p| p.1);
        let fan_pct = match self.hwattr("pwm1").filter(|p| p.0.starts_with("pwm1")) {
            Some((_, raw)) => Some(raw / 2.55), // 0..255 → %
            None => fan_rpm.clone().and_then(|_| self.hwattr("pwm1").map(|(_n, v)| v / 2.55)),
        };
        let _ = pwm;
        let temp_core = self.hwattr("temp1_input").map(|p| p.1 / 1000.0);
        let temp_mem = self.hwattr("temp2_input").map(|p| p.1 / 1000.0)
            .or_else(|| self.hwattr("temp3_input").map(|p| p.1 / 1000.0));
        let power_w = self
            .hwattr("power1_average")
            .or_else(|| self.hwattr("power1_input"))
            .map(|p| p.1 / 1_000_000.0);
        let power_limit_w = self.hw_f("power1_cap").map(|v| v / 1_000_000.0);
        let usage_gpu = self.hwattr("freq1_critical") // not typical
            .and(None::<f32>)
            .or_else(|| {
                // busy_percent exists for some ASICs via amdgpu sysfs
                let p = self.dev_dir.join("gpu_busy_percent");
                util::read_string(&p.to_string_lossy())
                    .trim()
                    .parse::<f32>()
                    .ok()
            });
        Sample {
            temp_core,
            temp_mem,
            fan_pct,
            fan_rpm,
            clk_core: self.dpm_current("pp_dpm_sclk"),
            clk_mem: self.dpm_current("pp_dpm_mclk"),
            usage_gpu,
            usage_mem: None,
            power_w,
            power_limit_w,
            voltage_mv: self.hwattr("in0_input").map(|p| p.1 / 1000.0),
        }
    }

    fn limits(&mut self) -> Limits {
        let mut lim = Limits::default();
        if let Some(od) = self.pp_read("pp_od_clk_voltage") {
            for line in od.lines() {
                let t = line.trim();
                if t.starts_with("SCLK:") || t.starts_with("OD_SCLK") {
                    let (mn, mx) = bounds_from_line(t);
                    lim.clk_core_offset_min = mn; // operated in absolute later
                    lim.clk_core_offset_max = mx;
                } else if t.starts_with("MCLK:") || t.starts_with("OD_MCLK") {
                    let (mn, mx) = bounds_from_line(t);
                    lim.clk_mem_offset_min = mn;
                    lim.clk_mem_offset_max = mx;
                } else if t.starts_with("FAST_PPT_LIMIT") || t.starts_with("POWER") {
                    let n: String = t.split(':').nth(1).unwrap_or("").trim().to_string();
                    let n2 = n.split('(').next().unwrap_or("").trim().to_string();
                    if let Some(v) = n2.split_whitespace().next().and_then(|x| x.parse::<f32>().ok()) {
                        if lim.power_max_w.is_none() {
                            lim.power_max_w = Some(v);
                        } else if lim.power_min_w.is_none() {
                            lim.power_min_w = Some(v);
                        }
                    }
                }
            }
        }
        if let Some(cap) = self.hw_f("power1_cap_min").filter(|v| *v > 0.0) {
            lim.power_min_w = Some(cap / 1_000_000.0);
        }
        if let Some(cap) = self.hw_f("power1_cap_max").filter(|v| *v > 0.0) {
            lim.power_max_w = Some(cap / 1_000_000.0);
        }
        lim
    }

    fn caps(&mut self) -> Caps {
        Caps {
            core_clock_offset: true,  // OD tables
            mem_clock_offset: true,
            power_limit: true,
            core_voltage: false,      // Only with overdrive voltage table / unofficial
            fan: self.hw("pwm1").is_some(),
            fan_auto: false,
            thermal_limit: self.hw("temp1_crit_hyst").is_some() || true,
            vfs_curve: self.pp_read("pp_od_clk_voltage").map(|t| t.contains("VDDC")).unwrap_or(false),
        }
    }

    fn apply(&mut self, s: &ApplySettings) -> AppliedReport {
        let mut rep = AppliedReport::default();
        let od_path = "pp_od_clk_voltage";
        // Fan control
        if let Some(h) = &self.hwmon {
            if s.fan_auto {
                let _ = util::write_string(&h.join("pwm1_enable").to_string_lossy(), "2\n");
                rep.fan = (None, true);
            } else if let Some(pct) = s.fan_pct {
                let pwm = pct.clamp(0, 100) * 255 / 100;
                let _ = util::write_string(&h.join("pwm1_enable").to_string_lossy(), "1\n");
                let _ = util::write_string(&h.join("pwm1").to_string_lossy(), &format!("{pwm}\n"));
                rep.fan = (Some(pct), false);
            }
        }
        // OD table
        if let Some(od) = self.pp_read(od_path) {
            let mut had_lines = false;
            for line in od.lines() {
                let t = line.trim();
                if t.starts_with("OD_SCLK") || t.starts_with("SCLK:") {
                    if let Some(mx) = limits_of(t).1 {
                        if let Some(off) = s.core_offset_mhz {
                            // Offset semantics: raise/min raise the max level
                            let (min_level, max_level) = levels_of(t, mx);
                            if off >= 0 {
                                let _ = self.pp_write(od_path, &format!("s {} {}", max_level, mx + off as i32));
                            } else {
                                let _ = self.pp_write(od_path, &format!("s {} {}", min_level, mx + off as i32));
                            }
                            had_lines = true;
                            rep.core_offset = Some(off);
                        }
                    }
                }
                if t.starts_with("OD_MCLK") || t.starts_with("MCLK:") {
                    if let Some(mx) = limits_of(t).1 {
                        if let Some(off) = s.mem_offset_mhz {
                            let (min_level, max_level) = levels_of(t, mx);
                            if off >= 0 {
                                let _ = self.pp_write(od_path, &format!("m {} {}", max_level, mx + off as i32));
                            } else {
                                let _ = self.pp_write(od_path, &format!("m {} {}", min_level, mx + off as i32));
                            }
                            had_lines = true;
                            rep.mem_offset = Some(off);
                        }
                    }
                }
            }
            let _ = self.hw("pwm1");
            let _ = had_lines;
            // Power limit
            if let Some(pl) = s.power_limit_w {
                if od.contains("FAST_PPT_LIMIT") || od.contains("POWER") {
                    let _ = self.pp_write(od_path, &format!("p {:.0}", pl * 1000.0));
                    rep.power_limit = Some(pl);
                    if let Some(pc) = self.hw_f("power1_cap") {
                        if pc > 0.0 {
                            self.hwmon = self.hwmon.clone();
                        }
                    }
                } else if let Some(cap_path) = self.hwmon.as_ref().map(|h| h.join("power1_cap")) {
                    let uw = (pl * 1_000_000.0) as i64;
                    if util::write_string(&cap_path.to_string_lossy(), &format!("{uw}\n")).is_ok() {
                        rep.power_limit = Some(pl);
                    } else {
                        rep.notes.push("power1_cap write failed".into());
                    }
                }
            }
            // Commit
            if rep.core_offset.is_some() || rep.mem_offset.is_some() {
                match self.pp_write(od_path, "c") {
                    Ok(_) => {}
                    Err(e) => rep.notes.push(format!("commit: {e}")),
                }
            }
        } else if let Some(pl) = s.power_limit_w {
            // Only hwmon power cap available
            if let Some(cap_path) = self.hwmon.as_ref().map(|h| h.join("power1_cap")) {
                let uw = (pl * 1_000_000.0) as i64;
                if util::write_string(&cap_path.to_string_lossy(), &format!("{uw}\n")).is_ok() {
                    rep.power_limit = Some(pl);
                } else {
                    rep.notes.push("power1_cap write failed".into());
                }
            }
        }
        if s.thermal_limit_c.is_some() {
            rep.notes.push("thermal limit: managed by kernel amdgpu (temp*_crit)".into());
        }
        rep
    }

    fn reset(&mut self) -> AppliedReport {
        let mut rep = AppliedReport::default();
        if self.hwmon.is_some() {
            let h = self.hwmon.clone().unwrap();
            let _ = util::write_string(&h.join("pwm1_enable").to_string_lossy(), "2\n"); // auto
            rep.fan = (None, true);
        }
        let _ = self.pp_write("pp_od_clk_voltage", "r"); // r = reset levels (OD8)
        if self.pp_write("pp_od_clk_voltage", "c").is_ok() {
            rep.core_offset = Some(0);
            rep.mem_offset = Some(0);
        }
        // restore default cap
        if let Some(h) = &self.hwmon {
            let dflt = util::read_string(&h.join("power1_cap_default").to_string_lossy());
            if !dflt.is_empty() {
                let _ = util::write_string(&h.join("power1_cap").to_string_lossy(), &dflt);
            }
        }
        rep
    }
}

/// "SCLK: 0 MHz - 2260 MHz, Default: 2100 MHz" → bounds
fn bounds_from_line(t: &str) -> (Option<i32>, Option<i32>) {
    let orig = t.to_string();
    let (mn, mx) = limits_of(&orig);
    (mn.map(|v| v as i32), mx.map(|v| v as i32))
}

fn limits_of(t: &str) -> (Option<i32>, Option<i32>) {
    let nums: Vec<i32> = extract_numbers(t);
    if nums.len() >= 2 {
        (Some(nums[0]), Some(nums[1]))
    } else if nums.len() == 1 {
        (None, Some(nums[0]))
    } else {
        (None, None)
    }
}

fn levels_of(_t: &str, max: i32) -> (i32, i32) {
    (0, max) // level indices in most target formats
}

fn extract_numbers(t: &str) -> Vec<i32> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in t.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            if let Ok(n) = cur.parse::<i32>() {
                out.push(n);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() {
        if let Ok(n) = cur.parse::<i32>() {
            out.push(n);
        }
    }
    out
}
