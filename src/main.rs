//! afterburner-rs — native Linux port of the MSI Afterburner behaviour
//! surface (monitoring / clocks / power / fan curves / profiles /
//! MAHM+MACM compatible shared memory), built from the analysis of the
//! shipped 4.6.6 distribution.

mod api;
mod config;
mod gui;
mod drivers;
mod fancurve;
mod fan_service;
mod gpu;
mod macm;
mod mahm;
mod monitor;
mod tui;
mod util;

use drivers::{ApplySettings, Caps, GpuDriver, Limits, Sample};
use gpu::{GpuDevice, Vendor};
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn eprint_help() {
    println!("afterburner-rs {} — MSI Afterburner 4.6.6 port for Linux (analysis-derived)", env!("CARGO_PKG_VERSION"));
    println!();
    println!("USAGE:");
    println!("  afterburner-rs list                       inventory of GPUs (MAHM-format IDs)");
    println!("  afterburner-rs info                       per-GPU caps, limits and current values");
    println!("  afterburner-rs monitor [--csv [path]] [--period MS] [--iterations N]");
    println!("  afterburner-rs tui [--frames N] [--plain]  live dashboard (frames limits run length; plain for scripts)");
    println!("  afterburner-rs set [options]              apply settings to a GPU");
    println!("       --gpu IDX | --all");
    println!("       --core-offset MHZ  --mem-offset MHZ");
    println!("       --power W          --power-pct %");
    println!("       --fan PCT          --fan-auto");
    println!("  afterburner-rs reset [--gpu IDX | --all]");
    println!("  afterburner-rs fan-curve TEMP:PERCENT,...  install software fan curve (daemon required)");
    println!("  afterburner-rs profile list|save N|load N|show N");
    println!("  afterburner-rs daemon                     monitoring + fan curve + MACM control + GUI/API server");
    println!("  afterburner-rs gui                        native GTK4 desktop interface (no browser)");
    println!("  afterburner-rs mahm-dump                  read /dev/shm/MAHMSharedMemory as a client would");
    println!("  afterburner-rs macm-dump                  read /dev/shm/MACMSharedMemory as a client would");
    println!();
    println!("Decision data: 4.6.6 installer analysis (MAHM/MACM headers, RTHAL backends: NVAPI,");
    println!("ATIO Overdrive5/6/N/8, IGCL).*p The Linux equivalent routes through NVML/nvidia-smi,");
    println!("amdgpu pp_od_clk_voltage/pp_power_profile_mode and drm hwmon.");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprint_help();
        return;
    }
    let cmd = args[0].as_str();
    let rest = &args[1..];
    let code = match cmd {
        "list" => cmd_list(),
        "info" => cmd_info(rest),
        "monitor" => cmd_monitor(rest),
        "tui" => cmd_tui(rest),
        "set" => cmd_set(rest),
        "reset" => cmd_reset(rest),
        "fan-curve" => cmd_fan_curve(rest),
        "profile" => cmd_profile(rest),
        "daemon" => cmd_daemon(rest),
        "mahm-dump" => cmd_mahm_dump(),
        "macm-dump" => cmd_macm_dump(),
        "gui" => gui::run(&rest),
        "fan-service" => fan_service::run(rest),
        "help" | "--help" | "-h" => {
            eprint_help();
            0
        }
        other => {
            println!("unknown command: {other}");
            eprint_help();
            2
        }
    };
    std::process::exit(code);
}

/// Build the driver trait object per GPU.
pub(crate) fn driver_for(g: &GpuDevice) -> Box<dyn GpuDriver> {
    match g.vendor {
        Vendor::Nvidia => Box::new(drivers::nvidia::NvidiaDriver::new(g.pci_addr.clone())),
        Vendor::Amd => Box::new(drivers::amd::AmdDriver::new(
            std::path::PathBuf::from(format!("/sys/bus/pci/devices/{}", g.pci_addr)),
            g.hwmon.clone(),
        )),
        Vendor::Intel => Box::new(drivers::intel::IntelDriver::new(g.hwmon.clone())),
        Vendor::Other(_) => unreachable!(),
    }
}

