//! Servidor HTTP local del daemon — el equivalente del "server" de
//! MSI Afterburner (Windows: servidor de sensores remoto en TCP 7939).
//! Sirve la GUI (assets/gui.html) y un API JSON mínimo en 127.0.0.1.
//! POST /api/set aplicar settings; POST /api/curve cambia la curva de fan.

use crate::config;
use crate::drivers::{ApplySettings, Caps, Limits, Sample};
use crate::gpu::{self, GpuDevice};
use crate::fancurve;
use crate::util;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const DEFAULT_PORT: u16 = 7939;

pub struct ApiState {
    pub samples: Arc<Mutex<Vec<SampleData>>>,
    pub curve: Arc<Mutex<fancurve::FanCurveCfg>>,
    pub curve_enabled: Arc<AtomicBool>,
    pub annotate: String, // version string
}

#[derive(Clone)]
pub struct SampleData {
    pub gpu_id: String,
    pub vendor: String,
    pub display: String,
    pub sample: Sample,
    pub limits: Limits,
    pub caps: Caps,
}

fn json_num(v: Option<f32>) -> String {
    match v {
        Some(x) if x.is_finite() => format!("{x:.2}"),
        _ => "null".into(),
    }
}

fn json_num_i(v: Option<i32>) -> String {
    match v {
        Some(x) => format!("{x}"),
        None => "null".into(),
    }
}

