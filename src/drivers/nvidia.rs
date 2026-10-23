//! NVIDIA backend.
//!
//! Windows 4.6.6 routes through `CNVAPIWrapper` (nvapi.dll + nvpowerapi.dll,
//! see RTHAL.dll REA string inventory). The Linux-native equivalent is NVML
//! (loaded through dlopen so the program still runs on machines without it)
//! with a `nvidia-smi` CLI fallback for everything NVML cannot represent
//! (clock locking, clock offsets, power limits are also reachable there).

use super::{ApplySettings, AppliedReport, Caps, GpuDriver, Limits, Sample};
use crate::util;
use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, CString};
use std::process::Command;

const NVML_SUCCESS: i32 = 0;
type F = std::collections::HashMap<&'static str, usize>;

pub struct NvidiaDriver {
    pci_addr: String,
    // dlopen handle (raw pointer, never deref'd manually)
    lib: Option<*mut core::ffi::c_void>,
    index: Option<u32>,
    use_cli: bool,
    funcs: Option<&'static ()>, // marker so struct keeps static lifetime semantics
    smi_cache: Option<BTreeMap<String, String>>,
    smi_limit_cache: Option<Limits>,
    smi_offset_cache: Option<Limits>,
}

// SAFETY: libnvidia-ml handle is thread-local in practice; we keep driver
// single-threaded, and dlopen + dlsym pointers are only dereferenced as
// extern fn pointers inside `call_*` helpers.
unsafe impl Send for NvidiaDriver {}

extern "C" {
    fn dlopen(path: *const c_char, flag: c_int) -> *mut core::ffi::c_void;
    fn dlsym(handle: *mut core::ffi::c_void, sym: *const c_char) -> *mut core::ffi::c_void;
}

const RTLD_NOW: c_int = 2;

impl NvidiaDriver {
    pub fn new(pci_addr: String) -> Self {
        let mut s = Self {
            pci_addr,
            lib: None,
            index: None,
            use_cli: true,
            funcs: None,
            smi_cache: None,
            smi_limit_cache: None,
            smi_offset_cache: None,
        };
        unsafe {
            // libnvidia-ml.so.1 — loaded dynamically; if absent we fall back to nvidia-smi.
            let names = ["libnvidia-ml.so.1", "/usr/lib/x86_64-linux-gnu/nvidia/current/libnvidia-ml.so.1"];
            for n in names {
                let c = CString::new(n).unwrap();
                let h = dlopen(c.as_ptr(), RTLD_NOW);
                if !h.is_null() {
                    s.lib = Some(h);
                    s.use_cli = false;
                    s.attach_nvml(h);
                    break;
                }
            }
        }
        s
    }

    unsafe fn sym(&self, handle: *mut core::ffi::c_void, name: &str) -> Option<usize> {
        let c = CString::new(name).ok()?;
        let p = dlsym(handle, c.as_ptr());
        if p.is_null() { None } else { Some(p as usize) }
    }

    unsafe fn attach_nvml(&mut self, h: *mut core::ffi::c_void) {
        // nvmlInit_v2
        let mut ok = true;
        for n in ["nvmlInit_v2", "nvmlShutdown"] {
            if self.sym(h, n).is_none() {
                ok = false;
            }
        }
        if !ok {
            self.use_cli = true;
            return;
        }
        // Keep the map of entry points in a static registry keyed by handle.
        let mut map = F::new();
        for n in [
            "nvmlInit_v2", "nvmlShutdown", "nvmlDeviceGetHandleByPciBusId_v2",
            "nvmlDeviceGetCount_v2", "nvmlDeviceGetName",
            "nvmlDeviceGetTemperature", "nvmlDeviceGetFanSpeed",
            "nvmlDeviceGetUtilizationRates", "nvmlDeviceGetPowerUsage",
            "nvmlDeviceGetPowerManagementLimit", "nvmlDeviceGetEnforcedPowerLimit",
            "nvmlDeviceGetPowerManagementDefaultLimit", "nvmlDeviceGetPowerManagementLimitConstraints",
            "nvmlDeviceGetClockInfo", "nvmlDeviceGetMemClock",
            "nvmlDeviceSetPowerManagementLimit", "nvmlDeviceSetGpuLockedClocks",
            "nvmlDeviceResetGpuLockedClocks", "nvmlDeviceSetMemLockedClocks",
            "nvmlDeviceResetMemLockedClocks", "nvmlDeviceGpuClockOffset",
            "nvmlDeviceMemClockOffset", "nvmlDeviceGetFeeClk", "nvmlDeviceGetFieldValues",
        ] {
            if let Some(p) = self.sym(h, n) {
                map.insert(n, p);
            }
        }
        LUT.with(|m| *m.borrow_mut() = map);
        let init = LUT.with(|m| m.borrow().get("nvmlInit_v2").copied());
        if let Some(p) = init {
            let f: extern "C" fn() -> i32 = std::mem::transmute(p);
            let r = f();
            if r != NVML_SUCCESS {
                self.use_cli = true;
            }
        }
        self.lib = Some(h);
    }

