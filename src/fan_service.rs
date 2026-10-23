//! Narrow privileged bridge: fan control only, authenticated local Unix socket.
//! No shell commands, HTTP, user-controlled file paths, or arbitrary GPU writes.
use crate::drivers::nvfan;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::{fs::PermissionsExt, net::{UnixListener, UnixStream}};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const SOCKET: &str = "/run/afterburner-rs-fan/control.sock";
static STOP: AtomicBool = AtomicBool::new(false);
unsafe extern "C" {
    fn getsockopt(fd: i32, level: i32, name: i32, value: *mut std::ffi::c_void, len: *mut u32) -> i32;
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    fn geteuid() -> u32;
}
extern "C" fn stop(_: i32) { STOP.store(true, Ordering::Relaxed); }

pub fn request(pci: &str, pct: Option<u32>) -> Result<String, String> {
    let mut s = UnixStream::connect(SOCKET)
        .map_err(|_| "The fan-control permission service is not available (afterburner-rs-fan.service)".to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(8)));
    let command = match pct { Some(p) => format!("{pci} {p}\n"), None => format!("{pci} auto\n") };
    s.write_all(command.as_bytes()).map_err(|e| e.to_string())?;
    s.shutdown(std::net::Shutdown::Write).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    s.take(2048).read_to_string(&mut reply).map_err(|e| e.to_string())?;
    reply.strip_prefix("OK ").map(|s| s.trim().to_string())
        .ok_or_else(|| reply.trim_start_matches("ERROR ").trim().to_string())
}

fn parse(s: &str) -> Result<(&str, Option<u32>), String> {
    let v: Vec<_> = s.split_whitespace().collect();
    if v.len() != 2 { return Err("Expected a PCI address and a percentage or auto".into()); }
    let pci = v[0];
    if pci.len() != 12 || pci.as_bytes()[4] != b':' || pci.as_bytes()[7] != b':' || pci.as_bytes()[10] != b'.'
        || !pci.bytes().enumerate().all(|(i, b)| matches!(i, 4|7|10) || b.is_ascii_hexdigit()) {
        return Err("Invalid PCI address".into());
    }
    let pct = if v[1] == "auto" { None } else {
        let p = v[1].parse::<u32>().map_err(|_| "Invalid fan percentage")?;
        if p > 100 { return Err("Fan percentage out of range".into()); }
        Some(p)
    };
    Ok((pci, pct))
}

fn permitted(s: &UnixStream, uid: u32) -> bool {
    #[repr(C)] struct Peer { pid: i32, uid: u32, gid: u32 }
    let mut peer = Peer { pid: 0, uid: u32::MAX, gid: 0 };
    let mut size = std::mem::size_of::<Peer>() as u32;
    let r = unsafe { getsockopt(s.as_raw_fd(), 1, 17, (&mut peer as *mut Peer).cast(), &mut size) };
    r == 0 && size as usize == std::mem::size_of::<Peer>() && (peer.uid == uid || peer.uid == 0)
}

pub fn run(args: &[String]) -> i32 {
    let Some(uid) = args.first().and_then(|s| s.parse::<u32>().ok()) else { return 2 };
    if unsafe { geteuid() } != 0 { eprintln!("fan-service must be run by the system service"); return 1; }
    if let Err(e) = serve(uid) { eprintln!("fan-service: {e}"); return 1; }
    0
}

fn serve(uid: u32) -> std::io::Result<()> {
    // RuntimeDirectory is root-owned and created by systemd, not by a client.
    let _ = std::fs::remove_file(SOCKET);
    let listener = UnixListener::bind(SOCKET)?;
    std::fs::set_permissions(SOCKET, std::fs::Permissions::from_mode(0o660))?;
    listener.set_nonblocking(true)?;
    unsafe { signal(15, stop); signal(2, stop); }
    let mut touched = BTreeSet::new();
    while !STOP.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                if !permitted(&s, uid) { let _ = s.write_all(b"ERROR Access denied\n"); continue; }
                let mut b = String::new();
                let reply = match (&mut s).take(128).read_to_string(&mut b) {
                    Ok(_) => match parse(&b) {
                        Ok((pci, pct)) => match nvfan::set(pci, pct) {
                            Ok(msg) => { touched.insert(pci.to_string()); format!("OK {msg}\n") },
                            Err(e) => format!("ERROR {}\n", e.message),
                        },
                        Err(e) => format!("ERROR {e}\n"),
                    },
                    Err(e) => format!("ERROR {e}\n"),
                };
                let _ = s.write_all(reply.as_bytes());
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e),
        }
    }
    for pci in touched { let _ = nvfan::set(&pci, None); }
    let _ = std::fs::remove_file(SOCKET);
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn only_bounded_fan_commands() {
        assert_eq!(parse("0000:01:00.0 45").unwrap(), ("0000:01:00.0", Some(45)));
        assert_eq!(parse("0000:01:00.0 auto").unwrap().1, None);
        for bad in ["../../etc/passwd 45", "0000:01:00.0 101", "0000:01:00.0 -1", "0000:01:00.0 45 extra"] {
            assert!(parse(bad).is_err());
        }
    }
}
