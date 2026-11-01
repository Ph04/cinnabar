//! Installed and currently available physical memory, plus a conservative pressure governor.

/// Installed RAM is kept separate from the volatile amount currently available to the process.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SystemMemorySnapshot {
    pub total_bytes: u64,
    pub available_bytes: Option<u64>,
    pub low_memory_threshold_bytes: Option<u64>,
    pub low_memory: bool,
    pub process_resident_bytes: Option<u64>,
}

/// Reads physical RAM without using current free memory or process memory pressure.
pub fn physical_bytes() -> u64 {
    static BYTES: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BYTES.get_or_init(|| {
        let result = read_physical_bytes();
        if result.is_none() {
            tracing::warn!(
                "physical memory unavailable; resource packs use the lowest automatic tier"
            );
        }
        result.unwrap_or(0)
    })
}

/// Android integrated-GPU devices need a conservative first-launch render recommendation.
/// Keep the real installed-memory value for resource-pack tier selection.
pub fn render_distance_recommendation_bytes() -> u64 {
    let installed = physical_bytes();
    #[cfg(target_os = "android")]
    {
        const LOW_MEMORY_DEVICE_LIMIT: u64 = 4_u64 << 30;
        if installed <= LOW_MEMORY_DEVICE_LIMIT {
            installed.min(2_u64 << 30)
        } else {
            installed
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        installed
    }
}

/// Samples volatile memory pressure. Android uses ActivityManager when available and procfs as
/// a fallback; Linux uses procfs. Other platforms currently leave the guard inactive.
pub fn system_memory_snapshot() -> Option<SystemMemorySnapshot> {
    #[cfg(target_os = "android")]
    {
        let proc_meminfo = std::fs::read_to_string("/proc/meminfo").ok();
        let android = crate::desktop::android::memory_info();
        let total_bytes = android
            .map(|values| values.0)
            .filter(|bytes| *bytes > 0)
            .or_else(|| proc_meminfo.as_deref().and_then(meminfo_total_bytes))?;
        let available_bytes = android
            .map(|values| values.1)
            .filter(|bytes| *bytes > 0)
            .or_else(|| proc_meminfo.as_deref().and_then(meminfo_available_bytes));
        let low_memory_threshold_bytes = android
            .map(|values| values.2)
            .filter(|bytes| *bytes > 0)
            .or_else(|| Some(total_bytes / 20));
        let low_memory = android.is_some_and(|values| values.3);
        return Some(SystemMemorySnapshot {
            total_bytes,
            available_bytes,
            low_memory_threshold_bytes,
            low_memory,
            process_resident_bytes: read_process_resident_bytes(),
        });
    }

    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let total_bytes = meminfo_total_bytes(&meminfo)?;
        return Some(SystemMemorySnapshot {
            total_bytes,
            available_bytes: meminfo_available_bytes(&meminfo),
            low_memory_threshold_bytes: Some(total_bytes / 20),
            low_memory: false,
            process_resident_bytes: read_process_resident_bytes(),
        });
    }

    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    {
        None
    }
}

/// Reads the kernel's byte-valued `hw.memsize` property.
#[cfg(target_os = "macos")]
fn read_physical_bytes() -> Option<u64> {
    use std::{ffi::{c_char, c_int, c_void}, mem::size_of};
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            old: *mut c_void,
            old_length: *mut usize,
            new: *mut c_void,
            new_length: usize,
        ) -> c_int;
    }
    let mut bytes = 0u64;
    let mut length = size_of::<u64>();
    // SAFETY: the name is NUL-terminated and `bytes` has the `length` the kernel may write.
    let result = unsafe {
        sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut bytes).cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && length == size_of::<u64>()).then_some(bytes)
}

#[cfg(target_os = "linux")]
fn read_physical_bytes() -> Option<u64> {
    meminfo_total_bytes(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

#[cfg(target_os = "android")]
fn read_physical_bytes() -> Option<u64> {
    crate::desktop::android::memory_info()
        .map(|values| values.0)
        .filter(|bytes| *bytes > 0)
        .or_else(|| meminfo_total_bytes(&std::fs::read_to_string("/proc/meminfo").ok()?))
}

/// Reads a KiB-valued procfs field such as `MemTotal`, `MemAvailable` or `VmRSS`.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn meminfo_value_bytes(meminfo: &str, field: &str) -> Option<u64> {
    let prefix = format!("{field}:");
    let line = meminfo
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))?;
    line.split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
