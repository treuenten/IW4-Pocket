//! Small, restart-only memory controls shared by loading, rendering and reports.
//! Environment is read once, after the iOS settings file and before loading.
use std::sync::OnceLock;

pub const FPV_CACHE_ENV: &str = "IW4L_FPV_RETAIN_MIB";
pub const SHADER_WORKERS_ENV: &str = "IW4L_SHADER_WORKERS";
pub const MOVE_IMAGES_ENV: &str = "IW4L_MOVE_IMAGES";
pub const RESIDENT_MAP_ENV: &str = "IW4L_RESIDENT_MAP";
pub const IOS_BC_ENV: &str = "IW4L_IOS_BC";

#[derive(Clone, Copy, Debug)]
pub struct MemorySettings {
    /// Optional next-map FPV payload cache, not active weapon data or donor batches.
    pub fpv_retain_bytes: u64,
    /// Zero means use the existing load pool width (desktop default).
    pub shader_workers: usize,
    pub move_images: bool,
    /// Keep the walked match for an instant same-map reload. It shares every
    /// decoded image, so their texels cannot move to the GPU (off on iOS).
    pub resident_map: bool,
    /// Keep BC textures compressed on iOS when the GPU samples BC.
    pub ios_bc: bool,
}

fn flag(value: Option<&str>, default: bool) -> bool {
    match value.map(str::trim) {
        Some("0") => false,
        Some("1") => true,
        _ => default,
    }
}

fn bounded(value: Option<&str>, default: u64, min: u64, max: u64) -> u64 {
    value.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| (min..=max).contains(n)).unwrap_or(default)
}

impl MemorySettings {
    fn parse(
        ios: bool,
        fpv: Option<&str>,
        shaders: Option<&str>,
        moves: Option<&str>,
        resident: Option<&str>,
        bc: Option<&str>,
    ) -> Self {
        let default_fpv = if ios { 0 } else { u64::MAX };
        let fpv_retain_bytes = fpv.and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n <= 1024).map(|n| n * 1024 * 1024).unwrap_or(default_fpv);
        Self {
            fpv_retain_bytes,
            shader_workers: bounded(shaders, if ios { 1 } else { 0 }, 1, 64) as usize,
            move_images: match moves.map(str::trim) {
                Some("0") => false,
                Some("1") => true,
                _ => true,
            },
            resident_map: flag(resident, !ios),
            ios_bc: flag(bc, true),
        }
    }

    pub fn effective_shader_workers(self, pool_width: usize) -> usize {
        let pool_width = pool_width.max(1);
        if self.shader_workers == 0 { pool_width } else { self.shader_workers.min(pool_width) }
    }

    pub fn report(self) -> String {
        let fpv = if self.fpv_retain_bytes == u64::MAX {
            "unlimited (desktop default)".to_owned()
        } else {
            (self.fpv_retain_bytes / (1024 * 1024)).to_string()
        };
        format!("memory settings: {FPV_CACHE_ENV}={fpv} MiB; {SHADER_WORKERS_ENV}={} requested (0=pool); {MOVE_IMAGES_ENV}={}; {RESIDENT_MAP_ENV}={}; {IOS_BC_ENV}={}",
            self.shader_workers, u8::from(self.move_images), u8::from(self.resident_map),
            u8::from(self.ios_bc))
    }
}

pub fn get() -> &'static MemorySettings {
    static SETTINGS: OnceLock<MemorySettings> = OnceLock::new();
    SETTINGS.get_or_init(|| MemorySettings::parse(
        cfg!(target_os = "ios"),
        std::env::var(FPV_CACHE_ENV).ok().as_deref(),
        std::env::var(SHADER_WORKERS_ENV).ok().as_deref(),
        std::env::var(MOVE_IMAGES_ENV).ok().as_deref(),
        std::env::var(RESIDENT_MAP_ENV).ok().as_deref(),
        std::env::var(IOS_BC_ENV).ok().as_deref(),
    ))
}

