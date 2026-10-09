//! iOS-only helpers that the rest of the engine can call on every platform (on other
//! platforms they return `None` or do nothing, so callers need no `cfg`).
//!
//! * memory probes: the footprint iOS enforces, and the malloc heap in use
//! * device facts written once at the top of the log
//! * `Documents/iw4l-env.txt`: `NAME=value` lines applied as environment variables,
//!   so the engine's own switches (cache budgets, sound, present mode...) can be tried
//!   on a phone without a rebuild
//! * log rotation and a noise filter, so `iw4l-boot.log` stays readable

/// `task_info(TASK_VM_INFO)` on every Apple platform: [`footprint_bytes`] on the
/// phone, and the health log's `footprint_mb` (see `health`) on the Mac too.
#[cfg(target_vendor = "apple")]
pub(crate) mod task_vm {
    #[repr(C)]
    #[derive(Default)]
    pub struct TaskVmInfo {
        pub virtual_size: u64,
        pub region_count: i32,
        pub page_size: i32,
        pub resident_size: u64,
        pub resident_size_peak: u64,
        pub device: u64,
        pub device_peak: u64,
        pub internal: u64,
        pub internal_peak: u64,
        pub external: u64,
        pub external_peak: u64,
        pub reusable: u64,
        pub reusable_peak: u64,
        pub purgeable_volatile_pmap: u64,
        pub purgeable_volatile_resident: u64,
        pub purgeable_volatile_virtual: u64,
        pub compressed: u64,
        pub compressed_peak: u64,
        pub compressed_lifetime: u64,
        pub phys_footprint: u64,
    }

    /// The footprint the kernel enforces (GPU-backed and compressed memory included).
    pub(crate) fn phys_footprint() -> Option<u64> {
        const TASK_VM_INFO: libc::c_int = 22;
        let mut info = TaskVmInfo::default();
        let mut count =
            (size_of::<TaskVmInfo>() / size_of::<u32>()) as libc::mach_msg_type_number_t;
        // SAFETY: the struct mirrors the kernel's task_vm_info prefix; `count` bounds the write.
        #[allow(deprecated)]
        let kr = unsafe {
            libc::task_info(
                libc::mach_task_self(),
                TASK_VM_INFO as _,
                (&raw mut info).cast(),
                &raw mut count,
            )
        };
        (kr == 0).then_some(info.phys_footprint)
    }
}

#[cfg(target_os = "ios")]
mod apple {
    #[repr(C)]
    #[derive(Default)]
    pub struct MallocStats {
        pub blocks_in_use: libc::c_uint,
        pub size_in_use: libc::size_t,
        pub max_size_in_use: libc::size_t,
        pub size_allocated: libc::size_t,
    }

    unsafe extern "C" {
        pub fn malloc_zone_statistics(zone: *mut libc::c_void, stats: *mut MallocStats);
        pub fn os_proc_available_memory() -> usize;
        pub fn sysctlbyname(
            name: *const libc::c_char,
            oldp: *mut libc::c_void,
            oldlenp: *mut libc::size_t,
            newp: *mut libc::c_void,
            newlen: libc::size_t,
        ) -> libc::c_int;
    }