fn meminfo_total_bytes(meminfo: &str) -> Option<u64> {
    meminfo_value_bytes(meminfo, "MemTotal")
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
fn meminfo_available_bytes(meminfo: &str) -> Option<u64> {
    meminfo_value_bytes(meminfo, "MemAvailable")
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
fn process_status_resident_bytes(status: &str) -> Option<u64> {
    meminfo_value_bytes(status, "VmRSS")
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_process_resident_bytes() -> Option<u64> {
    process_status_resident_bytes(&std::fs::read_to_string("/proc/self/status").ok()?)
}

#[cfg(target_os = "windows")]
fn read_physical_bytes() -> Option<u64> {
    crate::desktop::windows::physical_memory()
}

/// Unsupported platforms retain the lowest automatic tier until a native reader exists.
#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
)))]
fn read_physical_bytes() -> Option<u64> {
    None
}

/// The guard's pressure state. More severe levels apply progressively tighter admission limits.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum MemoryPressureLevel {
    #[default]
    Normal,
    Moderate,
    High,
    Critical,
}

impl MemoryPressureLevel {
    /// Temporary distance ceilings; player settings are never overwritten.
    pub const fn render_distance_cap(self) -> Option<u8> {
        match self {
            Self::Normal => None,
            Self::Moderate => Some(10),
            Self::High => Some(8),
            Self::Critical => Some(5),
        }
    }

    /// Limits how many new sub-chunk intents can be sent in one frame.
    pub const fn sub_chunk_request_budget(self) -> usize {
        match self {
            Self::Normal => 16,
            Self::Moderate => 12,
            Self::High => 6,
            Self::Critical => 2,
        }
    }

    /// Scales optional prediction dispatch without starving a non-empty budget completely.
    pub fn prediction_job_budget(self, requested: usize) -> usize {
        if requested == 0 {
            return 0;
        }
        match self {
            Self::Normal => requested,
            Self::Moderate => (requested.saturating_mul(3) / 4).max(1),
            Self::High => (requested / 2).max(1),
            Self::Critical => (requested / 4).max(1),
        }
    }

    /// Reduces newly admitted terrain geometry and lighting jobs during memory pressure.
    pub fn terrain_job_budget(self, requested: usize) -> usize {
        if requested == 0 {
            return 0;
        }
        match self {
            Self::Normal => requested,
            Self::Moderate => (requested.saturating_mul(3) / 4).max(1),
            Self::High => (requested / 2).max(1),
            Self::Critical => (requested / 4).max(1),
        }
    }

    const fn one_step_recovery(self) -> Self {
        match self {
            Self::Critical => Self::High,
            Self::High => Self::Moderate,
            Self::Moderate | Self::Normal => Self::Normal,
        }
    }
}

/// Hysteretic memory governor. Escalation is immediate; recovery requires eight healthy samples
/// and descends one level at a time to avoid repeatedly raising and lowering workload.
#[derive(Clone, Debug, Default)]
pub struct MemoryGuard {
    level: MemoryPressureLevel,
    recovery_samples: u8,
}

impl MemoryGuard {
    pub const fn level(&self) -> MemoryPressureLevel {
        self.level
    }

    pub fn observe(&mut self, sample: SystemMemorySnapshot) -> MemoryPressureLevel {
        if sample.total_bytes == 0 {
            return self.level;
        }
        let target = pressure_level(sample);
        if target > self.level {
            self.level = target;
            self.recovery_samples = 0;
        } else if target < self.level {
            self.recovery_samples = self.recovery_samples.saturating_add(1);
            if self.recovery_samples >= 8 {
                self.level = self.level.one_step_recovery();
                self.recovery_samples = 0;
            }
        } else {
            self.recovery_samples = 0;
        }
        self.level
    }
}

