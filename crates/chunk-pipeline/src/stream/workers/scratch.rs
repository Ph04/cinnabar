//! Storage retained only by a world worker, never by queued jobs or completed results.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static RECLAIM_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Requests every world worker to release its retained scratch after its current job.
pub(super) fn request_reclaim() -> u64 {
    RECLAIM_GENERATION.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
}

/// Quiet interval before a worker releases temporary mesh allocations.
pub(super) const SCRATCH_IDLE_RELEASE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct WorkerScratch {
    pub(in crate::stream) light: world::LightSolverScratch,
    pub(in crate::stream) mesh: meshing::MeshScratch,
    finished_at: Option<Moment>,
    reclaim_generation: u64,
}

impl WorkerScratch {
    /// Remaining idle grace, or no timeout when mesh storage has already been released.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn idle_wait(&self, now: Moment) -> Option<Duration> {
        if self.reclaim_generation != RECLAIM_GENERATION.load(Ordering::Acquire) {
            return Some(Duration::ZERO);
        }
        self.finished_at.map(|finished| {
            SCRATCH_IDLE_RELEASE.saturating_sub(now.saturating_duration_since(finished))
        })
    }

    /// Records job completion using the executor's monotonic observation.
    pub(super) fn finished(&mut self, now: Moment) {
        self.finished_at = Some(now);
    }

    /// Releases mesh capacities after a quiet interval; lighting keeps its existing policy.
    pub(super) fn release_if_idle(&mut self, now: Moment) {
        let reclaim_generation = RECLAIM_GENERATION.load(Ordering::Acquire);
        if self.reclaim_generation != reclaim_generation {
            // This runs only on the owning worker after a job or a wait, never concurrently
            // with the worker's meshing or lighting code.
            self.mesh = meshing::MeshScratch::default();
            self.light = world::LightSolverScratch::default();
            self.reclaim_generation = reclaim_generation;
            self.finished_at = None;
            return;
        }
        if self
            .finished_at
            .is_some_and(|finished| now.saturating_duration_since(finished) >= SCRATCH_IDLE_RELEASE)
        {
            self.mesh = meshing::MeshScratch::default();
            self.finished_at = None;
        }
    }
}