    /// NVML device handle for our PCI address.
    fn device_handle(&mut self) -> Option<usize> {
        if self.use_cli {
            return None;
        }
        if let Some(_i) = self.index {
            unsafe {
                let sym = LUT.with(|m| m.borrow().get("nvmlDeviceGetHandleByPciBusId_v2").copied());
                if let Some(p) = sym {
                    let f: extern "C" fn(*const c_char, *mut usize) -> i32 =
                        std::mem::transmute(p);
                    let _bus = CString::new(format!("{}:{}.{}", self.pci_addr_resolved(3), self.pci_addr_resolved(4), self.pci_addr_resolved(5))).ok()?;
                    let mut h: usize = 0;
                    // PCI string form: 0000:01:00.0
                    let bus = CString::new(self.pci_addr_full()).ok()?;
                    let r = f(bus.as_ptr(), &mut h);
                    if r == NVML_SUCCESS {
                        return Some(h);
                    }
                }
            }
            self.use_cli = true; // fall back permanently
        }
        None
    }

    fn pci_addr_full(&self) -> String {
        format!("0000:{}", &self.pci_addr)
    }

    fn pci_addr_resolved(&self, part: usize) -> String {
        // 0001:23:45.6 → parts
        let full = format!("0000:{}", self.pci_addr);
        let segs: Vec<&str> = full.split(&[':', '.'][..]).collect();
        segs.get(part).copied().unwrap_or("").to_string()
    }

    fn ensure_index(&mut self) {
        if self.index.is_some() {
            return;
        }
        // enumerate via CLI to map busId->gpu index
        let out = Command::new("nvidia-smi")
            .args(["--list-gpus"])
            .output();
        if let Ok(o) = out {
            let txt = String::from_utf8_lossy(&o.stdout);
            for (i, line) in txt.lines().enumerate() {
                // "GPU 0: NVIDIA GeForce RTX 3070 Ti (PCI:1:0:0)"
                let t = line.trim();
                if t.starts_with("GPU") && t.contains(&self.pci_addr) {
                    self.index = Some(i as u32);
                    return;
                }
            }
        }
        // Fallback: parse old pci_bus_id query if available.
        let out = Command::new("nvidia-smi")
            .args(["--query-gpu=pci.bus_id", "--format=csv,noheader"])
            .output();
        if let Ok(o) = out {
            let txt = String::from_utf8_lossy(&o.stdout);
            for (i, line) in txt.lines().enumerate() {
                if line.trim().eq_ignore_ascii_case(&format!("00000000:{}", self.pci_addr)) {
                    self.index = Some(i as u32);
                    return;
                }
            }
        }
    }

    fn refresh_smi(&mut self) {
        if self.smi_cache.is_some() {
            return;
        }
        self.ensure_index();
        let idx = self.index.unwrap_or(0);
        let out = Command::new("nvidia-smi")
            .args([
                &format!("--id={idx}"),
                "--query-gpu=name,temperature.gpu,temperature.memory,utilization.gpu,utilization.memory,fan.speed,clocks.current.sm,clocks.current.memory,power.draw,power.limit,power.max_limit,power.min_limit,power.default_limit",
                "--format=csv,noheader,nounits",
            ])
            .output();
        if let Ok(o) = out {
            if !o.stdout.is_empty() {
                let txt = String::from_utf8_lossy(&o.stdout);
                let fields: Vec<String> = txt
                    .trim()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect();
                let keys = [
                    "name", "temp_core", "temp_mem", "usage_gpu", "usage_mem", "fan_speed",
                    "clk_core", "clk_mem", "power_w", "power_limit_w", "power_max_w", "power_min_w",
                    "power_default_w",
                ];
                let mut m = BTreeMap::new();
                for (k, v) in keys.iter().zip(fields.iter()) {
                    m.insert((*k).to_string(), v.clone());
                }
                self.smi_cache = Some(m);
                return;
            }
        }
        self.smi_cache = Some(BTreeMap::new());
    }

