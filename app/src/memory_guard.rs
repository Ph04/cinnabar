//! Android memory-pressure sampling and runtime policy wiring.

use bevy::{
    prelude::{Local, Res, ResMut, Resource, Time},
    time::Real,
};
use crate::runtime::world::ClientWorld;
use launcher_host::memory::{MemoryGuard, MemoryPressureLevel, system_memory_snapshot};
use std::time::Duration;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const MIB: u64 = 1024 * 1024;

/// Bevy resource shared by view-distance, prediction and chunk-request admission.
#[derive(Resource, Default)]
pub(crate) struct Runtime {
    guard: MemoryGuard,
}

impl Runtime {
    pub(crate) fn pressure_level(&self) -> MemoryPressureLevel {
        self.guard.level()
    }

    pub(crate) fn effective_render_distance(&self, requested: u8) -> u8 {
        self.pressure_level()
            .render_distance_cap()
            .map_or(requested, |cap| requested.min(cap))
    }

    pub(crate) fn sub_chunk_request_budget(&self) -> usize {
        self.pressure_level().sub_chunk_request_budget()
    }

    pub(crate) fn prediction_job_budget(&self, requested: usize) -> usize {
        self.pressure_level().prediction_job_budget(requested)
    }

    pub(crate) fn mesh_job_budget(&self, requested: usize) -> usize {
        self.pressure_level().terrain_job_budget(requested)
    }

    pub(crate) fn light_job_budget(&self, requested: usize) -> usize {
        self.pressure_level().terrain_job_budget(requested)
    }

    pub(crate) fn render_queue_byte_limit(&self, normal_limit: u64) -> u64 {
        match self.pressure_level() {
            MemoryPressureLevel::Normal => normal_limit,
            MemoryPressureLevel::Moderate => normal_limit.min(48 * 1024 * 1024),
            MemoryPressureLevel::High => normal_limit.min(32 * 1024 * 1024),
            MemoryPressureLevel::Critical => normal_limit.min(16 * 1024 * 1024),
        }
    }
}

/// Polls on the frame thread once per second; Android JNI work is never done per frame.
pub(crate) fn sample(
    mut runtime: ResMut<Runtime>,
    time: Res<Time<Real>>,
    client_world: Option<Res<ClientWorld>>,
    mut elapsed: Local<Duration>,
) {
    let first_sample = elapsed.is_zero();
    *elapsed = elapsed.saturating_add(time.delta());
    if !first_sample && *elapsed < SAMPLE_INTERVAL {
        return;
    }
    *elapsed = Duration::ZERO;

    let Some(snapshot) = system_memory_snapshot() else {
        return;
    };
    let previous = runtime.pressure_level();
    let current = runtime.guard.observe(snapshot);
    if current == previous {
        return;
    }

    if current >= MemoryPressureLevel::High
        && previous < MemoryPressureLevel::High
        && client_world
            .as_deref()
            .is_some_and(|world| world.stream.is_some())
    {
        chunk_pipeline::WorldStream::request_worker_scratch_reclaim();
    }

    let details = (
        snapshot.total_bytes / MIB,
        snapshot.available_bytes.map(|bytes| bytes / MIB),
        snapshot.low_memory_threshold_bytes.map(|bytes| bytes / MIB),
        snapshot.process_resident_bytes.map(|bytes| bytes / MIB),
        snapshot.low_memory,
    );
    match current {
        MemoryPressureLevel::Normal | MemoryPressureLevel::Moderate => {
            bevy::log::info!(
                previous = ?previous,
                level = ?current,
                total_mib = details.0,
                available_mib = ?details.1,
                low_memory_threshold_mib = ?details.2,
                process_rss_mib = ?details.3,
                os_low_memory = details.4,
                "Cinnabar Memory Guard changed pressure level"
            );
        }
        MemoryPressureLevel::High | MemoryPressureLevel::Critical => {
            bevy::log::warn!(
                previous = ?previous,
                level = ?current,
                total_mib = details.0,
                available_mib = ?details.1,
                low_memory_threshold_mib = ?details.2,
                process_rss_mib = ?details.3,
                os_low_memory = details.4,
                "Cinnabar Memory Guard reduced world workload"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_host::memory::SystemMemorySnapshot;

    #[test]
    fn render_queue_byte_limit_descends_with_pressure() {
        let mut runtime = Runtime::default();
        let normal = 64 * 1024 * 1024;
        assert_eq!(runtime.render_queue_byte_limit(normal), normal);
        runtime.guard.observe(SystemMemorySnapshot {
            total_bytes: 4 << 30,
            available_bytes: Some(700 << 20),
            low_memory_threshold_bytes: Some(256 << 20),
            low_memory: false,
            process_resident_bytes: Some(700 << 20),
        });
        assert_eq!(runtime.pressure_level(), MemoryPressureLevel::Moderate);
        assert_eq!(runtime.render_queue_byte_limit(normal), 48 * 1024 * 1024);
        runtime.guard.observe(SystemMemorySnapshot {
            total_bytes: 4 << 30,
            available_bytes: Some(200 << 20),
            low_memory_threshold_bytes: Some(256 << 20),
            low_memory: true,
            process_resident_bytes: Some(700 << 20),
        });
        assert_eq!(runtime.render_queue_byte_limit(normal), 16 * 1024 * 1024);
    }

    #[test]
    fn render_distance_cap_is_temporary_and_never_increases_preference() {
        let mut runtime = Runtime::default();
        assert_eq!(runtime.effective_render_distance(18), 18);
        let sample = SystemMemorySnapshot {
            total_bytes: 4 << 30,
            available_bytes: Some(200 << 20),
            low_memory_threshold_bytes: Some(256 << 20),
            low_memory: true,
            process_resident_bytes: Some(700 << 20),
        };
        runtime.guard.observe(sample);
        assert_eq!(runtime.effective_render_distance(18), 5);
        assert_eq!(runtime.effective_render_distance(4), 4);
    }
}
