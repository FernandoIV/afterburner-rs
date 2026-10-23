//! NVML fan control, independent of X11/Wayland. No NVIDIA SDK dependency.
use std::ffi::{c_char, c_int, c_void, CStr, CString};

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(lib: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlclose(lib: *mut c_void) -> c_int;
}

#[derive(Debug)]
pub struct Error {
    pub code: i32,
    pub message: String,
}

struct Nvml {
    lib: *mut c_void,
    shutdown: unsafe extern "C" fn() -> i32,
}

impl Nvml {
    fn open() -> Result<Self, Error> {
        unsafe {
            let lib = dlopen(c"libnvidia-ml.so.1".as_ptr(), 2);
            if lib.is_null() {
                return Err(error(-1, "NVML is not available"));
            }
            let init = dlsym(lib, c"nvmlInit_v2".as_ptr());
            let shutdown = dlsym(lib, c"nvmlShutdown".as_ptr());
            if init.is_null() || shutdown.is_null() {
                dlclose(lib);
                return Err(error(-1, "NVML initialization functions are not available"));
            }
            let init: unsafe extern "C" fn() -> i32 = std::mem::transmute(init);
            let r = init();
            if r != 0 {
                dlclose(lib);
                return Err(error(r, "Could not initialize NVML"));
            }
            Ok(Self { lib, shutdown: std::mem::transmute(shutdown) })
        }
    }

    fn symbol(&self, name: &CStr) -> Result<*mut c_void, Error> {
        let p = unsafe { dlsym(self.lib, name.as_ptr()) };
        if p.is_null() { Err(error(13, "The driver does not expose the required NVML fan-control function")) } else { Ok(p) }
    }

    fn check(&self, r: i32) -> Result<(), Error> {
        if r == 0 { return Ok(()); }
        let p = unsafe { dlsym(self.lib, c"nvmlErrorString".as_ptr()) };
        let msg = if p.is_null() { format!("NVML error {r}") } else {
            let f: unsafe extern "C" fn(i32) -> *const c_char = unsafe { std::mem::transmute(p) };
            unsafe { CStr::from_ptr(f(r)).to_string_lossy().into_owned() }
        };
        Err(Error { code: r, message: msg })
    }

    fn device(&self, pci: &str) -> Result<*mut c_void, Error> {
        let addr = CString::new(pci).map_err(|_| error(-1, "Invalid PCI address"))?;
        let f: unsafe extern "C" fn(*const c_char, *mut *mut c_void) -> i32 =
            unsafe { std::mem::transmute(self.symbol(c"nvmlDeviceGetHandleByPciBusId_v2")?) };
        let mut d = std::ptr::null_mut();
        self.check(unsafe { f(addr.as_ptr(), &mut d) })?;
        Ok(d)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) { unsafe { (self.shutdown)(); dlclose(self.lib); } }
}

fn error(code: i32, message: &str) -> Error { Error { code, message: message.into() } }

pub fn limits(pci: &str) -> Result<(u32, u32), Error> {
    let n = Nvml::open()?;
    let d = n.device(pci)?;
    let f: unsafe extern "C" fn(*mut c_void, *mut u32, *mut u32) -> i32 =
        unsafe { std::mem::transmute(n.symbol(c"nvmlDeviceGetMinMaxFanSpeed")?) };
    let (mut lo, mut hi) = (0, 100);
    n.check(unsafe { f(d, &mut lo, &mut hi) })?;
    Ok((lo, hi))
}

/// Applies only to the fans belonging to this exact PCI device.
/// None (and zero) returns control to firmware, preserving zero-RPM support.
pub fn set(pci: &str, percent: Option<u32>) -> Result<String, Error> {
    if percent.is_some_and(|p| p > 100) { return Err(error(-1, "Fan speed out of range (0–100 %)")); }
    let n = Nvml::open()?;
    let d = n.device(pci)?;
    let count: unsafe extern "C" fn(*mut c_void, *mut u32) -> i32 =
        unsafe { std::mem::transmute(n.symbol(c"nvmlDeviceGetNumFans")?) };
    let default: unsafe extern "C" fn(*mut c_void, u32) -> i32 =
        unsafe { std::mem::transmute(n.symbol(c"nvmlDeviceSetDefaultFanSpeed_v2")?) };
    let manual: unsafe extern "C" fn(*mut c_void, u32, u32) -> i32 =
        unsafe { std::mem::transmute(n.symbol(c"nvmlDeviceSetFanSpeed_v2")?) };
    let mut fans = 0;
    n.check(unsafe { count(d, &mut fans) })?;
    if fans == 0 || fans > 32 { return Err(error(3, "The GPU has no controllable fans")); }
    let percent = percent.filter(|p| *p > 0);
    let p = percent.map(|p| {
        let (lo, hi) = limits(pci).ok().filter(|(lo,hi)|lo<=hi && *hi<=100).unwrap_or((30, 100));
        p.clamp(lo, hi)
    });
    for i in 0..fans {
        let result = unsafe { match p { Some(p) => manual(d, i, p), None => default(d, i) } };
        if result != 0 {
            // Never leave only half the fans in manual after a partial failure.
            if i > 0 { for j in 0..fans { unsafe { default(d, j); } } }
            n.check(result)?;
        }
    }
    Ok(match p {
        Some(p) => format!("{fans} fans set to {p} % through NVML"),
        None => format!("Automatic control restored on {fans} fans"),
    })
}