/// Combines OS headroom signals with this process's resident set instead of trusting one metric.
fn pressure_level(sample: SystemMemorySnapshot) -> MemoryPressureLevel {
    let total = sample.total_bytes;
    let threshold = sample
        .low_memory_threshold_bytes
        .filter(|bytes| *bytes > 0)
        .unwrap_or(total / 20);
    let available = sample.available_bytes;
    let resident = sample.process_resident_bytes;

    let critical_available = threshold.max(total / 24);
    let high_available = threshold.saturating_mul(2).max(total / 12);
    let moderate_available = threshold.saturating_mul(3).max(total / 6);

    let critical = sample.low_memory
        || available.is_some_and(|bytes| bytes <= critical_available)
        || resident.is_some_and(|bytes| bytes >= total.saturating_mul(3) / 5);
    if critical {
        return MemoryPressureLevel::Critical;
    }
    let high = available.is_some_and(|bytes| bytes <= high_available)
        || resident.is_some_and(|bytes| bytes >= total.saturating_mul(48) / 100);
    if high {
        return MemoryPressureLevel::High;
    }
    let moderate = available.is_some_and(|bytes| bytes <= moderate_available)
        || resident.is_some_and(|bytes| bytes >= total.saturating_mul(38) / 100);
    if moderate {
        return MemoryPressureLevel::Moderate;
    }
    MemoryPressureLevel::Normal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procfs_values_are_read_in_kib() {
        let meminfo = "MemFree: 1000 kB\nMemTotal: 16318060 kB\nMemAvailable: 8192 kB\n";
        assert_eq!(meminfo_total_bytes(meminfo), Some(16_318_060 * 1024));
        assert_eq!(meminfo_available_bytes(meminfo), Some(8192 * 1024));
        assert_eq!(meminfo_total_bytes("MemFree: 1 kB\n"), None);
        assert_eq!(meminfo_total_bytes("MemTotal: lots kB\n"), None);
        assert_eq!(process_status_resident_bytes("VmRSS: 1234 kB\n"), Some(1234 * 1024));
    }

    #[test]
    fn installed_ram_is_not_mistaken_for_free_memory() {
        let sample = SystemMemorySnapshot {
            total_bytes: 4 << 30,
            available_bytes: Some(3 << 30),
            low_memory_threshold_bytes: Some(256 << 20),
            low_memory: false,
            process_resident_bytes: Some(512 << 20),
        };
        let mut guard = MemoryGuard::default();
        assert_eq!(guard.observe(sample), MemoryPressureLevel::Normal);
        assert_eq!(guard.level(), MemoryPressureLevel::Normal);
    }

    #[test]
    fn process_rss_and_android_low_memory_signal_escalate_pressure() {
        let mut guard = MemoryGuard::default();
        let sample = SystemMemorySnapshot {
            total_bytes: 4 << 30,
            available_bytes: Some(2 << 30),
            low_memory_threshold_bytes: Some(256 << 20),
            low_memory: false,
            process_resident_bytes: Some(2 << 30),
        };
        assert_eq!(guard.observe(sample), MemoryPressureLevel::High);
        let low_memory = SystemMemorySnapshot { low_memory: true, ..sample };
        assert_eq!(guard.observe(low_memory), MemoryPressureLevel::Critical);
    }

    #[test]
    fn terrain_job_budget_only_reduces_requested_work() {
        assert_eq!(MemoryPressureLevel::Normal.terrain_job_budget(8), 8);
        assert_eq!(MemoryPressureLevel::Moderate.terrain_job_budget(8), 6);
        assert_eq!(MemoryPressureLevel::High.terrain_job_budget(8), 4);
        assert_eq!(MemoryPressureLevel::Critical.terrain_job_budget(8), 2);
        assert_eq!(MemoryPressureLevel::Critical.terrain_job_budget(1), 1);
        assert_eq!(MemoryPressureLevel::Critical.terrain_job_budget(0), 0);
    }

    #[test]
    fn recovery_is_delayed_and_descends_one_level_at_a_time() {
        let mut guard = MemoryGuard::default();
        let pressured = SystemMemorySnapshot {
            total_bytes: 4 << 30,
            available_bytes: Some(200 << 20),
            low_memory_threshold_bytes: Some(256 << 20),
            low_memory: false,
            process_resident_bytes: Some(700 << 20),
        };
        let healthy = SystemMemorySnapshot {
            available_bytes: Some(3 << 30),
            process_resident_bytes: Some(512 << 20),
            ..pressured
        };
        assert_eq!(guard.observe(pressured), MemoryPressureLevel::Critical);
        for _ in 0..7 {
            assert_eq!(guard.observe(healthy), MemoryPressureLevel::Critical);
        }
        assert_eq!(guard.observe(healthy), MemoryPressureLevel::High);
        for _ in 0..8 {
            guard.observe(healthy);
        }
        assert_eq!(guard.level(), MemoryPressureLevel::Moderate);
        for _ in 0..8 {
            guard.observe(healthy);
        }
        assert_eq!(guard.level(), MemoryPressureLevel::Normal);
    }

    #[test]
    fn unknown_snapshots_do_not_change_the_guard_state() {
        let mut guard = MemoryGuard::default();
        guard.observe(SystemMemorySnapshot {
            total_bytes: 4 << 30,
            available_bytes: Some(128 << 20),
            low_memory_threshold_bytes: Some(256 << 20),
            low_memory: false,
            process_resident_bytes: None,
        });
        assert_eq!(
            guard.observe(SystemMemorySnapshot::default()),
            MemoryPressureLevel::Critical
        );
    }
}