fn cmd_list() -> i32 {
    let mut gpus = gpu::discover_gpus();
    let map = nvsmi_names();
    gpu::attach_display_names(&mut gpus, &map);
    println!("Found {} GPU(s):", gpus.len());
    for (i, g) in gpus.iter().enumerate() {
        let disp = g.name.clone().unwrap_or_else(|| match g.vendor {
            Vendor::Nvidia => "NVIDIA (unnamed)".into(),
            Vendor::Amd => "AMD (PCI)".into(),
            Vendor::Intel => "Intel Graphics".into(),
            Vendor::Other(_) => "GPU".into(),
        });
        println!(
            "  GPU{i}: {} [{:?}]\n        PCI {}\n        backend {}\n        card {}  driver {}\n        ID {}",
            disp,
            g.vendor,
            g.pci_addr,
            match g.vendor {
                Vendor::Nvidia => "nvml/nvidia-smi (CNVAPIWrapper equiv.)",
                Vendor::Amd => "amdgpu pp_od (CATIADLWrapper equiv.)",
                Vendor::Intel => "drm hwmon (IGCL equiv.)",
                Vendor::Other(_) => "sysfs",
            },
            g.card,
            g.driver,
            g.gpu_id()
        );
    }
    0
}

pub(crate) fn nvsmi_names() -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let o = Command::new("nvidia-smi")
        .args(["--query-gpu=pci.bus_id,name", "--format=csv,noheader"])
        .output();
    if let Ok(o) = o {
        if o.status.success() && !o.stdout.is_empty() {
            let t = String::from_utf8_lossy(&o.stdout);
            for line in t.lines() {
                let p: Vec<&str> = line.splitn(2, ',').map(|s| s.trim()).collect();
                if p.len() == 2 {
                    out.insert(p[0].to_uppercase(), p[1].to_string());
                    continue;
                }
            }
        }
        // older field spellings
        let o2 = Command::new("nvidia-smi")
            .args(["--query-gpu=pci.bus_id,name", "--format=csv,noheader"])
            .output();
        if let Ok(o2) = o2 {
            let t = String::from_utf8_lossy(&o2.stdout);
            for line in t.lines() {
                let p: Vec<&str> = line.splitn(2, ',').map(|s| s.trim()).collect();
                if p.len() == 2 {
                    out.insert(p[0].to_uppercase(), p[1].to_string());
                }
            }
        }
    }
    out
}

fn pick(gulp: &[usize], total: usize) -> Vec<usize> {
    if total == 0 {
        return vec![];
    }
    if gulp.is_empty() {
        return (0..total).collect();
    }
    gulp.iter().copied().filter(|&i| i < total).collect()
}

fn parse_gpu_list(args: &[String]) -> Vec<usize> {
    let mut v = vec![];
    for (i, a) in args.iter().enumerate() {
        if a == "--gpu" {
            if let Some(n) = args.get(i + 1).and_then(|x| x.parse::<usize>().ok()) {
                v.push(n);
            }
        }
    }
    v
}

fn cmd_info(args: &[String]) -> i32 {
    let gpus = gpu::discover_gpus();
    let sel = parse_gpu_list(args);
    let idxs = pick(&sel, gpus.len());
    for gi in idxs {
        let g = gpus[gi].clone();
        let mut d = driver_for(&g);
        let s = d.sample();
        let l = d.limits();
        let c = d.caps();
        println!("GPU{gi} [{}] {}", g.vendor_name(), g.gpu_id());
        println!("  driver : {}  sysfs hwmon {}", g.driver, g.hwmon.clone().map(|p| format!("{p:?}")).unwrap_or_default());
        println!("  caps   : {}", fmt_caps(&c));
        println!("  limits : {}", fmt_limits(&l));
        println!("  now    : {}", fmt_sample(&s));
        println!();
    }
    0
}

fn fmt_caps(c: &Caps) -> String {
    let mut v = String::new();
    if c.core_clock_offset {
        v.push_str("core-clock-offset ");
    }
    if c.mem_clock_offset {
        v.push_str("mem-clock-offset ");
    }
    if c.power_limit {
        v.push_str("power ");
    }
    if c.fan {
        v.push_str("fan ");
    }
    if c.fan_auto {
        v.push_str("fan-auto ");
    }
    if c.vfs_curve {
        v.push_str("vf-curve ");
    }
    v.trim().to_string()
}

fn fmt_limits(l: &Limits) -> String {
    let mut parts = vec![];
    if let (Some(a), Some(b)) = (l.power_min_w, l.power_max_w) {
        parts.push(format!("power {a:.0}–{b:.0}W"));
        if let Some(d) = l.power_default_w {
            parts.push(format!("default {d:.0}W"));
        }
    }
    if let (Some(a), Some(b)) = (l.clk_core_offset_min, l.clk_core_offset_max) {
        parts.push(format!("core offset {a}..{b}MHz"));
    }
    if let (Some(a), Some(b)) = (l.clk_mem_offset_min, l.clk_mem_offset_max) {
        parts.push(format!("mem offset {a}..{b}MHz"));
    }
    parts.join(" | ")
}