impl ApiState {
    pub fn samples_json(&self) -> String {
        let s = self.samples.lock().unwrap();
        let mut arr = Vec::with_capacity(s.len());
        for (i, g) in s.iter().enumerate() {
            let lim = &g.limits;
            arr.push(format!(
                "{{\"index\":{i},\"id\":\"{id}\",\"vendor\":\"{v}\",\"name\":\"{n}\",\
                 \"temp\":{t},\"fan\":{f},\"fan_rpm\":{fr},\"clk_core\":{cc},\"clk_mem\":{cm},\
                 \"usage\":{u},\"power\":{p},\"power_limit\":{pl},\
                 \"power_min\":{pmin},\"power_max\":{pmax},\"power_default\":{pdef},\
                 \"caps_core_offset\":{co},\"caps_mem_offset\":{cmo},\"caps_power\":{cp},\
                 \"caps_fan\":{cf},\"caps_fan_auto\":{cfa},\"driver\":\"{dr}\"}}",
                id = g.gpu_id,
                v = g.vendor,
                n = g.display.replace('"', "'"),
                t = json_num(g.sample.temp_core),
                f = json_num(g.sample.fan_pct),
                fr = json_num(g.sample.fan_rpm),
                cc = json_num(g.sample.clk_core),
                cm = json_num(g.sample.clk_mem),
                u = json_num(g.sample.usage_gpu),
                p = json_num(g.sample.power_w),
                pl = json_num(g.sample.power_limit_w),
                pmin = json_num(lim.power_min_w),
                pmax = json_num(lim.power_max_w),
                pdef = json_num(lim.power_default_w),
                co = g.caps.core_clock_offset,
                cmo = g.caps.mem_clock_offset,
                cp = g.caps.power_limit,
                cf = g.caps.fan,
                cfa = g.caps.fan_auto,
                dr = g.vendor,
            ));
        }
        let (nodes, en) = {
            let c = self.curve.lock().unwrap();
            (c.nodes.clone(), self.curve_enabled.load(Ordering::SeqCst))
        };
        let curve = nodes
            .iter()
            .map(|n| format!("[{:.0},{:.0}]", n.temp, n.duty))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"app\":\"afterburner-rs\",\"version\":\"{vers}\",\"gpus\":[{gpu_arr}],\"curve_enabled\":{en},\"curve\":[{curve}]}}",
            gpu_arr = arr.join(","),
            vers = self.annotate,
        )
    }

    fn body_json_get_num(body: &str, key: &str) -> Option<f32> {
        let pat = format!("\"{key}\"");
        let pos = body.find(&pat)? + pat.len();
        let rest = &body[pos..];
        let colon = rest.find(':')?;
        let after = &rest[colon + 1..];
        let mut s = String::new();
        for c in after.chars() {
            match c {
                ' ' | '\t' | '\r' | '\n' => {
                    if !s.is_empty() {
                        break;
                    }
                }
                '0'..='9' | '-' | '+' | '.' => s.push(c),
                _ => break,
            }
        }
        s.parse::<f32>().ok()
    }

    fn body_json_get_str(body: &str, key: &str) -> Option<String> {
        let pat = format!("\"{key}\"");
        let pos = body.find(&pat)? + pat.len();
        let rest = &body[pos..];
        let colon = rest.find(':')?;
        let after = &rest[colon + 1..];
        let q1 = after.find('"')?;
        let rest2 = &after[q1 + 1..];
        let q2 = rest2.find('"')?;
        Some(rest2[..q2].to_string())
    }

    /// aplica settings desde un cuerpo JSON del GUI (toda la superficie de set).
    fn handle_set(&self, body: &str) -> String {
        let gpus = gpu::discover_gpus();
        let idx = Self::body_json_get_num(body, "gpu").unwrap_or(0.0).max(0.0) as usize;
        if idx >= gpus.len() {
            return "{\"ok\":false,\"error\":\"gpu index out of range\"}".into();
        }
        let g = gpus[idx].clone();
        let mut d = crate::driver_for(&g);
        let fan_mode = Self::body_json_get_str(body, "fan_mode").unwrap_or_default();
        let fan_pct = Self::body_json_get_num(body, "fan_pct");
        let power = Self::body_json_get_num(body, "power_w");
        let power_pct = Self::body_json_get_num(body, "power_pct");
        let core_offset = Self::body_json_get_num(body, "core_offset");
        let mem_offset = Self::body_json_get_num(body, "mem_offset");
        let lock_core_min = Self::body_json_get_num(body, "lock_core_min");
        let lock_core_max = Self::body_json_get_num(body, "lock_core_max");
        let lock_mem_min = Self::body_json_get_num(body, "lock_mem_min");
        let lock_mem_max = Self::body_json_get_num(body, "lock_mem_max");
        let mut set = ApplySettings {
            core_offset_mhz: core_offset.map(|x| x as i32),
            mem_offset_mhz: mem_offset.map(|x| x as i32),
            lock_core_clock: None,
            lock_mem_clock: None,
            power_limit_w: None,
            fan_pct: None,
            fan_auto: false,
            thermal_limit_c: None,
        };
        if fan_mode == "auto" {
            set.fan_auto = true;
        } else if fan_mode == "manual" {
            set.fan_pct = fan_pct.map(|x| x as u32);
        }
        if let Some(p) = power {
            set.power_limit_w = Some(p);
        } else if let Some(pp) = power_pct {
            let lim = d.limits();
            let base = lim.power_default_w.unwrap_or(lim.power_max_w.unwrap_or(0.0));
            if base > 0.0 {
                set.power_limit_w = Some(base * pp / 100.0);
            }
        }
        if let (Some(a), Some(b)) = (lock_core_min, lock_core_max) {
            if a >= 0.0 && b > 0.0 {
                set.lock_core_clock = Some((a as u32, b as u32));
            }
        }
        if let (Some(a), Some(b)) = (lock_mem_min, lock_mem_max) {
            if a >= 0.0 && b > 0.0 {
                set.lock_mem_clock = Some((a as u32, b as u32));
            }
        }
        let rep = d.apply(&set);
        let mut notes = vec![];
        for n in &rep.notes {
            notes.push(n.replace('"', "'"));
        }
        let notes_json = notes
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"ok\":true,\"notes\":[{notes_json}]}}")
    }

    fn handle_reset(&self, body: &str) -> String {
        let gpus = gpu::discover_gpus();
        let idx = Self::body_json_get_num(body, "gpu").unwrap_or(0.0).max(0.0) as usize;
        let mut all_notes = vec![];
        let chosen: Vec<GpuDevice> = if idx < gpus.len() {
            vec![gpus[idx].clone()]
        } else {
            gpus.clone()
        };
        for g in chosen {
            let mut d = crate::driver_for(&g);
            let rep = d.reset();
            for n in rep.notes {
                all_notes.push(n.replace('"', "'"));
            }
        }
        let j = all_notes
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"ok\":true,\"notes\":[{j}]}}")
    }

    fn handle_profile_save(&self, body: &str) -> String {
        let slot = Self::body_json_get_num(body, "slot").unwrap_or(1.0).max(1.0) as u32;
        let v = config::ProfileValues {
            core_clock_offset: Self::body_json_get_num(body, "core_offset").map(|x| x as i32),
            mem_clock_offset: Self::body_json_get_num(body, "mem_offset").map(|x| x as i32),
            power_limit_w: Self::body_json_get_num(body, "power_w"),
            power_limit_pct: Self::body_json_get_num(body, "power_pct"),
            core_voltage_boost: None,
            thermal_limit: None,
            fan_pct: Self::body_json_get_num(body, "fan_pct").map(|x| x as u32),
            fan_auto: Self::body_json_get_str(body, "fan_mode").as_deref() == Some("auto"),
            fan_curve_nodes: (|| {
                {
                    let c = self.curve.lock().unwrap();
                    c.nodes.iter().map(|n| (n.temp, n.duty)).collect()
                }
            })(),
            name: Self::body_json_get_str(body, "name").unwrap_or_else(|| format!("perfil {slot}")),
        };
        match config::save_profile(slot, &v, &Limits::default()) {
            Ok(p) => format!(
                "{{\"ok\":true,\"path\":\"{}\"}}",
                p.to_string_lossy().replace('\\', "/")
            ),
            Err(e) => format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
        }
    }

    fn handle_profile_load(&self, body: &str) -> String {
        let slot = Self::body_json_get_num(body, "slot").unwrap_or(1.0).max(1.0) as u32;
        let v = match config::load_profile(slot) {
            Some(v) => v,
            None => {
                return format!("{{\"ok\":false,\"error\":\"Profile{slot} no existe\"}}");
            }
        };
        let gpus = gpu::discover_gpus();
        if gpus.is_empty() {
            return "{\"ok\":false,\"error\":\"no GPUs\"}".into();
        }
        let mut d = crate::driver_for(&gpus[0]);
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
        let j = rep
            .notes
            .iter()
            .map(|n| format!("\"{}\"", n.replace('"', "'")))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"ok\":true,\"notes\":[{j}]}}")
    }

    fn handle_profiles_list(&self) -> String {
        let mut arr = vec![];
        for slot in 1u32..=9 {
            if let Some(v) = config::load_profile(slot) {
                arr.push(format!(
                    "{{\"slot\":{slot},\"name\":\"{name}\",\"core\":{c},\"mem\":{m},\"power\":{p},\"fan_auto\":{fa}}}",
                    name = v.name.replace('"', "'"),
                    c = json_num_i(v.core_clock_offset),
                    m = json_num_i(v.mem_clock_offset),
                    p = v.power_limit_w.map(|x| format!("{x:.1}")).unwrap_or_else(|| "null".into()),
                    fa = v.fan_auto,
                ));
            }
        }
        format!("{{\"ok\":true,\"profiles\":[{}]}}", arr.join(","))
    }

    fn handle_mahm(&self) -> String {
        match crate::mahm::read_shm() {
            Ok(_) => {
                match crate::mahm::entries_from_shm() {
                    Ok(entries) => {
                        let mut rows = vec![];
                        for e in entries {
                            let data = if e.data >= 3.4e38 {
                                "null".to_string()
                            } else {
                                format!("{:.1}", e.data)
                            };
                            rows.push(format!(
                                "{{\"name\":\"{n}\",\"units\":\"{u}\",\"data\":{d},\"id\":\"0x{i:02x}\",\"gpu\":{g}}}",
                                n = e.src_name.replace('"', "'"),
                                u = e.src_units.replace('"', "'"),
                                d = data,
                                i = e.src_id.max(0) as u32,
                                g = e.gpu
                            ));
                        }
                        format!(
                            "{{\"signature\":\"MAHM\",\"entries\":[{}],\"gpus\":\"{}\"}}",
                            rows.join(","),
                            gpu::discover_gpus().len()
                        )
                    }
                    Err(e) => format!("{{\"error\":\"{e}\"}}"),
                }
            }
            Err(e) => format!("{{\"error\":\"{e}\"}}"),
        }
    }

    fn handle_log(&self, body: &str) -> String {
        let enable = Self::body_json_get_num(body, "enable").unwrap_or(0.0) != 0.0;
        let path = Self::body_json_get_str(body, "path")
            .unwrap_or_else(|| config::config_dir().join("hardware-log.csv").to_string_lossy().to_string());
        let dir = config::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let txt = std::fs::read_to_string(config::settings_path()).unwrap_or_default();
        let mut ini = config::Ini::parse(&txt);
        ini.set("Settings", "EnableLog", if enable { "1" } else { "0" });
        ini.set("Settings", "LogPath", &path);
        let _ = std::fs::write(config::settings_path(), ini.to_text());
        format!("{{\"ok\":true,\"enabled\":{enable},\"path\":\"{}\"}}", path.replace('\\', "/"))
    }

    fn handle_curve(&self, body: &str) -> String {
        // {"curve":[[temp,duty]..]} — guardamos en el cfg del usuario
        let mut nodes: Vec<fancurve::FanNode> = vec![];
        let mut b = body;
        while let Some(o) = b.find('[') {
            let close = b.find(']');
            let close = match close {
                Some(c) => c,
                None => break,
            };
            if close < o {
                b = &b[close + 1..];
                continue;
            }
            let inner = &b[o + 1..close];
            if inner.contains('[') {
                // es el corchete exterior del array; avanzar hacia el nodo interior
                b = &b[o + 1..];
                continue;
            }
            let parts: Vec<&str> = inner.split(',').collect();
            if parts.len() == 2 {
                if let (Ok(t), Ok(d)) = (
                    parts[0].trim().parse::<f32>(),
                    parts[1].trim().parse::<f32>(),
                ) {
                    nodes.push(fancurve::FanNode { temp: t, duty: d });
                }
            }
            b = &b[close + 1..];
        }
        if nodes.is_empty() {
            return "{\"ok\":false,\"error\":\"no nodes parsed\"}".into();
        }
        // persist
        let dir = config::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let txt = std::fs::read_to_string(config::settings_path()).unwrap_or_default();
        let mut ini = config::Ini::parse(&txt);
        ini.set("Settings", "SoftwareFanCurve", &config::format_nodes(&nodes.iter().map(|n| (n.temp, n.duty)).collect::<Vec<_>>()));
        ini.set("Settings", "SoftwareFanCurveEnabled", "1");
        let _ = std::fs::write(config::settings_path(), ini.to_text());
        {
            let mut c = self.curve.lock().unwrap();
            c.nodes = nodes;
        }
        self.curve_enabled.store(true, Ordering::SeqCst);
        "{\"ok\":true}".into()
    }

    fn handle_curve_disable(&self) -> String {
        let dir = config::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let txt = std::fs::read_to_string(config::settings_path()).unwrap_or_default();
        let mut ini = config::Ini::parse(&txt);
        ini.set("Settings", "SoftwareFanCurveEnabled", "0");
        let _ = std::fs::write(config::settings_path(), ini.to_text());
        self.curve_enabled.store(false, Ordering::SeqCst);
        // dejar el fan en auto del driver
        let gpus = gpu::discover_gpus();
        if let Some(g) = gpus.first() {
            let mut d = crate::driver_for(g);
            let _ = d.apply(&ApplySettings { fan_auto: true, ..Default::default() });
        }
        "{\"ok\":true}".into()
    }
}

