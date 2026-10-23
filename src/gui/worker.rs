//! Hardware and processes never run on the GTK event loop.
use crate::{drivers::{ApplySettings, Caps, Limits, Sample}, gpu::GpuDevice};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Reading {
    pub gpu: GpuDevice,
    pub sample: Sample,
    pub limits: Limits,
    pub caps: Caps,
}
pub enum Work {
    Apply(usize, ApplySettings, bool),
    Reset(usize, bool),
    Service(&'static str),
    Stop,
}
pub enum Event {
    Readings(Vec<Reading>),
    Finished(String, bool),
}

fn failed(notes: &[String]) -> bool {
    notes.iter().any(|n| {
        let n = n.to_lowercase();
        ["error", "denied", "permission", "requires", "failed", "not supported", "not available", "could not", "does not support"]
            .iter().any(|word| n.contains(word))
    })
}

fn settings_args(gpu: usize, s: &ApplySettings) -> Vec<String> {
    let mut v = vec!["set".into(), "--gpu".into(), gpu.to_string()];
    for (name, val) in [
        ("--core-offset", s.core_offset_mhz.map(|x| x.to_string())),
        ("--mem-offset", s.mem_offset_mhz.map(|x| x.to_string())),
        ("--power", s.power_limit_w.map(|x| x.to_string())),
        ("--fan", s.fan_pct.map(|x| x.to_string())),
        ("--lock-gpu-clocks", s.lock_core_clock.map(|(a,b)| format!("{a},{b}"))),
        ("--lock-memory-clocks", s.lock_mem_clock.map(|(a,b)| format!("{a},{b}"))),
    ] {
        if let Some(val) = val { v.push(name.into()); v.push(val); }
    }
    if s.fan_auto { v.push("--fan-auto".into()); }
    v
}

fn authorized(args: Vec<String>) -> (String, bool) {
    // pkexec presents the system's graphical authorization dialog. Never run GTK as root.
    // Use only the installed, root-owned executable, not a writable build artifact.
    match Command::new("pkexec").arg("/usr/local/bin/afterburner-rs").args(args).output() {
        Ok(o) => {
            let msg = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            let error = !o.status.success() || failed(&[msg.clone()]);
            (if msg.trim().is_empty() { "Authorization was cancelled or returned no response".into() } else { msg.trim().into() }, error)
        },
        Err(e) => (format!("Could not open system authorization: {e}"), true),
    }
}

pub fn start() -> (Sender<Work>, Receiver<Event>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let (etx, erx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut gpus = crate::gpu::discover_gpus();
        crate::gpu::attach_display_names(&mut gpus, &crate::nvsmi_names());
        let mut drivers: Vec<_> = gpus.iter().map(crate::driver_for).collect();
        let mut limits = vec![Limits::default(); gpus.len()];
        let mut caps = vec![Caps::default(); gpus.len()];
        let mut next = Instant::now();
        let mut polls = 0;
        let mut manual_gpu = None;
        loop {
            match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(Work::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // A manually set speed belongs to this GUI session. A daemon curve is independent.
                    if let Some(i) = manual_gpu { let _ = drivers[i as usize].apply(&ApplySettings { fan_auto: true, ..Default::default() }); }
                    break;
                }
                Ok(Work::Apply(i, s, admin)) if i < drivers.len() => {
                    let result = if admin { authorized(settings_args(i, &s)) } else {
                        let r = drivers[i].apply(&s);
                        let bad = failed(&r.notes);
                        (if r.notes.is_empty() { "Request completed. Check the sensor readings to verify the result.".into() } else { r.notes.join(" · ") }, bad)
                    };
                    if !result.1 && s.fan_pct.is_some() { manual_gpu = Some(i); }
                    if s.fan_auto { manual_gpu = None; }
                    let _ = etx.send(Event::Finished(result.0, result.1));
                    next = Instant::now();
                }
                Ok(Work::Reset(i, admin)) if i < drivers.len() => {
                    let result = if admin { authorized(vec!["reset".into(), "--gpu".into(), i.to_string()]) } else {
                        let r = drivers[i].reset(); (r.notes.join(" · "), failed(&r.notes))
                    };
                    manual_gpu = None;
                    let _ = etx.send(Event::Finished(result.0, result.1));
                    next = Instant::now();
                }
                Ok(Work::Service(action)) => {
                    let r = Command::new("systemctl").args([action, "afterburner-rs.service"]).output();
                    let (msg, bad) = match r {
                        Ok(o) if o.status.success() => (format!("Service action completed: {action}"), false),
                        Ok(o) => (String::from_utf8_lossy(&o.stderr).trim().into(), true),
                        Err(e) => (e.to_string(), true),
                    };
                    let _ = etx.send(Event::Finished(msg, bad));
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if Instant::now() < next { continue; }
            let mut readings = Vec::new();
            for (i, d) in drivers.iter_mut().enumerate() {
                let sample = d.sample();
                if polls % 30 == 0 { limits[i] = d.limits(); caps[i] = d.caps(); }
                readings.push(Reading { gpu: gpus[i].clone(), sample, limits: limits[i].clone(), caps: caps[i] });
            }
            if etx.send(Event::Readings(readings)).is_err() { break; }
            polls += 1;
            next = Instant::now() + Duration::from_secs(1);
        }
    });
    (tx, erx, handle)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn fan_arguments_are_separate_not_a_shell() {
        let args = settings_args(1, &ApplySettings { fan_pct: Some(55), ..Default::default() });
        assert_eq!(args, ["set", "--gpu", "1", "--fan", "55"]);
    }
}