fn fmt_sample(s: &Sample) -> String {
    fn or(v: Option<f32>) -> String {
        match v {
            Some(x) => format!("{x:.0}"),
            None => "—".into(),
        }
    }
    format!(
        "T {}°C {}% {}rpm | clk {}/{} MHz | load {}% | {}W (limit {})",
        or(s.temp_core),
        or(s.fan_pct),
        or(s.fan_rpm),
        or(s.clk_core),
        or(s.clk_mem),
        or(s.usage_gpu),
        or(s.power_w),
        or(s.power_limit_w)
    )
}

fn cmd_monitor(args: &[String]) -> i32 {
    let mut csv: Option<String> = None;
    let mut period = 1000u64;
    let mut iters: u64 = 0;
    let mut sel = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--csv" => {
                csv = args
                    .get(i + 1)
                    .filter(|a| !a.starts_with("--"))
                    .cloned()
                    .or_else(|| Some(String::from("afterburner-rs-log.csv")));
                i += 1;
            }
            "--period" => {
                if let Some(n) = args.get(i + 1).and_then(|v| v.parse::<u64>().ok()) {
                    period = n;
                    i += 1;
                }
            }
            "--iterations" => {
                if let Some(n) = args.get(i + 1).and_then(|v| v.parse::<u64>().ok()) {
                    iters = n;
                    i += 1;
                }
            }
            "--gpu" => sel.push(args.get(i + 1).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0)),
            _ => {}
        }
        i += 1;
    }
    let gpus = gpu::discover_gpus();
    let idxs = pick(&sel, gpus.len());
    let mut headers: Vec<String> = vec![];
    let mut hist = monitor::History::new(4096);
    let mut out = std::io::stdout();
    let mut n = 0u64;
    loop {
        for (k, gi) in idxs.iter().enumerate() {
            let g = gpus[*gi].clone();
            let mut d = driver_for(&g);
            let s = d.sample();
            if n == 0 {
                let mut names: Vec<String> = [
                    format!("GPU{} temp", k),
                    format!("GPU{} fan%", k),
                    format!("GPU{} core", k),
                    format!("GPU{} mem", k),
                    format!("GPU{} usage", k),
                    format!("GPU{} power", k),
                ]
                .to_vec();
                headers.append(&mut names);
            }
            let vals = vec![
                s.temp_core,
                s.fan_pct,
                s.clk_core,
                s.clk_mem,
                s.usage_gpu,
                s.power_w,
            ];
            if csv.is_none() {
                let line = format!(
                    "{}\t{}", util::now_secs(),
                    vals.iter().map(|v| v.map(|x| format!("{x:.0}")).unwrap_or_else(|| "-".into())).collect::<Vec<_>>().join("\t")
                );
                writeln!(out, "{line}").ok();
            } else {
                hist.extend_sources(&headers);
                hist.push(&vals, util::now_secs());
            }
        }
        n += 1;
        if csv.is_none() {
            out.flush().ok();
        }
        if iters != 0 && n >= iters {
            break;
        }
        if csv.is_some() {
            simple_sleep(period);
        } else {
            simple_sleep(period);
        }
    }
    if let Some(path) = csv {
        let _ = monitor::log_csv(&path, &hist.stamps, &hist);
        println!("csv saved to {path}");
    }
    0
}

