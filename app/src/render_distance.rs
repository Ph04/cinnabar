//! Live view-radius requests. The requested radius sets the render distance whatever the server
//! grants; the server's response only widens what residency keeps.

use crate::runtime::network::resource_packs::PackApplication;
use crate::runtime::world::ClientWorld;
use bevy::prelude::{Local, Res, ResMut};
use client_runtime::network::NetworkHandle;
use client_shell::ui_resources::UiRuntimeResource;
use client_state::RuntimeSettings;

/// Sends the selected radius after joining and whenever it changes, retrying backpressure.
pub(crate) fn apply_render_distance(
    settings: Res<RuntimeSettings>,
    ui: Option<Res<UiRuntimeResource>>,
    world: Option<ResMut<ClientWorld>>,
    network: Option<Res<NetworkHandle<PackApplication>>>,
    #[cfg(not(target_arch = "wasm32"))]
    memory_guard: Option<Res<crate::memory_guard::Runtime>>,
    mut sent: Local<Option<(u64, u8)>>,
) {
    let (Some(ui), Some(mut world), Some(network)) = (ui, world, network) else {
        return;
    };
    let radius = settings
        .user_settings_update()
        .1
        .video
        .render_distance_chunks;
    #[cfg(not(target_arch = "wasm32"))]
    let effective_radius = memory_guard
        .as_deref()
        .map_or(radius, |guard| guard.effective_render_distance(radius));
    #[cfg(target_arch = "wasm32")]
    let effective_radius = radius;

    let Some(requested) = world
        .stream
        .as_ref()
        .filter(|_| ui.session_id() != 0)
        .map(chunk_pipeline::WorldStream::requested_radius)
    else {
        *sent = None;
        return;
    };
    // The temporary cap never writes over the player's saved graphics preference.
    if requested != Some(i32::from(effective_radius))
        && let Some(stream) = world.stream.as_mut()
    {
        stream.set_requested_radius(i32::from(effective_radius));
    }
    let revision = (ui.session_id(), effective_radius);
    if *sent == Some(revision) {
        return;
    }
    let packet = protocol::request_chunk_radius_packet(
        effective_radius,
        render_api::MAX_VIEW_RADIUS_CHUNKS as u8,
    );
    if network.send_settings_packet(revision.0, packet).is_ok() {
        *sent = Some(revision);
    }
}