fn respond(mut stream: TcpStream, status: &str, ctype: &str, body: &str) {
    let bin = body.as_bytes();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        bin.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(bin);
    let _ = stream.flush();
}

const HTML: &str = include_str!("assets/gui.html");
const ICON_SVG: &str = include_str!("assets/icon.svg");

fn ok_localhost(peer: &std::net::SocketAddr) -> bool {
    let ip = peer.ip();
    ip.is_loopback()
}

fn handle_conn(mut stream: TcpStream, state: Arc<ApiState>) {
    let peer = match stream.peer_addr() {
        Ok(p) => p,
        Err(_) => return,
    };
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    if n == 0 {
        return;
    }
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let first_line = req.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/").to_string();

    if !ok_localhost(&peer) {
        respond(stream, "403 Forbidden", "text/plain", "localhost only");
        return;
    }

    // por seguridad las escrituras solo desde loopback y método correcto
    match (method, path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => respond(stream, "200 OK", "text/html; charset=utf-8", HTML),
        ("GET", "/api/sample") => {
            let j = state.samples_json();
            respond(stream, "200 OK", "application/json", &j);
        }
        ("GET", "/api/profiles") => {
            let j = state.handle_profiles_list();
            respond(stream, "200 OK", "application/json", &j);
        }
        ("GET", "/api/mahm") => {
            let j = state.handle_mahm();
            respond(stream, "200 OK", "application/json", &j);
        }
        ("GET", "/icon.svg") => respond(stream, "200 OK", "image/svg+xml", ICON_SVG),
        ("POST", "/api/set") => {
            let body = extract_body(&req);
            let j = state.handle_set(&body);
            respond(stream, "200 OK", "application/json", &j);
        }
        ("POST", "/api/reset") => {
            let body = extract_body(&req);
            let j = state.handle_reset(&body);
            respond(stream, "200 OK", "application/json", &j);
        }
        ("POST", "/api/curve") => {
            let body = extract_body(&req);
            let j = state.handle_curve(&body);
            respond(stream, "200 OK", "application/json", &j);
        }
        ("POST", "/api/curve-off") => {
            let j = state.handle_curve_disable();
            respond(stream, "200 OK", "application/json", &j);
        }
        ("POST", "/api/profile/save") => {
            let body = extract_body(&req);
            let j = state.handle_profile_save(&body);
            respond(stream, "200 OK", "application/json", &j);
        }
        ("POST", "/api/profile/load") => {
            let body = extract_body(&req);
            let j = state.handle_profile_load(&body);
            respond(stream, "200 OK", "application/json", &j);
        }
        ("POST", "/api/log") => {
            let body = extract_body(&req);
            let j = state.handle_log(&body);
            respond(stream, "200 OK", "application/json", &j);
        }
        ("GET", _) => respond(stream, "404 Not Found", "text/plain", "no route"),
        _ => respond(stream, "405 Method Not Allowed", "text/plain", "method not allowed"),
    }
}