fn simple_sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn cmd_tui(args: &[String]) -> i32 {
    let mut frames: u64 = 0; // 0 = infinite
    let mut plain = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--frames" {
            frames = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0);
            i += 1;
        } else if args[i] == "--plain" {
            plain = true;
        }
        i += 1;
    }
    let gpus = gpu::discover_gpus();
    let mut gpus = gpus;
    let names = nvsmi_names();
    gpu::attach_display_names(&mut gpus, &names);
    let mut set: Vec<Box<dyn GpuDriver>> = gpus.iter().map(|g| driver_for(g)).collect();
    if !plain {
        tui::enter_alt();
    }
    let mut iter = 0u64;
    loop {
        if !plain {
            tui::clear();
        }
        println!("afterburner-rs — hardware monitor (Ctrl-C to exit)");
        println!();
        for (k, d) in set.iter_mut().enumerate() {
            let g = gpus[k].clone();
            let s = d.sample();
            println!(
                " GPU{k} {} [{}]",
                g.name.clone().unwrap_or_else(|| g.pci_addr.clone()),
                g.vendor_name()
            );
            let tcol = if plain {
                String::new()
            } else {
                tui::temps_color(s.temp_core.unwrap_or(0.0)).to_string()
            };
            println!(
                "        {}{}°C{}  fan {}%\n        clk {} MHz | mem {} MHz | load {}% | {} W (limit {} W)",
                tcol,
                or0(s.temp_core),
                if tcol.is_empty() { "" } else { "\x1b[0m" },
                or0(s.fan_pct),
                or0(s.clk_core),
                or0(s.clk_mem),
                or0(s.usage_gpu),
                or0(s.power_w),
                or0(s.power_limit_w)
            );
            println!("        temp {}", tui::bar(s.temp_core, 100.0, 24));
            println!("        fan  {}", tui::bar(s.fan_pct, 100.0, 24));
            println!("        load {}", tui::bar(s.usage_gpu, 100.0, 24));
            println!("        power{}", tui::bar(s.power_w, s.power_limit_w.unwrap_or(400.0), 24));
        }
        iter += 1;
        if frames != 0 && iter >= frames {
            break;
        }
        tui::sleep_ms(1000);
    }
    if !plain {
        tui::leave_alt();
    }
    0
}

fn or0(v: Option<f32>) -> String {
    v.map(|x| format!("{x:.0}")).unwrap_or_else(|| "-".into())
}

fn parse_pair(s: &str) -> Option<(u32, u32)> {
    let p: Vec<&str> = s.split(',').collect();
    if p.len() == 2 {
        if let (Ok(a), Ok(b)) = (p[0].trim().parse(), p[1].trim().parse()) {
            return Some((a, b));
        }
    }
    None
}

fn cmd_set(args: &[String]) -> i32 {
    let mut core: Option<i32> = None;
    let mut mem: Option<i32> = None;
    let mut power_w: Option<f32> = None;
    let mut power_pct: Option<f32> = None;
    let mut fan: Option<u32> = None;
    let mut fan_auto = false;
    let mut sel: Vec<usize> = vec![];
    let mut lock_core: Option<(u32, u32)> = None;
    let mut lock_mem: Option<(u32, u32)> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--core-offset" => core = args.get(i + 1).and_then(|v| v.parse().ok()),
            "--mem-offset" => mem = args.get(i + 1).and_then(|v| v.parse().ok()),
            "--lock-gpu-clocks" => {
                lock_core = args.get(i + 1).and_then(|v| parse_pair(v));
            }
            "--lock-memory-clocks" => {
                lock_mem = args.get(i + 1).and_then(|v| parse_pair(v));
            }
            "--power" => power_w = args.get(i + 1).and_then(|v| v.parse().ok()),
            "--power-pct" => power_pct = args.get(i + 1).and_then(|v| v.parse().ok()),
            "--fan" | "--fan-speed" => fan = args.get(i + 1).and_then(|v| v.parse().ok()),
            "--fan-auto" | "--fan-reset" => fan_auto = true,
            _ => {}
        }
        if args[i] == "--gpu" {
            sel.push(args.get(i + 1).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0));
        }
        i += 1;
    }
    let gpus = gpu::discover_gpus();
    let idxs = pick(&sel, gpus.len());
    if idxs.is_empty() {
        println!("no --gpu index given and no GPUs found");
        return 1;
    }
    for gi in idxs {
        let g = gpus[gi].clone();
        let mut d = driver_for(&g);
        let mut set = ApplySettings {
            core_offset_mhz: core,
            mem_offset_mhz: mem,
            lock_core_clock: lock_core,
            lock_mem_clock: lock_mem,
            power_limit_w: power_w,
            fan_pct: fan,
            fan_auto,
            thermal_limit_c: None,
        };
        if let Some(p) = power_pct {
            let l = d.limits();
            let base = l.power_default_w.unwrap_or(l.power_max_w.unwrap_or(0.0));
            if base > 0.0 {
                set.power_limit_w = Some(base * p / 100.0);
            }
        }
        let rep = d.apply(&set);
        println!("GPU{gi}: applied.");
        for n in &rep.notes {
            println!("  note: {n}");
        }
        let s = d.sample();
        println!("  now: {}", fmt_sample(&s));
    }
    0
}

fn cmd_reset(args: &[String]) -> i32 {
    let sel = parse_gpu_list(args);
    let gpus = gpu::discover_gpus();
    let idxs = pick(&sel, gpus.len());
    for gi in idxs {
        let g = gpus[gi].clone();
        let mut d = driver_for(&g);
        let rep = d.reset();
        println!("GPU{gi}: reset.");
        for n in &rep.notes {
            println!("  note: {n}");
        }
    }
    0
}

