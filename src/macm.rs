//! MACM-compatible control memory — the write side used by third-party
//! tools to command settings through MSI Afterburner (SDK/Include/
//! MACMSharedMemory.h format, reimplemented natively for Linux).
//!
//! Our daemon is the *consumer*: it reads /dev/shm/MACMSharedMemory,
//! inspects dwCommand (INIT / FLUSH / FLUSH_WITHOUT_APPLYING /
//! REFRESH_VF_CURVE) and applies the requested hardware values when a
//! client bumps the command field.

pub const MACM_SIGNATURE: u32 = 0x4D43414D; // 'MACM'
pub const MACM_VERSION: u32 = 0x00020000;

pub const CMD_INIT: u32 = 0x00AB0000;
pub const CMD_FLUSH: u32 = 0x00AB0001;
pub const CMD_FLUSH_WO_APPLY: u32 = 0x00AB0002;
pub const CMD_REFRESH_VF: u32 = 0x00AB0003;

// Flag namespace mirrors the MACM_SHARED_MEMORY_GPU_ENTRY_FLAG_* vocabulary.
pub const F_CORE_CLOCK: u32 = 0x00000001;
pub const F_SHADER_CLOCK: u32 = 0x00000002;
pub const F_MEMORY_CLOCK: u32 = 0x00000004;
pub const F_FAN_SPEED: u32 = 0x00000008;
pub const F_CORE_VOLTAGE: u32 = 0x00000010;
pub const F_MEMORY_VOLTAGE: u32 = 0x00000020;
pub const F_AUX_VOLTAGE: u32 = 0x00000040;
pub const F_CORE_VOLTAGE_BOOST: u32 = 0x00000080;
pub const F_MEMORY_VOLTAGE_BOOST: u32 = 0x00000100;
pub const F_AUX_VOLTAGE_BOOST: u32 = 0x00000200;
pub const F_POWER_LIMIT: u32 = 0x00000400;
pub const F_CORE_CLOCK_BOOST: u32 = 0x00000800;
pub const F_MEMORY_CLOCK_BOOST: u32 = 0x00001000;
pub const F_THERMAL_LIMIT: u32 = 0x00002000;
pub const F_THERMAL_PRIORITIZE: u32 = 0x00004000;
pub const F_AUX2_VOLTAGE: u32 = 0x00008000;
pub const F_AUX2_VOLTAGE_BOOST: u32 = 0x00010000;
pub const F_VF_CURVE: u32 = 0x00020000;
pub const F_VF_CURVE_ENABLED: u32 = 0x00040000;
pub const F_SYNC_MASTER: u32 = 0x80000000;

pub const VF_POINTS_MAX: usize = 256;
pub const VF_TUPLES_MAX: usize = 4;

pub const SHM_NAME: &str = "/dev/shm/MACMSharedMemory";
pub const SHM_ACCESS_NAME: &str = "/dev/shm/Access_MACMSharedMemory";

#[derive(Debug, Clone, Default)]
pub struct MacmGpuEntry {
    pub flags: u32,
    pub core_clock_khz: [u32; 4],   // cur min max def
    pub shader_clock_khz: [u32; 4],
    pub memory_clock_khz: [u32; 4],
    pub fan_speed_pct: u32,
    pub fan_flags: u32, // FAN_FLAG_AUTO = 0x1
    pub fan_speed_min: u32,
    pub fan_speed_max: u32,
    pub fan_speed_def: u32,
    pub fan_flags_def: u32,
    pub core_voltage_mv: [u32; 4],
    pub memory_voltage_mv: [u32; 4],
    pub aux_voltage_mv: [u32; 4],
    pub core_voltage_boost_mv: [i32; 4],
    pub memory_voltage_boost_mv: [i32; 4],
    pub aux_voltage_boost_mv: [i32; 4],
    pub power_limit_pct: [i32; 4],
    pub core_clock_boost_khz: [i32; 4],
    pub memory_clock_boost_khz: [i32; 4],
    pub thermal_limit_c: [i32; 4],
    pub thermal_prioritize: [u32; 2],
    pub aux2_voltage_mv: [u32; 4],
    pub aux2_voltage_boost_mv: [i32; 4],
    pub gpu_id: String,
    // parsed from stored vfCurve
    pub vf_lock_index: u32,
    pub vf_points: Vec<(u32, u32, i32)>, // (mV, MHz KHz, offset)
}

