//! PCI / DRM GPU discovery.
//!
//! GPU identifier is emulated in the exact format used by the Windows
//! version (MAHM/MACM `szGpuId`, see SDK/Include/MAHMSharedMemory.h):
//!   VEN_%04X&DEV_%04X&SUBSYS_%08X&REV_%02X&BUS_%X&DEV_%X&FN_%X

use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Nvidia, // 0x10DE
    Amd,    // 0x1002
    Intel,  // 0x8086
    Other(u16),
}

#[derive(Debug, Clone)]
pub struct GpuDevice {
    pub pci_addr: String, // "0000:01:00.0"
    pub vendor: Vendor,
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
    pub revision: u8,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub card: String, // "card1"
    pub hwmon: Option<PathBuf>,
    pub driver: String,
    pub name: Option<String>,
}

impl GpuDevice {
    pub fn gpu_id(&self) -> String {
        format!(
            "VEN_{:04X}&DEV_{:04X}&SUBSYS_{:04X}{:04X}&REV_{:02X}&BUS_{:X}&DEV_{:X}&FN_{:X}",
            self.vendor_id, self.device_id, self.subsystem_vendor, self.subsystem_device,
            self.revision, self.bus, self.dev, self.func
        )
    }
    pub fn vendor_name(&self) -> &'static str {
        match self.vendor {
            Vendor::Nvidia => "NVIDIA",
            Vendor::Amd => "AMD",
            Vendor::Intel => "Intel",
            Vendor::Other(_) => "Unknown",
        }
    }
}

/// Read first field of a PCI device attribute.
fn pci_attr(base: &str, attr: &str) -> Option<String> {
    std::fs::read_to_string(format!("{base}/{attr}"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn parse_hex_u16(s: &str) -> Option<u16> {
    let t = s.trim().trim_start_matches("0x");
    u16::from_str_radix(t, 16).ok()
}

pub fn discover_gpus() -> Vec<GpuDevice> {
    let mut out = Vec::new();
    let dir = match std::fs::read_dir("/sys/bus/pci/devices") {
        Ok(d) => d,
        Err(_) => return out,
    };
    let mut entries: Vec<String> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    entries.sort();

    for addr in entries {
        let base = format!("/sys/bus/pci/devices/{addr}");
        // Only display controllers (class starts with 0x03).
        let class = pci_attr(&base, "class").unwrap_or_default();
        if !class.starts_with("0x03") {
            continue;
        }
        let vid = pci_attr(&base, "vendor").and_then(|v| parse_hex_u16(&v));
        let did = pci_attr(&base, "device").and_then(|v| parse_hex_u16(&v));
        let (vid, did) = match (vid, did) {
            (Some(v), Some(d)) => (v, d),
            _ => continue,
        };
        let vendor = match vid {
            0x10de => Vendor::Nvidia,
            0x1002 | 0x1022 => Vendor::Amd,
            0x8086 => Vendor::Intel,
            other => Vendor::Other(other),
        };
        let subv = pci_attr(&base, "subsystem_vendor")
            .and_then(|v| parse_hex_u16(&v))
            .unwrap_or(0);
        let subd = pci_attr(&base, "subsystem_device")
            .and_then(|v| parse_hex_u16(&v))
            .unwrap_or(0);
        let rev = pci_attr(&base, "revision")
            .and_then(|v| parse_hex_u16(&v))
            .map(|v| v as u8)
            .unwrap_or(0);
        let parts: Vec<&str> = addr.split(':').collect();
        let (bus, df) = if parts.len() == 3 {
            (u8::from_str_radix(parts[1], 16).unwrap_or(0), parts[2])
        } else {
            (0, "00.0")
        };
        let dfp: Vec<&str> = df.split('.').collect();
        let dev = dfp.first().and_then(|d| u8::from_str_radix(d, 16).ok()).unwrap_or(0);
        let func = dfp.get(1).and_then(|d| u8::from_str_radix(d, 16).ok()).unwrap_or(0);

        let driver = std::fs::read_link(format!("{base}/driver"))
            .map(|l| l.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
            .unwrap_or_default();

        // Map to DRM card + hwmon
        let mut card = String::new();
        let mut hwmon: Option<PathBuf> = None;
        if let Ok(rd) = std::fs::read_dir(format!("{base}/drm")) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with("card") && !n.contains('-') {
                    card = n;
                }
            }
        }
        if let Ok(rd) = std::fs::read_dir(format!("{base}/hwmon")) {
            for e in rd.flatten() {
                hwmon = Some(e.path());
            }
        }
        // GPUs with only /dev/dri but no cardN link (older kernels) still count.
        if card.is_empty() {
            continue;
        }

        out.push(GpuDevice {
            pci_addr: addr,
            vendor,
            vendor_id: vid,
            device_id: did,
            subsystem_vendor: subv,
            subsystem_device: subd,
            revision: rev,
            bus,
            dev,
            func,
            card,
            hwmon,
            driver,
            name: None,
        });
    }
    out
}

/// Optional: nice human names through nvidia-smi / dri device name if available.
pub fn attach_display_names(gpus: &mut [GpuDevice], nvsmi_names: &BTreeMap<String, String>) {
    for g in gpus.iter_mut() {
        let key = format!("00000000:{:02X}:{:02X}.{:X}", g.bus, g.dev, g.func).to_uppercase();
        if let Some(n) = nvsmi_names.get(&key) {
            g.name = Some(n.clone());
        }
    }
}