/// Fan curve requested out-of-process: hand it to the running daemon
/// through the MACM control file (independent of Windows semantics but
/// same shape: command + gpu entry bitmap + value).
fn cmd_fan_curve(args: &[String]) -> i32 {
    if args.is_empty() {
        println!("usage: afterburner-rs fan-curve 40:30,55:55,70:85,85:100");
        return 2;
    }
    let txt = args[0].clone();
    let mut nodes = vec![];
    for part in txt.split(',') {
        let ab: Vec<&str> = part.split(':').collect();
        if ab.len() == 2 {
            if let (Ok(t), Ok(d)) = (ab[0].trim().parse::<f32>(), ab[1].trim().parse::<f32>()) {
                nodes.push((t, d));
            }
        }
    }
    if nodes.is_empty() {
        println!("could not parse nodes");
        return 1;
    }
    let dir = config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut ini = if let Ok(t) = std::fs::read_to_string(dir.join("MSIAfterburner.cfg")) {
        config::Ini::parse(&t)
    } else {
        config::Ini::default()
    };
    ini.set("Settings", "SoftwareFanCurve", &config::format_nodes(&nodes));
    ini.set("Settings", "SoftwareFanCurveEnabled", "1");
    let _ = std::fs::write(dir.join("MSIAfterburner.cfg"), ini.to_text());
    // nudge the daemon: write MACM command
    write_macm_nudge();
    println!("fan curve stored; daemon picks it up on next poll");
    0
}

fn write_macm_nudge() {
    let _ = std::fs::write(macm::SHM_ACCESS_NAME, b"ABRS");
}

fn cmd_profile(args: &[String]) -> i32 {
    if args.is_empty() {
        println!("usage: profile list | save N | load N | show N");
        return 2;
    }
    match args[0].as_str() {
        "list" => {
            let d = config::profiles_dir();
            let mut found = false;
            if let Ok(rd) = std::fs::read_dir(&d) {
                let mut names: Vec<String> = rd
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.ends_with(".cfg"))
                    .collect();
                names.sort();
                for n in names {
                    found = true;
                    println!("  {n}");
                }
            }
            if !found {
                println!("  (no profiles yet — create with: profile save 1)");
            }
        }
        "show" => {
            let n = args.get(1).and_then(|x| x.parse::<u32>().ok()).unwrap_or(1);
            match config::load_profile(n) {
                Some(v) => {
                    println!("Profile{n} ({}):", v.name);
                    if let Some(x) = v.core_clock_offset {
                        println!("  CoreClockBoost   = {x}");
                    }
                    if let Some(x) = v.mem_clock_offset {
                        println!("  MemoryClockBoost = {x}");
                    }
                    if let Some(x) = v.power_limit_w {
                        println!("  PowerLimitW      = {x:.1}");
                    }
                    if let Some(x) = v.power_limit_pct {
                        println!("  PowerLimit%      = {x:.1}");
                    }
                    if v.fan_auto {
                        println!("  FanSpeed         = AUTO");
                    } else if let Some(x) = v.fan_pct {
                        println!("  FanSpeed         = {x}");
                    }
                    if !v.fan_curve_nodes.is_empty() {
                        println!("  SoftwareFanCurve = {}", config::format_nodes(&v.fan_curve_nodes));
                    }
                }
                None => println!("Profile{n} not found"),
            }
        }
        "save" => {
            let n = args.get(1).and_then(|x| x.parse::<u32>().ok()).unwrap_or(1);
            let mut v = config::ProfileValues::default();
            // read current applied state via info
            let gpus = gpu::discover_gpus();
            if let Some(g) = gpus.first() {
                let mut d = driver_for(g);
                let _ = d.limits();
                let s = d.sample();
                v.fan_pct = s.fan_pct.map(|x| x as u32);
            }
            let p = config::save_profile(n, &v, &Limits::default());
            println!("saved {} -> {:?}", n, p.map(|x| x.file_name().unwrap_or_default().to_string_lossy().to_string()));
        }
        "load" => {
            let n = args.get(1).and_then(|x| x.parse::<u32>().ok()).unwrap_or(1);
            match config::load_profile(n) {
                Some(v) => {
                    let gpus = gpu::discover_gpus();
                    if let Some(g) = gpus.first() {
                        let mut d = driver_for(g);
                        let rep = d.apply(&ApplySettings {
                            core_offset_mhz: v.core_clock_offset,
                            mem_offset_mhz: v.mem_clock_offset,
                            lock_core_clock: None,
                            lock_mem_clock: None,
                            power_limit_w: v.power_limit_w,
                            fan_pct: v.fan_pct,
                            fan_auto: v.fan_auto,
                            thermal_limit_c: None,
                        });
                        println!("Profile{n} applied to GPU0.");
                        for x in rep.notes {
                            println!("  note: {x}");
                        }
                        let s = d.sample();
                        println!("  now: {}", fmt_sample(&s));
                    }
                }
                None => println!("Profile{n} not found"),
            }
        }
        other => {
            println!("unknown subcommand {other}");
            return 2;
        }
    }
    0
}

