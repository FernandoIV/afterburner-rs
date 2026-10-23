//! Hardware monitoring engine + history ring — the CMSIAfterburnerMonitoringData equivalent.

use crate::drivers::Sample;
use crate::mahm::{MahmEntry, MahmGpuEntry};
use crate::util;

#[derive(Debug, Clone)]
pub struct Poll {
    pub time: u64,
    pub gpu_index: usize,
    pub sample: Sample,
}

pub struct History {
    pub max: usize,
    pub stamps: Vec<u64>,
    pub series: Vec<Vec<Option<f32>>>, // per source; ring
    pub files: Vec<String>,
}

impl History {
    pub fn new(max: usize) -> Self {
        Self { max, stamps: vec![], series: vec![], files: vec![] }
    }

    pub fn extend_sources(&mut self, names: &[String]) {
        while self.series.len() < names.len() {
            self.series.push(Vec::with_capacity(self.max));
        }
        self.files = names.to_vec();
    }

    pub fn push(&mut self, values: &[Option<f32>], t: u64) {
        self.stamps.push(t);
        if self.stamps.len() > self.max {
            self.stamps.remove(0);
        }
        for (i, s) in self.series.iter_mut().enumerate() {
            let v = values.get(i).copied().flatten();
            s.push(v);
            if s.len() > self.max {
                s.remove(0);
            }
        }
    }
}

/// Build MAHM entries — same source naming and ordering as the
/// Windows monitoring surface (names matched from its string table).
pub fn entries_from_sample(
    _gpus: &[(String, String, String)], // gpu_id, device, driver
    idx: usize,
    s: &Sample,
    flags: u32,
) -> Vec<MahmEntry> {
    let mut v = Vec::with_capacity(12);
    let gpu = idx as u32;
    let mut e = |name: &str, units: &str, data: Option<f32>, min: f32, max: f32, id: u32| {
        v.push(MahmEntry {
            src_name: name.to_string(),
            src_units: units.to_string(),
            localized_src_name: name.to_string(),
            localized_src_units: units.to_string(),
            recommended_format: "%.1f".into(),
            data: data.unwrap_or(f32::MAX),
            min_limit: min,
            max_limit: max,
            flags,
            gpu: if id == crate::mahm::SID_FRAMERATE { gpu } else { gpu },
            src_id: id,
        })
    };
    e("GPU temperature", "°C", s.temp_core, 0.0, 125.0, crate::mahm::SID_GPU_TEMPERATURE);
    e("Memory temperature", "°C", s.temp_mem, 0.0, 125.0, crate::mahm::SID_MEM_TEMPERATURE);
    e("Fan speed", "%", s.fan_pct, 0.0, 100.0, crate::mahm::SID_FAN_SPEED);
    e("Fan tachometer", "RPM", s.fan_rpm, 0.0, 4000.0, crate::mahm::SID_FAN_TACHOMETER);
    e("Core clock", "MHz", s.clk_core, 0.0, 4000.0, crate::mahm::SID_CORE_CLOCK);
    e("Memory clock", "MHz", s.clk_mem, 0.0, 20000.0, crate::mahm::SID_MEMORY_CLOCK);
    e("GPU usage", "%", s.usage_gpu, 0.0, 100.0, crate::mahm::SID_GPU_USAGE);
    e("Memory usage", "%", s.usage_mem, 0.0, 100.0, crate::mahm::SID_MEMORY_USAGE);
    e("GPU voltage", "mv", s.voltage_mv, 0.0, 3000.0, crate::mahm::SID_GPU_VOLTAGE);
    e("Power", "W", s.power_w, 0.0, 2000.0, crate::mahm::SID_GPU_ABS_POWER);
    e("Power limit", "W", s.power_limit_w, 0.0, 2000.0, crate::mahm::SID_GPU_POWER_LIMIT);
    v
}

pub fn gpu_entries(gpus: &[&crate::gpu::GpuDevice]) -> Vec<MahmGpuEntry> {
    gpus.iter()
        .map(|g| MahmGpuEntry {
            gpu_id: g.gpu_id(),
            family: g.driver.clone(),
            device: g.name.clone().unwrap_or_else(|| format!("{:04X}:{:04X}", g.vendor_id, g.device_id)),
            driver: format!("driver {}", g.driver),
            bios: String::new(),
            mem_amount_kb: 0,
        })
        .collect()
}

/// Optional CSV hardware monitoring log — same spirit as Afterburner's
/// "EnableLog / LogPath" settings.
pub fn log_csv(path: &str, stamps: &[u64], series: &History) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "time,{}", series.files.join(","))?;
    let n = stamps.len();
    for i in 0..n {
        let vals: Vec<String> = series
            .series
            .iter()
            .map(|s| s.get(i).map(|v| format!("{:.1}", v.unwrap_or(f32::MAX))).unwrap_or_default())
            .collect();
        writeln!(f, "{},{}", stamps[i], vals.join(","))?;
    }
    Ok(())
}

/// CPU-side sources for global entries (CPU temp/usage, RAM usage).
pub fn cpu_snapshot() -> (Option<f32>, Option<f32>, Option<f32>) {
    // Temperature via coretemp package
    let mut temp = None;
    for n in 0..16 {
        let p = format!("/sys/class/hwmon/hwmon{}/name", n);
        if util::read_string(&p).trim() == "coretemp" {
            let dir = format!("/sys/class/hwmon/hwmon{}", n);
            let names = std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            let mut inputs: Vec<String> = names
                .iter()
                .filter(|f| f.ends_with("_input"))
                .cloned()
                .collect();
            inputs.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
            let mut vals: Vec<f32> = inputs
                .iter()
                .filter_map(|f| {
                    std::fs::read_to_string(format!("{dir}/{f}"))
                        .ok()
                        .and_then(|v| v.trim().parse::<f32>().ok())
                })
                .collect();
            if !vals.is_empty() {
                let max = vals.iter().copied().fold(f32::MIN, f32::max);
                temp = Some(max / 1000.0);
                break;
            }
            vals = vec![];
        }
    }
    // Usage from /proc/stat delta needs state; simple instant calc:
    let usage = if let Ok(stat) = util::read_to_string("/proc/stat") {
        let mut total = 0u64;
        let mut idle = 0u64;
        let mut first = true;
        for (i, line) in stat.lines().filter(|l| l.starts_with("cpu ")).take(1).enumerate() {
            let _ = i;
            let mut parts = line.split_whitespace();
            parts.next();
            let mut idleall = 0u64;
            let mut totalall = 0u64;
            for (j, f) in parts.enumerate() {
                if let Ok(v) = f.parse::<u64>() {
                    totalall += v;
                    if j == 3 || j == 4 {
                        idleall += v;
                    }
                }
            }
            total = totalall;
            idle = idleall;
            if !first {
                // fallback single-instant: not precise; keep simple
            }
            first = false;
        }
        if total > 0 {
            Some(100.0 - (idle as f32 / total as f32 * 100.0))
        } else {
            None
        }
    } else {
        None
    };
    let mem = read_meminfo();
    (temp, usage, mem)
}

fn read_meminfo() -> Option<f32> {
    if let Ok(t) = util::read_to_string("/proc/meminfo") {
        let mut total = 0u64;
        let mut avail = 0u64;
        for l in t.lines() {
            if l.starts_with("MemTotal") {
                total = l.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
            } else if l.starts_with("MemAvailable") {
                avail = l.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
            }
        }
        if total > 0 {
            return Some(100.0 - (avail as f32 / total as f32 * 100.0));
        }
    }
    None
}
