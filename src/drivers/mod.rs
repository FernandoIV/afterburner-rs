//! Backend driver abstraction — mirrors the HAL layer of the Windows
//! version (RTHAL.dll routes through NVAPI / ATIO ADL / Intel IGCL; the
//! Linux port routes through NVML + nvidia-cli, amdgpu sysfs and Intel hwmon).

pub mod amd;
pub mod intel;
pub mod nvidia;
pub mod nvfan;

#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub temp_core: Option<f32>,
    pub temp_mem: Option<f32>,
    pub fan_pct: Option<f32>,
    pub fan_rpm: Option<f32>,
    pub clk_core: Option<f32>,
    pub clk_mem: Option<f32>,
    pub usage_gpu: Option<f32>,
    pub usage_mem: Option<f32>,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub voltage_mv: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct Limits {
    pub power_min_w: Option<f32>,
    pub power_max_w: Option<f32>,
    pub power_default_w: Option<f32>,
    pub clk_core_offset_min: Option<i32>,
    pub clk_core_offset_max: Option<i32>,
    pub clk_mem_offset_min: Option<i32>,
    pub clk_mem_offset_max: Option<i32>,
}

#[derive(Debug, Clone, Default)]
pub struct ApplySettings {
    pub core_offset_mhz: Option<i32>,
    pub mem_offset_mhz: Option<i32>,
    /// new-style locked clock ranges (NVIDIA `nvidia-smi -lgc/-lmc`)
    pub lock_core_clock: Option<(u32, u32)>,
    pub lock_mem_clock: Option<(u32, u32)>,
    pub power_limit_w: Option<f32>,
    pub fan_pct: Option<u32>,
    pub fan_auto: bool,
    pub thermal_limit_c: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct AppliedReport {
    pub core_offset: Option<i32>,
    pub mem_offset: Option<i32>,
    pub power_limit: Option<f32>,
    pub fan: (Option<u32>, bool), // (pct, auto)
    pub notes: Vec<String>,
}

pub trait GpuDriver {
    fn sample(&mut self) -> Sample;
    fn limits(&mut self) -> Limits;
    fn apply(&mut self, s: &ApplySettings) -> AppliedReport;
    fn reset(&mut self) -> AppliedReport;
    /// Détail capability flags (emulation of MACM flag bitmask semantics).
    fn caps(&mut self) -> Caps;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    /// equivalent of MACM_SHARED_MEMORY_GPU_ENTRY_FLAG_CORE_CLOCK_BOOST
    pub core_clock_offset: bool,
    pub mem_clock_offset: bool,
    pub power_limit: bool,
    pub core_voltage: bool,
    pub fan: bool,
    pub fan_auto: bool,
    pub thermal_limit: bool,
    pub vfs_curve: bool,
}