/// Daemon: poll loop publishing MAHM memory, driving the software fan
/// curve, serving MACM control commands and the GUI HTTP server.
fn cmd_daemon(args: &[String]) -> i32 {
    let mut period = 1000u64;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--period" {
            period = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1000);
            i += 1;
        }
        i += 1;
    }
    let mut gpus = gpu::discover_gpus();
    gpu::attach_display_names(&mut gpus, &nvsmi_names());
    if gpus.is_empty() {
        println!("No GPUs found. Make sure the GPU drivers expose /sys/class/drm.");
        return 1;
    }

    // -- settings
    let settings = if let Ok(t) = std::fs::read_to_string(config::settings_path()) {
        config::Ini::parse(&t)
    } else {
        config::Ini::default()
    };
    let server_on = settings
        .get_i64("Settings", "EnableServer")
        .unwrap_or(0)
        != 0;
    let mut log_path = {
        let en = settings.get_i64("Settings", "EnableLog").unwrap_or(0) == 1;
        match settings.get("Settings", "LogPath") {
            Some(p) if !p.is_empty() && en => Some(p.to_string()),
            Some("/dev/stderr") if en => None,
            _ if en => Some(config::config_dir().join("hardware-log.csv").to_string_lossy().to_string()),
            _ => None,
        }
    };

    // -- shared GUI/API state
    let curve_arc: Arc<Mutex<fancurve::FanCurveCfg>> = Arc::new(Mutex::new(fancy_default_curve(&settings)));
    let curve_on: Arc<AtomicBool> = Arc::new(AtomicBool::new(
        settings
            .get_i64("Settings", "SoftwareFanCurveEnabled")
            .unwrap_or(0)
            == 1,
    ));
    let samples_arc: Arc<Mutex<Vec<api::SampleData>>> = Arc::new(Mutex::new(vec![]));
    let state = Arc::new(api::ApiState {
        samples: Arc::clone(&samples_arc),
        curve: Arc::clone(&curve_arc),
        curve_enabled: Arc::clone(&curve_on),
        annotate: env!("CARGO_PKG_VERSION").to_string(),
    });
    let port = api::port_from_settings(&settings);
    if server_on {
        match api::start_server(port, Arc::clone(&state)) {
            Ok(_) => println!("Optional web compatibility server on http://127.0.0.1:{port}"),
            Err(e) => eprintln!("API server unavailable on {port}: {e}"),
        }
    }

    let mut curve_cfg = fancy_default_curve(&settings);
    let mut fanctl = fancurve::FanController::new(curve_cfg.clone());
    let mut last_cfg_hash = String::new();
    let mut limits_cache: Vec<drivers::Limits> = vec![];
    let mut caps_cache: Vec<drivers::Caps> = vec![];
    let mut iter = 0u64;

    let mut curve_gpu = settings.get_i64("Settings", "SoftwareFanCurveGpu").unwrap_or(0).max(0) as usize;
    let mut last_log_error = String::new();
    println!(
        "afterburner-rs daemon on {} GPU(s). Poll {}ms; writing {}",
        gpus.len(), period, mahm::SHM_NAME
    );

    loop {
        // -- settings file can change under us (fan curve, log, flags)
        let cfg_text = std::fs::read_to_string(config::settings_path()).unwrap_or_default();
        if cfg_text != last_cfg_hash {
            last_cfg_hash = cfg_text.clone();
            let ini = config::Ini::parse(&cfg_text);
            let fresh = fancy_default_curve(&ini);
            {
                let mut c = curve_arc.lock().unwrap();
                *c = fresh.clone();
            }
            curve_cfg = fresh;
            fanctl = fancurve::FanController::new(curve_cfg.clone());
            curve_on.store(
                ini.get_i64("Settings", "SoftwareFanCurveEnabled").unwrap_or(0) == 1,
                Ordering::SeqCst,
            );
            curve_gpu = ini.get_i64("Settings", "SoftwareFanCurveGpu").unwrap_or(0).max(0) as usize;
            if let Some(ms) = ini.get_i64("Settings", "HwPollPeriod") {
                period = ms.clamp(100, 10000) as u64;
            }
            log_path = if ini.get_i64("Settings", "EnableLog").unwrap_or(0) == 1 {
                Some(ini.get("Settings", "LogPath").filter(|p|!p.is_empty()).map(str::to_owned)
                    .unwrap_or_else(||config::config_dir().join("hardware-log.csv").to_string_lossy().into_owned()))
            } else { None };
        }

        // -- poll all GPUs
        let mut entries = vec![];
        let mut samples: Vec<Sample> = vec![];
        let mut limits_now: Vec<drivers::Limits> = vec![];
        let mut caps_now: Vec<drivers::Caps> = vec![];
        for (k, g) in gpus.iter().enumerate() {
            let mut d = driver_for(g);
            let s = d.sample();
            entries.append(&mut monitor::entries_from_sample(
                &[(g.gpu_id(), String::new(), g.driver.clone())], k, &s, 0,
            ));
            if iter % 30 == 0 || caps_cache.len() != gpus.len() {
                limits_now.push(d.limits());
                caps_now.push(d.caps());
            }
            samples.push(s.clone());
        }
        if iter % 30 == 0 || limits_cache.len() != gpus.len() {
            limits_cache = limits_now.clone();
            caps_cache = caps_now.clone();
        }

        // -- publish to GUI/API
        {
            let snap = api::snapshot(&gpus, &samples, limits_cache.clone(), caps_cache.clone());
            *samples_arc.lock().unwrap() = snap;
        }

        // -- fan curve act
        if curve_on.load(Ordering::SeqCst) {
            if let Some(s) = samples.get(curve_gpu) {
                if let Some(temp) = s.temp_core {
                    if let Some(duty) = fanctl.target_for(temp) {
                        let mut d = driver_for(&gpus[curve_gpu]);
                        let rep = d.apply(&ApplySettings {
                            fan_pct: Some(duty),
                            fan_auto: false,
                            ..Default::default()
                        });
                        if !rep.notes.is_empty() {
                            eprintln!("[fan] {:?}", rep.notes);
                        }
                        eprintln!("[fan] {}\u{00b0}C -> {}%", temp, fanctl.last_duty());
                    }
                }
            }
        }

        // -- publish MAHM
        let gpu_entries = monitor::gpu_entries(&gpus.iter().collect::<Vec<_>>());
        let img = mahm::MahmImage {
            time: util::now_secs() as u32,
            entries,
            gpu_entries,
        };
        if let Err(e) = mahm::publish(&img) {
            eprintln!("mahm publish: {e}");
        }

        // -- serve MACM
        if let Some(m) = macm::read_shm() {
            macm::touch_access();
            match macm::request_of(&m) {
                macm::MacmRequest::Init => {
                    println!("[macm] INIT received");
                    macm::clear_command();
                }
                macm::MacmRequest::Apply { gpu: _, core_clock_boost_mhz, mem_clock_boost_mhz, fan_pct, fan_auto, thermal_limit_c } => {
                    println!("[macm] FLUSH received");
                    let mut d = driver_for(&gpus[0]);
                    let rep = d.apply(&ApplySettings {
                        core_offset_mhz: core_clock_boost_mhz,
                        mem_offset_mhz: mem_clock_boost_mhz,
                        lock_core_clock: None,
                        lock_mem_clock: None,
                        fan_pct,
                        fan_auto,
                        thermal_limit_c: thermal_limit_c.map(|t| t as u32),
                        power_limit_w: None,
                    });
                    for n in rep.notes {
                        eprintln!("[macm] {n}");
                    }
                    macm::clear_command();
                }
                macm::MacmRequest::FlushOnly => macm::clear_command(),
                _ => {}
            }
        }

        // -- optional CSV log
        if let Some(p) = &log_path {
            if let Err(e) = monitor::append_samples_csv(p, util::now_secs(), &samples) {
                let msg = format!("CSV {p}: {e}");
                if msg != last_log_error { eprintln!("{msg}"); last_log_error = msg; }
            } else { last_log_error.clear(); }
        }

        iter += 1;
        simple_sleep(period);
    }
}