/// No overflow and no partial payloads. Zero really disables retention.
pub fn admits_bytes(held: u64, next: u64, limit: u64) -> bool {
    limit != 0 && next <= limit.saturating_sub(held) && held <= limit
}

/// Only these restart-time tuning switches may be set from the iOS text file.
/// Paths, external endpoints and arbitrary engine controls are deliberately excluded.
pub fn valid_file_setting(name: &str, value: &str) -> bool {
    match name {
        FPV_CACHE_ENV => value.parse::<u64>().is_ok_and(|n| n <= 1024),
        SHADER_WORKERS_ENV => value.parse::<u64>().is_ok_and(|n| (1..=64).contains(&n)),
        MOVE_IMAGES_ENV | RESIDENT_MAP_ENV | IOS_BC_ENV => matches!(value, "0" | "1"),
        "IW4L_IMAGE_DECODE_BUDGET_MIB" | "IW4L_CACHE_BUDGET_MIB" =>
            value.parse::<u64>().is_ok_and(|n| n <= 4096),
        "IW4L_SOUND" => matches!(value, "off" | "0" | "on" | "1"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn platform_defaults_and_invalid_values() {
        let ios = MemorySettings::parse(true, None, None, None, None, None);
        assert_eq!(ios.fpv_retain_bytes, 0);
        assert_eq!(ios.effective_shader_workers(4), 1);
        assert!(ios.move_images);
        assert!(!ios.resident_map);
        assert!(ios.ios_bc);
        let invalid = MemorySettings::parse(true, Some("-1"), Some("0"), Some("bad"), Some("bad"), Some("bad"));
        assert_eq!(invalid.fpv_retain_bytes, 0);
        assert_eq!(invalid.shader_workers, 1);
        assert!(!invalid.resident_map);
        assert!(invalid.ios_bc);
        let desktop = MemorySettings::parse(false, None, None, None, None, None);
        assert_eq!(desktop.fpv_retain_bytes, u64::MAX);
        assert_eq!(desktop.effective_shader_workers(4), 4);
        assert!(desktop.resident_map);
    }
    #[test]
    fn ab_settings_are_bounded() {
        let s = MemorySettings::parse(true, Some("64"), Some("64"), Some("0"), Some("1"), Some("0"));
        assert_eq!(s.fpv_retain_bytes, 64 * 1024 * 1024);
        assert_eq!(s.effective_shader_workers(4), 4);
        assert_eq!(s.effective_shader_workers(0), 1);
        assert!(!s.move_images);
        assert!(s.resident_map);
        assert!(!s.ios_bc);
        assert_eq!(MemorySettings::parse(true, Some("18446744073709551615"), Some("65"), None, None, None).fpv_retain_bytes, 0);
    }
    #[test]
    fn budget_boundaries() {
        assert!(!admits_bytes(0, 0, 0));
        assert!(admits_bytes(32, 32, 64));
        assert!(!admits_bytes(32, 33, 64));
        assert!(!admits_bytes(65, 0, 64));
        assert!(!admits_bytes(u64::MAX - 1, 2, u64::MAX));
    }
    #[test]
    fn file_allowlist_rejects_paths_unknowns_and_nuls() {
        assert!(valid_file_setting(FPV_CACHE_ENV, "0"));
        assert!(valid_file_setting(SHADER_WORKERS_ENV, "4"));
        assert!(valid_file_setting(RESIDENT_MAP_ENV, "1"));
        assert!(valid_file_setting(IOS_BC_ENV, "0"));
        assert!(!valid_file_setting(IOS_BC_ENV, "yes"));
        assert!(!valid_file_setting(SHADER_WORKERS_ENV, "0"));
        assert!(!valid_file_setting("IW4L_GAMES", "/tmp"));
        assert!(!valid_file_setting("IW4L_UNKNOWN", "1"));
        assert!(!valid_file_setting(FPV_CACHE_ENV, "1\0"));
        assert!(!valid_file_setting("IW4L_\0", "1"));
    }
}
