//! MAHM-compatible shared memory — runtime representation emulating the
//! "MSI Afterburner Hardware Monitor shared memory" interface v2.0.
//!
//! Layout constants (header/entry sizes, source IDs, MAX_PATH=260) are taken
//! from the interop SDK headers published inside the 4.6.6 distribution
//! (SDK/Include/MAHMSharedMemory.h). Nothing here links vendor code: it is an
//! original reimplementation on top of Linux POSIX shared memory (/dev/shm).

use std::io::Write;

pub const MAX_PATH: usize = 260;
pub const MAHM_SIGNATURE: u32 = 0x4D48414D; // 'MAHM'
pub const MAHM_SIGNATURE_DEAD: u32 = 0xDEAD;
pub const MAHM_VERSION: u32 = 0x00020000;

// MONITORING_SOURCE_ID_* — source ID vocabulary from the interop header.
pub const SID_UNKNOWN: u32 = 0xFFFFFFFF;
pub const SID_GPU_TEMPERATURE: u32 = 0x00000000;
pub const SID_PCB_TEMPERATURE: u32 = 0x00000001;
pub const SID_MEM_TEMPERATURE: u32 = 0x00000002;
pub const SID_VRM_TEMPERATURE: u32 = 0x00000003;
pub const SID_FAN_SPEED: u32 = 0x00000010;
pub const SID_FAN_TACHOMETER: u32 = 0x00000011;
pub const SID_CORE_CLOCK: u32 = 0x00000020;
pub const SID_SHADER_CLOCK: u32 = 0x00000021;
pub const SID_MEMORY_CLOCK: u32 = 0x00000022;
pub const SID_GPU_USAGE: u32 = 0x00000030;
pub const SID_MEMORY_USAGE: u32 = 0x00000031;
pub const SID_FB_USAGE: u32 = 0x00000032;
pub const SID_GPU_VOLTAGE: u32 = 0x00000040;
pub const SID_FRAMERATE: u32 = 0x00000050;
pub const SID_FRAMETIME: u32 = 0x00000051;
pub const SID_GPU_REL_POWER: u32 = 0x00000060;
pub const SID_GPU_ABS_POWER: u32 = 0x00000061;
pub const SID_GPU_TEMP_LIMIT: u32 = 0x00000070;
pub const SID_GPU_POWER_LIMIT: u32 = 0x00000071;
pub const SID_CPU_TEMPERATURE: u32 = 0x00000080;
pub const SID_CPU_USAGE: u32 = 0x00000090;
pub const SID_RAM_USAGE: u32 = 0x00000091;

pub const FLAG_SHOW_IN_OSD: u32 = 0x00000001;
pub const FLAG_SHOW_IN_LCD: u32 = 0x00000002;
pub const FLAG_SHOW_IN_TRAY: u32 = 0x00000004;

pub const SHM_NAME: &str = "/dev/shm/MAHMSharedMemory";
pub const SHM_ACCESS_NAME: &str = "/dev/shm/Access_MAHMSharedMemory";

#[derive(Debug, Clone)]
pub struct MahmEntry {
    pub src_name: String,
    pub src_units: String,
    pub localized_src_name: String,
    pub localized_src_units: String,
    pub recommended_format: String,
    pub data: f32,      // f32::MAX → not available
    pub min_limit: f32,
    pub max_limit: f32,
    pub flags: u32,
    pub gpu: u32,
    pub src_id: u32,
}

#[derive(Debug, Clone)]
pub struct MahmGpuEntry {
    pub gpu_id: String,
    pub family: String,
    pub device: String,
    pub driver: String,
    pub bios: String,
    pub mem_amount_kb: u32,
}

pub struct MahmImage {
    pub time: u32,
    pub entries: Vec<MahmEntry>,
    pub gpu_entries: Vec<MahmGpuEntry>,
}

// ---- binary emit -----------------------------------------------------------

fn str_field(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut v = b.to_vec();
    if b.len() >= MAX_PATH {
        v.truncate(MAX_PATH - 1);
    }
    v.resize(MAX_PATH, 0);
    v
}

impl MahmImage {
    pub fn header_size() -> usize {
        8 * 4
    }
    pub fn entry_size() -> usize {
        5 * MAX_PATH + 3 * 4 + 3 * 4
    }
    pub fn gpu_entry_size() -> usize {
        5 * MAX_PATH + 4
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(
            Self::header_size()
                + self.entries.len() * Self::entry_size()
                + self.gpu_entries.len() * Self::gpu_entry_size(),
        );
        b.extend_from_slice(&MAHM_SIGNATURE.to_le_bytes());
        b.extend_from_slice(&MAHM_VERSION.to_le_bytes());
        b.extend_from_slice(&(Self::header_size() as u32).to_le_bytes());
        b.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        b.extend_from_slice(&(Self::entry_size() as u32).to_le_bytes());
        b.extend_from_slice(&self.time.to_le_bytes());
        b.extend_from_slice(&(self.gpu_entries.len() as u32).to_le_bytes());
        b.extend_from_slice(&(Self::gpu_entry_size() as u32).to_le_bytes());

        for e in &self.entries {
            b.extend_from_slice(&str_field(&e.src_name));
            b.extend_from_slice(&str_field(&e.src_units));
            b.extend_from_slice(&str_field(&e.localized_src_name));
            b.extend_from_slice(&str_field(&e.localized_src_units));
            b.extend_from_slice(&str_field(&e.recommended_format));
            b.extend_from_slice(&e.data.to_le_bytes());
            b.extend_from_slice(&e.min_limit.to_le_bytes());
            b.extend_from_slice(&e.max_limit.to_le_bytes());
            b.extend_from_slice(&e.flags.to_le_bytes());
            b.extend_from_slice(&e.gpu.to_le_bytes());
            b.extend_from_slice(&e.src_id.to_le_bytes());
        }
        for g in &self.gpu_entries {
            b.extend_from_slice(&str_field(&g.gpu_id));
            b.extend_from_slice(&str_field(&g.family));
            b.extend_from_slice(&str_field(&g.device));
            b.extend_from_slice(&str_field(&g.driver));
            b.extend_from_slice(&str_field(&g.bios));
            b.extend_from_slice(&g.mem_amount_kb.to_le_bytes());
        }
        b
    }
}