fn fancy_default_curve(ini: &config::Ini) -> fancurve::FanCurveCfg {
    let mut cfg = fancurve::FanCurveCfg::default();
    if let Some(c) = ini.get("Settings", "SoftwareFanCurve") {
        let mut nodes = vec![];
        for part in c.split(',') {
            let ab: Vec<&str> = part.split(':').collect();
            if ab.len() == 2 {
                if let (Ok(t), Ok(d)) = (ab[0].trim().parse::<f32>(), ab[1].trim().parse::<f32>()) {
                    nodes.push(fancurve::FanNode { temp: t, duty: d });
                }
            }
        }
        if !nodes.is_empty() {
            cfg.nodes = nodes;
        }
    }
    if let Some(p) = ini.get_f32("Settings", "HwPollPeriod") {
        if p > 50.0 {
            cfg.or_interval_ms = p as u64 / 5;
        }
    }
    cfg
}

fn cmd_mahm_dump() -> i32 {
    match mahm::read_shm() {
        Ok((sig, esz, _b, gpus)) => {
            println!("signature = {sig:#x} entrySize = {esz}");
            println!("GPU entries served: {}", gpus.len());
            match read_entries() {
                Ok(v) => {
                    println!("{:<24} {:>10} {:>7} {:>4} {:>2}", "source", "data", "units", "gpu", "id");
                    for e in v {
                        let data = if e.data == f32::MAX {
                            "-".to_string()
                        } else {
                            format!("{:.1}", e.data)
                        };
                        println!(
                            "{:<24} {:>10} {:>7} {:>4} {:>2}",
                            e.src_name, data, e.src_units, e.gpu, e.src_id
                        );
                    }
                }
                Err(e) => {
                    println!("entries unreadable: {e}");
                    return 1;
                }
            }
            0
        }
        Err(e) => {
            println!("no MAHM memory found ({e}); run the daemon first");
            1
        }
    }
}