fn extract_body(req: &str) -> String {
    // El contenido que recibimos cabe en el buffer inicial de 4 KiB.
    match req.split_once("\r\n\r\n") {
        Some((_, b)) => b.to_string(),
        None => req.to_string(),
    }
}

pub fn start_server(port: u16, state: Arc<ApiState>) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(s) = stream {
                let st = Arc::clone(&state);
                std::thread::spawn(move || handle_conn(s, st));
            }
        }
    });
    Ok(())
}

/// Llamado desde el daemon en cada vuelta de encuesta.
pub fn snapshot(
    gpus: &[GpuDevice],
    samples: &[Sample],
    limits: Vec<Limits>,
    caps: Vec<Caps>,
) -> Vec<SampleData> {
    gpus.iter()
        .enumerate()
        .map(|(i, g)| SampleData {
            gpu_id: g.gpu_id(),
            vendor: g.vendor_name().to_string(),
            display: g
                .name
                .clone()
                .unwrap_or_else(|| format!("{:04X}:{:04X}", g.vendor_id, g.device_id)),
            sample: samples.get(i).cloned().unwrap_or_default(),
            limits: limits.get(i).cloned().unwrap_or_default(),
            caps: caps.get(i).copied().unwrap_or_default(),
        })
        .collect()
}