    fn f(&self, m: &BTreeMap<String, String>, k: &str) -> Option<f32> {
        let v = m.get(k)?;
        if v.contains('[') {
            // values like "N/A" or "[N/A]"
            return None;
        }
        parse_float(v)
    }

    fn smi_limits(&mut self) -> Limits {
        self.refresh_smi();
        let m = self.smi_cache.clone().unwrap_or_default();
        Limits {
            power_min_w: self.f(&m, "power_min_w"),
            power_max_w: self.f(&m, "power_max_w"),
            power_default_w: self.f(&m, "power_default_w"),
            clk_core_offset_min: Some(-2000),
            clk_core_offset_max: Some(2000),
            clk_mem_offset_min: Some(-5000),
            clk_mem_offset_max: Some(5000),
        }
    }
}

fn parse_float(s: &str) -> Option<f32> {
    if s.contains("N/A") || s.contains("[Not") {
        return None;
    }
    let t: String = s.chars().filter(|c| *c != '[' && *c != ']').collect();
    t.trim().replace("%", "").parse::<f32>().ok()
}

// -- NVML symbol registry (populated on attach) ------------------------------
thread_local! {
    static LUT: std::cell::RefCell<F> = std::cell::RefCell::new(F::new());
}

unsafe fn call0(name: &str) -> i32 {
    let p = LUT.with(|m| m.borrow().get(name).copied());
    match p {
        Some(p) => {
            let f: extern "C" fn() -> i32 = std::mem::transmute(p);
            f()
        }
        None => -1,
    }
}

impl GpuDriver for NvidiaDriver {
    fn sample(&mut self) -> Sample {
        self.refresh_smi();
        let m = self.smi_cache.clone().unwrap_or_default();
        Sample {
            temp_core: self.f(&m, "temp_core"),
            temp_mem: self.f(&m, "temp_mem"),
            fan_pct: self.f(&m, "fan_speed"),
            fan_rpm: self.f(&m, "fan_rpm"),
            clk_core: self.f(&m, "clk_core"),
            clk_mem: self.f(&m, "clk_mem"),
            usage_gpu: self.f(&m, "usage_gpu"),
            usage_mem: self.f(&m, "usage_mem"),
            power_w: self.f(&m, "power_w"),
            power_limit_w: self.f(&m, "power_limit_w"),
            voltage_mv: None,
        }
    }

    fn limits(&mut self) -> Limits {
        self.smi_limits()
    }

    fn caps(&mut self) -> Caps {
        Caps {
            core_clock_offset: true,   // locked/gco
            mem_clock_offset: true,    // -mco driver dependent
            power_limit: true,
            core_voltage: false,       // not on Linux
            fan: true,
            fan_auto: true,
            thermal_limit: false,
            vfs_curve: false,
        }
    }