#[derive(Debug, Clone, Default)]
pub struct MacmImage {
    pub master_gpu: u32,
    pub flags: u32,
    pub time: u32,
    pub command: u32,
    pub gpus: Vec<MacmGpuEntry>,
}

pub const MAX_PATH: usize = 260;

// Sizes as declared by the interop header (32-bit build).
pub fn header_size() -> usize {
    9 * 4
}

pub fn gpu_entry_size() -> usize {
    let mut ints = 0usize;
    ints += 1; // flags
    ints += 4 * 3; // core/shader/memory clock groups
    ints += 6; // fan group
    ints += 3 * 4; // core/memory/aux voltage
    ints += 3 * 4; // voltage boost triples
    ints += 3; // power limit + core boost + mem boost
    ints += 1; // thermal limit
    ints += 2; // thermal prioritize
    ints += 1; // aux2 voltage
    ints += 1; // aux2 boost
    // vf curve:
    let vf = 2 + 1 + 1 // version, flags... version(1)+flags(1)+points(1)
        + VF_POINTS_MAX * 3
        + 1 // lock index
        + 1 + VF_TUPLES_MAX * 4
        + 1 + VF_TUPLES_MAX * 4;
    ints + vf * 4 + MAX_PATH // gpu id field
}

fn parse_field_u32(v: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(v[o..o + 4].try_into().unwrap())
}

fn read_vec4(v: &[u8], o: usize) -> [u32; 4] {
    [parse_field_u32(v, o), parse_field_u32(v, o + 4), parse_field_u32(v, o + 8), parse_field_u32(v, o + 12)]
}

fn read_vec4i(v: &[u8], o: usize) -> [i32; 4] {
    let r = read_vec4(v, o);
    [r[0] as i32, r[1] as i32, r[2] as i32, r[3] as i32]
}

fn cstr(v: &[u8], from: usize, len: usize) -> String {
    let s = &v[from..from + len];
    let e = s.iter().position(|c| *c == 0).unwrap_or(len);
    String::from_utf8_lossy(&s[..e]).to_string()
}

/// Parse a MACM image as written by Windows/other client tools.
pub fn read_shm() -> Option<MacmImage> {
    let b = std::fs::read(SHM_NAME).ok()?;
    if b.len() < header_size() {
        return None;
    }
    let sig = parse_field_u32(&b, 0);
    if sig != MACM_SIGNATURE {
        return None;
    }
    let mut img = MacmImage {
        master_gpu: parse_field_u32(&b, 20),
        flags: parse_field_u32(&b, 24),
        time: parse_field_u32(&b, 28),
        command: parse_field_u32(&b, 32),
        gpus: vec![],
    };
    let n = parse_field_u32(&b, 12) as usize;
    let esz = parse_field_u32(&b, 16) as usize;
    if esz < gpu_entry_size() {
        return None;
    }
    for i in 0..n {
        let o = header_size() + i * esz;
        if o + esz > b.len() {
            break;
        }
        let v = &b[o..o + esz];
        let mut e = MacmGpuEntry {
            flags: parse_field_u32(v, 0),
            ..Default::default()
        };
        let mut cur = 4;
        e.core_clock_khz = read_vec4(v, cur); cur += 16;
        e.shader_clock_khz = read_vec4(v, cur); cur += 16;
        e.memory_clock_khz = read_vec4(v, cur); cur += 16;
        e.fan_speed_pct = parse_field_u32(v, cur); cur += 4;
        e.fan_flags = parse_field_u32(v, cur); cur += 4;
        e.fan_speed_min = parse_field_u32(v, cur); cur += 4;
        e.fan_speed_max = parse_field_u32(v, cur); cur += 4;
        e.fan_speed_def = parse_field_u32(v, cur); cur += 4;
        e.fan_flags_def = parse_field_u32(v, cur); cur += 4;
        e.core_voltage_mv = read_vec4(v, cur); cur += 16;
        e.memory_voltage_mv = read_vec4(v, cur); cur += 16;
        e.aux_voltage_mv = read_vec4(v, cur); cur += 16;
        e.core_voltage_boost_mv = read_vec4i(v, cur); cur += 16;
        e.memory_voltage_boost_mv = read_vec4i(v, cur); cur += 16;
        e.aux_voltage_boost_mv = read_vec4i(v, cur); cur += 16;
        e.power_limit_pct = read_vec4i(v, cur); cur += 16;
        e.core_clock_boost_khz = read_vec4i(v, cur); cur += 16;
        e.memory_clock_boost_khz = read_vec4i(v, cur); cur += 16;
        e.thermal_limit_c = read_vec4i(v, cur); cur += 16;
        e.thermal_prioritize = [parse_field_u32(v, cur), parse_field_u32(v, cur + 4)]; cur += 8;
        e.aux2_voltage_mv = read_vec4(v, cur); cur += 16;
        e.aux2_voltage_boost_mv = read_vec4i(v, cur); cur += 16;
        // vf curve (skip contents except lock index) — derive offsets
        let vf_start = cur;
        let pf = |idx: usize| -> u32 { parse_field_u32(v, vf_start + idx * 4) };
        let points = pf(2) as usize;
        e.vf_lock_index = pf(3 + VF_POINTS_MAX * 3);
        e.vf_points = (0..points.min(VF_POINTS_MAX))
            .map(|p| {
                let bo = (3 + p * 3) * 4;
                (
                    parse_field_u32(v, vf_start + bo),
                    parse_field_u32(v, vf_start + bo + 4),
                    parse_field_u32(v, vf_start + bo + 8) as i32,
                )
            })
            .collect();
        cur = vf_start + (3 + VF_POINTS_MAX * 3 + 1 + 1 + VF_TUPLES_MAX * 4 * 2 + 0) * 4;
        e.gpu_id = cstr(v, cur, MAX_PATH);
        img.gpus.push(e);
    }
    Some(img)
}