pub fn port_from_settings(ini: &config::Ini) -> u16 {
    let p = ini.get_i64("Settings", "ServerPort").unwrap_or(DEFAULT_PORT as i64);
    if p > 0 && p < 65536 {
        p as u16
    } else {
        DEFAULT_PORT
    }
}

/// subcomando gui: garantiza el daemon vivo y abre el navegador.
pub fn open_gui() -> i32 {
    use std::process::{Command, Stdio};
    if let Ok(t) = TcpStream::connect(("127.0.0.1", DEFAULT_PORT)) {
        drop(t);
    } else {
        // daemon no vivo: levantarlo desacoplado
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("afterburner-rs"));
        let _ = Command::new(&exe)
            .arg("daemon")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        // esperar hasta 3 s
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(t) = TcpStream::connect(("127.0.0.1", DEFAULT_PORT)) {
                drop(t);
                break;
            }
        }
    }
    match Command::new("xdg-open").arg(format!("http://127.0.0.1:{DEFAULT_PORT}")).spawn() {
        Ok(_) => {
            println!("GUI abierta en http://127.0.0.1:{DEFAULT_PORT}");
            0
        }
        Err(_) => {
            println!("no se puede abrir el navegador; visita http://127.0.0.1:{DEFAULT_PORT}");
            1
        }
    }
}

pub fn now_secs64() -> u64 {
    util::now_secs()
}