fn read_entries() -> std::io::Result<Vec<mahm::MahmEntry>> {
    let b = std::fs::read(mahm::SHM_NAME)?;
    if b.len() < mahm::MahmImage::header_size() {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short"));
    }
    let rd = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let (sig, n, esz, hdr) = (rd(0), rd(12), rd(16), rd(8));
    if sig != mahm::MAHM_SIGNATURE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "signature mismatch"));
    }
    let mut out = vec![];
    for i in 0..n as usize {
        let o = hdr as usize + i * esz as usize;
        if o + esz as usize > b.len() {
            break;
        }
        let v = &b[o..o + esz as usize];
        let c = |from: usize| -> String {
            let s = &v[from..from + mahm::MAX_PATH];
            let e = s.iter().position(|x| *x == 0).unwrap_or(0);
            String::from_utf8_lossy(&s[..e]).to_string()
        };
        out.push(mahm::MahmEntry {
            src_name: c(0),
            src_units: c(mahm::MAX_PATH),
            localized_src_name: c(2 * mahm::MAX_PATH),
            localized_src_units: c(3 * mahm::MAX_PATH),
            recommended_format: c(4 * mahm::MAX_PATH),
            data: f32::from_le_bytes([v[1300], v[1301], v[1302], v[1303]]),
            min_limit: f32::from_le_bytes([v[1304], v[1305], v[1306], v[1307]]),
            max_limit: f32::from_le_bytes([v[1308], v[1309], v[1310], v[1311]]),
            flags: u32::from_le_bytes([v[1312], v[1313], v[1314], v[1315]]),
            gpu: u32::from_le_bytes([v[1316], v[1317], v[1318], v[1319]]),
            src_id: u32::from_le_bytes([v[1320], v[1321], v[1322], v[1323]]),
        });
    }
    Ok(out)
}

fn cmd_macm_dump() -> i32 {
    match macm::read_shm() {
        Some(m) => {
            println!("master={} flags={:#x} time={} command={:#x}", m.master_gpu, m.flags, m.time, m.command);
            for (i, g) in m.gpus.iter().enumerate() {
                println!("GPU{i}: id={}", g.gpu_id);
                println!("  core boost {} kHz (cur..def) {}", g.core_clock_boost_khz[0], g.core_clock_boost_khz[3]);
                println!("  mem  boost {} kHz", g.memory_clock_boost_khz[0]);
                println!("  fan {}% flags={:#x}", g.fan_speed_pct, g.fan_flags);
                println!("  thermal limit {} °C", g.thermal_limit_c[0]);
                println!("  vf points {}", g.vf_points.len());
            }
            0
        }
        None => {
            println!("no MACM memory found; run daemon first");
            1
        }
    }
}