    fn apply(&mut self, s: &ApplySettings) -> AppliedReport {
        self.ensure_index();
        let idx = self.index.unwrap_or(0);
        let mut rep = AppliedReport::default();
        if let Some(big) = s.lock_core_clock {
            let mut cmd = Command::new("nvidia-smi");
            cmd.args([&format!("--id={idx}"), "--lock-gpu-clocks", &format!("{},{}", big.0, big.1)]);
            push_result(&mut rep, run_smi(&mut cmd));
            rep.notes.push(format!("locked GPU clocks to {}-{} MHz", big.0, big.1));
        }
        if let Some(m) = s.lock_mem_clock {
            let mut cmd = Command::new("nvidia-smi");
            cmd.args([&format!("--id={idx}"), "--lock-memory-clocks", &format!("{},{}", m.0, m.1)]);
            push_result(&mut rep, run_smi(&mut cmd));
            rep.notes.push(format!("locked memory clocks to {}-{} MHz", m.0, m.1));
        }
        if let Some(pl) = s.power_limit_w {
            let mut cmd = Command::new("nvidia-smi");
            cmd.args([&format!("--id={idx}"), "--power-limit", &format!("{:.0}", pl)]);
            push_result(&mut rep, run_smi(&mut cmd));
        }
        if s.fan_auto {
            if let Some(msg) = nvidiasettings_reset_fan(&idx) {
                rep.notes.push(msg);
            } else {
                rep.notes.push("fan auto: no nvidia-settings available".into());
            }
        } else if let Some(p) = s.fan_pct {
            if let Some(msg) = nvidiasettings_set_fan(&idx, p) {
                rep.notes.push(msg);
            } else {
                rep.notes.push(format!("set fan: no nvidia-settings found (NVIDIA fan control on Linux needs Xorg/display tooling)"));
            }
        }
        if let Some(o) = s.core_offset_mhz {
            // Try the per-driver GPU clock offset control first; some
            // builds expose it, others (like 6xx series) only expose
            // locked clocks.
            let mut cmd = Command::new("nvidia-smi");
            cmd.args([&format!("--id={idx}"), "--gpu-clock-offset", &format!("{o}")]);
            let r = run_smi(&mut cmd);
            if r.1.to_lowercase().contains("invalid") || r.1.is_empty() && !r.0 {
                rep.notes.push(
                    "GPU clock offset control not exposed by this driver; use --lock-gpu-clocks instead".into(),
                );
            } else {
                push_result(&mut rep, r);
            }
        }
        if let Some(o) = s.mem_offset_mhz {
            let mut cmd = Command::new("nvidia-smi");
            cmd.args([&format!("--id={idx}"), "--mem-clock-offset", &format!("{o}")]);
            let r = run_smi(&mut cmd);
            if r.1.to_lowercase().contains("invalid") || r.1.is_empty() && !r.0 {
                rep.notes.push(
                    "Mem clock offset control not exposed by this driver; use --lock-memory-clocks instead".into(),
                );
            } else {
                push_result(&mut rep, r);
            }
        }
        self.smi_cache = None;
        rep
    }

    fn reset(&mut self) -> AppliedReport {
        self.ensure_index();
        let idx = self.index.unwrap_or(0);
        let mut rep = AppliedReport::default();
        let mut cmd = Command::new("nvidia-smi");
        cmd.args([&format!("--id={idx}"), "--reset-gpu-clocks"]);
        push_result(&mut rep, run_smi(&mut cmd));
        let mut cmd = Command::new("nvidia-smi");
        cmd.args([&format!("--id={idx}"), "--reset-mem-clocks"]);
        push_result(&mut rep, run_smi(&mut cmd));
        let mut cmd = Command::new("nvidia-smi");
        cmd.args([&format!("--id={idx}"), "--power-limit", "DEFAULT"]);
        push_result(&mut rep, run_smi(&mut cmd));
        if let Some(msg) = nvidiasettings_reset_fan(&idx) {
            rep.notes.push(msg);
        }
        self.smi_cache = None;
        rep
    }
}

fn run_smi(cmd: &mut Command) -> (bool, String) {
    match cmd.output() {
        Ok(o) => {
            let ok = o.status.success();
            let mut msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if msg.is_empty() {
                msg = String::from_utf8_lossy(&o.stdout).trim().to_string();
            }
            (ok, msg)
        }
        Err(e) => (false, format!("nvidia-smi spawn failed: {e}")),
    }
}

fn push_result(rep: &mut AppliedReport, r: (bool, String)) {
    let (ok, msg) = r;
    if msg.contains("deprecated") || msg.contains("was deprecated") {
        rep.notes.push("driver reports deprecated API; try locked clocks instead".into());
    } else if !ok {
        rep.notes.push(format!("error: {msg}"));
    }
}

fn nvidiasettings_set_fan(idx: &u32, pct: u32) -> Option<String> {
    let bin = util::read_to_string("/proc/self/maps").ok(); // touch to prove proc; not needed
    drop(bin);
    let mut cmd = Command::new("nvidia-settings");
    cmd.args([
        "-a",
        &format!("[fan:{idx}]/GPUTargetFanSpeed={pct}"),
    ]);
    match cmd.output() {
        Ok(o) if o.status.success() => Some("set fan via nvidia-settings".into()),
        Ok(_) => None,
        Err(_) => None,
    }
}

fn nvidiasettings_reset_fan(idx: &u32) -> Option<String> {
    let mut cmd = Command::new("nvidia-settings");
    cmd.args(["-a", &format!("[fan:{idx}]/FanControl=1")]);
    match cmd.output() {
        Ok(o) if o.status.success() => Some("fan auto via nvidia-settings".into()),
        Ok(_) => None,
        Err(_) => None,
    }
}