    pub fn sysctl_string(name: &str) -> Option<String> {
        let cname = std::ffi::CString::new(name).ok()?;
        let mut len: libc::size_t = 0;
        // SAFETY: first call only asks for the size.
        let rc = unsafe {
            sysctlbyname(cname.as_ptr(), std::ptr::null_mut(), &raw mut len, std::ptr::null_mut(), 0)
        };
        if rc != 0 || len == 0 || len > 256 {
            return None;
        }
        let mut buf = vec![0u8; len];
        // SAFETY: buffer is `len` bytes long.
        let rc = unsafe {
            sysctlbyname(cname.as_ptr(), buf.as_mut_ptr().cast(), &raw mut len, std::ptr::null_mut(), 0)
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(len);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf8(buf).ok()
    }

    pub fn sysctl_u64(name: &str) -> Option<u64> {
        let cname = std::ffi::CString::new(name).ok()?;
        let mut value: u64 = 0;
        let mut len: libc::size_t = size_of::<u64>();
        // SAFETY: `value` is 8 bytes and `len` says so.
        let rc = unsafe {
            sysctlbyname(
                cname.as_ptr(),
                (&raw mut value).cast(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0).then_some(value)
    }
}

/// The footprint iOS enforces (GPU-backed and compressed memory included), in bytes.
pub fn footprint_bytes() -> Option<u64> {
    #[cfg(target_os = "ios")]
    {
        task_vm::phys_footprint()
    }
    #[cfg(not(target_os = "ios"))]
    {
        None
    }
}

/// (malloc bytes in use, malloc bytes reserved), over all zones.
pub fn heap_bytes() -> Option<(u64, u64)> {
    #[cfg(target_os = "ios")]
    {
        let mut stats = apple::MallocStats::default();
        // SAFETY: a null zone asks for totals over all zones; the struct matches the ABI.
        unsafe { apple::malloc_zone_statistics(std::ptr::null_mut(), &raw mut stats) };
        Some((stats.size_in_use as u64, stats.size_allocated as u64))
    }
    #[cfg(not(target_os = "ios"))]
    {
        None
    }
}

pub fn heap_in_use_bytes() -> Option<u64> {
    heap_bytes().map(|(used, _)| used)
}

/// Bytes iOS still lets this process allocate.
pub fn available_bytes() -> Option<u64> {
    #[cfg(target_os = "ios")]
    {
        // SAFETY: no arguments, returns a byte count (iOS 13+).
        Some(unsafe { apple::os_proc_available_memory() } as u64)
    }
    #[cfg(not(target_os = "ios"))]
    {
        None
    }
}

/// Multi-line device description for the top of the log.
pub fn device_report(app_version: &str) -> String {
    #[cfg(target_os = "ios")]
    {
        let model = apple::sysctl_string("hw.machine").unwrap_or_else(|| "?".into());
        let os_build = apple::sysctl_string("kern.osversion").unwrap_or_else(|| "?".into());
        let os_release = apple::sysctl_string("kern.osrelease").unwrap_or_else(|| "?".into());
        let ram = apple::sysctl_u64("hw.memsize").map_or(0, |b| b / (1024 * 1024));
        let cpus = apple::sysctl_u64("hw.ncpu").unwrap_or(0);
        let avail = available_bytes().map_or(0, |b| b / (1024 * 1024));
        format!(
            "device: model={model} ram={ram}MB cpus={cpus} | darwin={os_release} build={os_build} | app={app_version} | iOS grants {avail}MB at launch"
        )
    }
    #[cfg(not(target_os = "ios"))]
    {
        format!("device: non-iOS | app={app_version}")
    }
}

/// Apply `Documents/iw4l-env.txt`: one `NAME=value` per line, `#` starts a comment.
/// Only validated, allowlisted tuning controls are accepted. Returns diagnostics.
/// Call before any thread is spawned.
pub fn apply_env_file(path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut applied = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            applied.push("ignored malformed setting (expected NAME=value)".into());
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if !crate::memory_settings::valid_file_setting(name, value) {
            applied.push("ignored unknown or invalid tuning setting".into());
            continue;
        }
        // SAFETY: documented precondition: called at startup before other threads exist.
        unsafe { std::env::set_var(name, value) };
        applied.push(format!("{name}={value}"));
    }
    applied
}

/// Move an existing log aside so a crash does not lose it when the app is reopened.
pub fn rotate_log(path: &std::path::Path) {
    if path.exists() {
        let prev = path.with_file_name("iw4l-boot.prev.log");
        let _ = std::fs::remove_file(&prev);
        let _ = std::fs::rename(path, prev);
    }
}

/// True for lines that bury the useful ones (the menu catalog logs one per material).
pub fn is_log_noise(message: &str) -> bool {
    message.starts_with("menu catalog: material ")
        || message.starts_with("menu catalog: UI image ")
        || message.starts_with("hud: namespace tree ")
}
