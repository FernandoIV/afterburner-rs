//! NVIDIA backend.
//!
//! Windows 4.6.6 routes through `CNVAPIWrapper` (nvapi.dll + nvpowerapi.dll,
//! see RTHAL.dll REA string inventory). The Linux-native equivalent is NVML
//! (loaded through dlopen so the program still runs on machines without it)
//! with a `nvidia-smi` CLI fallback for everything NVML cannot represent
//! (clock locking, clock offsets, power limits are also reachable there).

use super::{ApplySettings, AppliedReport, Caps, GpuDriver, Limits, Sample};
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
        let out = Command::new("nvidia-smi")
            .args(["--query-gpu=index,pci.bus_id", "--format=csv,noheader"])
            .output();
        if let Ok(o) = out {
            let txt = String::from_utf8_lossy(&o.stdout);
            for line in txt.lines() {
                if let Some((index, pci)) = line.split_once(',') {
                    let pci = pci.trim();
                    let normalized = if pci.len() == 16 { &pci[4..] } else { pci };
                    if normalized.eq_ignore_ascii_case(&self.pci_addr) {
                        self.index = index.trim().parse().ok();
                        return;
                    }
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
        // Drivers live on the native UI worker now; never retain the first reading forever.
        self.smi_cache = None;
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
            let r = run_smi(&mut cmd);
            if r.0 { rep.notes.push(format!("Core clocks locked to {}–{} MHz", big.0, big.1)); }
            push_result(&mut rep, r);
        }
        if let Some(m) = s.lock_mem_clock {
            let mut cmd = Command::new("nvidia-smi");
            cmd.args([&format!("--id={idx}"), "--lock-memory-clocks", &format!("{},{}", m.0, m.1)]);
            let r = run_smi(&mut cmd);
            if r.0 { rep.notes.push(format!("Memory clocks locked to {}–{} MHz", m.0, m.1)); }
            push_result(&mut rep, r);
        }
        if let Some(pl) = s.power_limit_w {
            let mut cmd = Command::new("nvidia-smi");
            cmd.args([&format!("--id={idx}"), "--power-limit", &format!("{:.0}", pl)]);
            let r = run_smi(&mut cmd);
            if r.0 { rep.power_limit = Some(pl); rep.notes.push(format!("Power limit set to {pl:.0} W")); }
            push_result(&mut rep, r);
        }
        if s.fan_auto {
            match set_fan(&self.pci_addr, idx, None) {
                Ok(msg) => { rep.notes.push(msg); rep.fan = (None, true); }
                Err(msg) => rep.notes.push(format!("error: {msg}")),
            }
        } else if let Some(p) = s.fan_pct {
            match set_fan(&self.pci_addr, idx, Some(p)) {
                Ok(msg) => { rep.notes.push(msg); rep.fan = (Some(p), false); }
                Err(msg) => rep.notes.push(format!("error: {msg}")),
            }
        }
        if let Some(o) = s.core_offset_mhz {
            // Path Xorg (Coolbits>=8): GPUGraphicsClockOffset[<n>] por nivel de rendimiento.
            let done = nvsettings_offset_all(&idx, "GPUGraphicsClockOffset", o);
            if done > 0 {
                rep.notes.push(format!("Core offset {o:+} MHz applied through NV-CONTROL (Coolbits)"));
            } else {
                // path de auxilio: nvidia-smi (datacenter/some builds)
                let mut cmd = Command::new("nvidia-smi");
                cmd.args([&format!("--id={idx}"), "--gpu-clock-offset", &format!("{o}")]);
                let r = run_smi(&mut cmd);
                if r.1.to_lowercase().contains("invalid") || r.1.is_empty() && !r.0 {
                    rep.notes.push(
                        "Core offset requires driver support and may need Xorg + Coolbits (nvidia-settings); use --lock-gpu-clocks (nvidia-smi -lgc) as an alternative".into(),
                    );
                } else {
                    push_result(&mut rep, r);
                }
            }
        }
        if let Some(o) = s.mem_offset_mhz {
            let done = nvsettings_offset_all(&idx, "GPUMemoryTransferRateOffset", o);
            if done > 0 {
                rep.notes.push(format!("Memory offset {o:+} MHz applied through NV-CONTROL (Coolbits)"));
            } else {
                let mut cmd = Command::new("nvidia-smi");
                cmd.args([&format!("--id={idx}"), "--mem-clock-offset", &format!("{o}")]);
                let r = run_smi(&mut cmd);
                if r.1.to_lowercase().contains("invalid") || r.1.is_empty() && !r.0 {
                    rep.notes.push(
                        "Memory offset requires driver support and may need Xorg + Coolbits (nvidia-settings); use --lock-memory-clocks (nvidia-smi -lmc) as an alternative".into(),
                    );
                } else {
                    push_result(&mut rep, r);
                }
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
        if let Some(default) = self.smi_limits().power_default_w {
            let mut cmd = Command::new("nvidia-smi");
            cmd.args([&format!("--id={idx}"), "--power-limit", &format!("{default:.0}")]);
            push_result(&mut rep, run_smi(&mut cmd));
        }
        nvsettings_offset_all(&idx, "GPUGraphicsClockOffset", 0);
        nvsettings_offset_all(&idx, "GPUMemoryTransferRateOffset", 0);
        match set_fan(&self.pci_addr, idx, None) {
            Ok(msg) => rep.notes.push(msg),
            Err(msg) => rep.notes.push(format!("error: {msg}")),
        }
        self.smi_cache = None;
        rep
    }
}

/// Aplica un offset a todos los niveles de rendimiento vía NV-CONTROL.
/// Devuelve cuántos índices aceptaron la asignación (0 = none).
fn nvsettings_offset_all(idx: &u32, attr: &str, delta_mhz: i32) -> u32 {
    if !Command::new("which")
        .arg("nvidia-settings")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return 0;
    }
    let mut done = 0;
    // One assignment to all exposed performance levels is preferable to guessing 8 levels.
    let all = format!("[gpu:{idx}]/{attr}AllPerformanceLevels={delta_mhz}");
    if let Ok(o) = Command::new("nvidia-settings").args(["-a", &all]).output() {
        if o.status.success() && !String::from_utf8_lossy(&o.stderr).contains("ERROR") { return 1; }
    }
    for lvl in 0..5i32 {
        let mut cmd = Command::new("nvidia-settings");
        cmd.args([
            "-a",
            &format!("[gpu:{idx}]/{attr}[{lvl}]={delta_mhz}"),
        ]);
        if let Ok(o) = cmd.output() {
            if o.status.success() && !String::from_utf8_lossy(&o.stderr).contains("ERROR") {
                done += 1;
            }
        }
    }
    done
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

fn set_fan(pci: &str, idx: u32, pct: Option<u32>) -> Result<String, String> {
    match super::nvfan::set(pci, pct) {
        Ok(msg) => return Ok(msg),
        Err(e) if e.code == 4 => return crate::fan_service::request(pci, pct),
        Err(e) if e.code != 3 && e.code != 13 && e.code != -1 => return Err(e.message),
        Err(_) => {} // older driver: try NV-CONTROL, if available
    }
    let assign = |a: &str| -> Result<(), String> {
        let o = Command::new("nvidia-settings").args(["-a", a]).output().map_err(|e| e.to_string())?;
        let err = String::from_utf8_lossy(&o.stderr);
        if !o.status.success() || err.contains("ERROR") { Err(err.trim().to_string()) } else { Ok(()) }
    };
    assign(&format!("[gpu:{idx}]/GPUFanControlState={}", if pct.is_some() {1} else {0}))?;
    if let Some(p) = pct {
        if p > 100 { return Err("Fan speed out of range".into()); }
        let o = Command::new("nvidia-settings").args(["-q", &format!("[gpu:{idx}]/fans"), "-t"])
            .output().map_err(|e|e.to_string())?;
        let fans: Vec<u32> = String::from_utf8_lossy(&o.stdout).split(|c:char| !c.is_ascii_digit())
            .filter_map(|n|n.parse().ok()).collect();
        if fans.is_empty() { return Err("The driver does not expose fans for this GPU".into()); }
        for fan in fans { assign(&format!("[fan:{fan}]/GPUTargetFanSpeed={p}"))?; }
    }
    Ok("Fan control applied through NV-CONTROL".into())
}