/// Write the full image into /dev/shm exactly as Windows clients expect.
pub fn publish(img: &MahmImage) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let blob = img.to_bytes();
    // Write atomically: data then rename keeps readers from tearing.
    {
        let mut f = std::fs::File::create(SHM_NAME)?;
        f.write_all(&blob)?;
        // Interop: any user-space consumer (GUI, scripts, other monitors)
        // must be able to read the image, and a future daemon running as a
        // different user must be able to take ownership of publication.
        let _ = std::fs::set_permissions(SHM_NAME, std::fs::Permissions::from_mode(0o666));
    }
    // sentinel object so external tools see that this driver is present
    let mut a = std::fs::File::create(SHM_ACCESS_NAME)?;
    a.write_all(b"ABRS")?;
    let _ = std::fs::set_permissions(SHM_ACCESS_NAME, std::fs::Permissions::from_mode(0o666));
    Ok(())
}

pub fn mark_dead() {
    let _ = std::fs::remove_file(SHM_ACCESS_NAME);
    if let Ok(meta) = std::fs::metadata(SHM_NAME) {
        if meta.len() >= 6 {
            if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(SHM_NAME) {
                let _ = f.write_all(&MAHM_SIGNATURE_DEAD.to_le_bytes());
            }
        }
    }
}

/// Read back for `mahm-dump` (equivalent of MAHMSharedMemorySample.exe).
pub fn read_shm() -> std::io::Result<(u32, u32, Vec<u8>, Vec<Vec<u8>>)> {
    let b = std::fs::read(SHM_NAME)?;
    if b.len() < 8 * 4 {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short header"));
    }
    let rd = |off: usize| -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    };
    let sig = rd(0);
    let hdr = rd(8);
    let n = rd(12);
    let esz = rd(16);
    let ng = rd(24);
    let gesz = rd(28);
    if esz <= 0 || b.len() < hdr as usize + (n * esz) as usize {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad sizes"));
    }
    let _entries: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let o = hdr as usize + (i * esz) as usize;
            b[o..o + esz as usize].to_vec()
        })
        .collect();
    let go = hdr as usize + (n * esz) as usize;
    let gpus: Vec<Vec<u8>> = (0..ng)
        .filter_map(|i| {
            let o = go + (i * gesz) as usize;
            if o + gesz as usize <= b.len() {
                Some(b[o..o + gesz as usize].to_vec())
            } else {
                None
            }
        })
        .collect();
    Ok((sig, esz, b, gpus))
}

/// Lectura de entradas del objeto compartido para consumidores externos
/// (mahm-dump CLI y el API del GUI).
pub fn entries_from_shm() -> std::io::Result<Vec<MahmEntry>> {
    let b = std::fs::read(SHM_NAME)?;
    let hdr = 8 * 4;
    if b.len() < hdr {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short"));
    }
    let rd = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let (sig, n, esz, hsize) = (rd(0), rd(12), rd(16), rd(8));
    if sig != MAHM_SIGNATURE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "signature mismatch"));
    }
    let esz = esz as usize;
    let mut out = vec![];
    for i in 0..n as usize {
        let o = hsize as usize + i * esz;
        if o + esz > b.len() {
            break;
        }
        let v = &b[o..o + esz];
        let c = |from: usize| -> String {
            let s = &v[from..from + MAX_PATH];
            let e = s.iter().position(|x| *x == 0).unwrap_or(0);
            String::from_utf8_lossy(&s[..e]).to_string()
        };
        let f = |from: usize| f32::from_le_bytes(v[from..from + 4].try_into().unwrap());
        let u = |from: usize| u32::from_le_bytes(v[from..from + 4].try_into().unwrap());
        out.push(MahmEntry {
            src_name: c(0),
            src_units: c(MAX_PATH),
            localized_src_name: c(2 * MAX_PATH),
            localized_src_units: c(3 * MAX_PATH),
            recommended_format: c(4 * MAX_PATH),
            data: f(5 * MAX_PATH),
            min_limit: f(5 * MAX_PATH + 4),
            max_limit: f(5 * MAX_PATH + 8),
            flags: u(5 * MAX_PATH + 12),
            gpu: u(5 * MAX_PATH + 16),
            src_id: u(5 * MAX_PATH + 20),
        });
    }
    Ok(out)
}
