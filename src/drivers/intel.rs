//! Intel i915/xe backend.
//!
//! Windows 4.6.6 uses the Intel IGCL `ctl*` API for Arc GPUs
//! (see RTHAL.dll: ctlEnumerateDevices, ctlFanGetConfig, ctlFrequency*,
//! ctlGetPowerOptimizationCaps...). On Linux the hardware already exposes
//! the monitoring surface through drm hwmon; fan/frequency control from
//! sysfs is limited to what the kernel driver authorises.

use super::{ApplySettings, AppliedReport, Caps, GpuDriver, Limits, Sample};
use crate::util;
use std::path::PathBuf;

pub struct IntelDriver {
    hwmon: Option<PathBuf>,
}

impl IntelDriver {
    pub fn new(hwmon: Option<PathBuf>) -> Self {
        Self { hwmon }
    }

    fn f(&self, name: &str) -> Option<f32> {
        let h = self.hwmon.as_ref()?;
        util::read_string(&h.join(name).to_string_lossy())
            .trim()
            .parse::<f32>()
            .ok()
    }

    fn attr(&self, prefix: &str) -> Option<f32> {
        let h = self.hwmon.as_ref()?;
        let rd = std::fs::read_dir(h).ok()?;
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for n in names {
            if n.starts_with(prefix) && !n.ends_with("_label") {
                if let Some(v) = self.f(&n) {
                    return Some(v);
                }
            }
        }
        None
    }
}

impl GpuDriver for IntelDriver {
    fn sample(&mut self) -> Sample {
        Sample {
            temp_core: self.attr("temp1_input").map(|v| v / 1000.0),
            temp_mem: None,
            fan_pct: None,
            fan_rpm: self.attr("fan1_input"),
            clk_core: self.attr("freq1_input").map(|v| v / 1_000_000.0),
            clk_mem: None,
            usage_gpu: None,
            usage_mem: None,
            power_w: self.attr("power1_average").map(|v| v / 1_000_000.0)
                .or_else(|| self.attr("power1_input").map(|v| v / 1_000_000.0)),
            power_limit_w: self.attr("power1_rated_max").map(|v| v / 1_000_000.0)
                .or_else(|| self.attr("power1_max").map(|v| v / 1_000_000.0)),
            voltage_mv: None,
        }
    }

    fn limits(&mut self) -> Limits {
        Limits::default()
    }

    fn caps(&mut self) -> Caps {
        Caps {
            core_clock_offset: false,
            mem_clock_offset: false,
            power_limit: false,
            core_voltage: false,
            fan: false,
            fan_auto: false,
            thermal_limit: false,
            vfs_curve: false,
        }
    }

    fn apply(&mut self, s: &ApplySettings) -> AppliedReport {
        let mut rep = AppliedReport::default();
        if s.power_limit_w.is_some() || s.core_offset_mhz.is_some() || s.mem_offset_mhz.is_some() {
            rep.notes
                .push("intel i915/xe: sysfs exposes no OD controls (IGCL is Windows-only in 4.6.6)".into());
        }
        rep
    }

    fn reset(&mut self) -> AppliedReport {
        AppliedReport::default()
    }
}