/// Describe the requested action for the daemon.
pub enum MacmRequest {
    Init,
    Apply { gpu: usize, core_clock_boost_mhz: Option<i32>, mem_clock_boost_mhz: Option<i32>, fan_pct: Option<u32>, fan_auto: bool, thermal_limit_c: Option<i32> },
    FlushOnly,
    RefreshVf,
    None,
}

pub fn request_of(img: &MacmImage) -> MacmRequest {
    let g = img.gpus.first().map(|g| g.clone());
    match img.command {
        CMD_INIT => MacmRequest::Init,
        CMD_FLUSH | CMD_FLUSH_WO_APPLY => match g {
            Some(e) => MacmRequest::Apply {
                gpu: 0,
                core_clock_boost_mhz: if e.flags & F_CORE_CLOCK_BOOST != 0 {
                    Some((e.core_clock_boost_khz[0] as f32 / 1000.0) as i32)
                } else {
                    None
                },
                mem_clock_boost_mhz: if e.flags & F_MEMORY_CLOCK_BOOST != 0 {
                    Some((e.memory_clock_boost_khz[0] as f32 / 1000.0) as i32)
                } else {
                    None
                },
                fan_pct: if e.flags & F_FAN_SPEED != 0 && e.fan_flags & 1 == 0 {
                    Some(e.fan_speed_pct)
                } else {
                    None
                },
                fan_auto: e.flags & F_FAN_SPEED != 0 && e.fan_flags & 1 != 0,
                thermal_limit_c: if e.flags & F_THERMAL_LIMIT != 0 { Some(e.thermal_limit_c[0]) } else { None },
            },
            None => MacmRequest::None,
        },
        CMD_REFRESH_VF => MacmRequest::RefreshVf,
        0 => MacmRequest::None,
        _ => MacmRequest::None,
    }
}

/// Zero the command field so the client observes acceptance.
pub fn clear_command() {
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(SHM_NAME) {
        use std::io::{Seek, SeekFrom, Write};
        let _ = f.seek(SeekFrom::Start(32));
        let _ = f.write_all(&0u32.to_le_bytes());
    }
}

/// Touch the access sentinel so client tools notice a valid controller.
pub fn touch_access() {
    let _ = std::fs::write(SHM_ACCESS_NAME, b"ABRS");
}
