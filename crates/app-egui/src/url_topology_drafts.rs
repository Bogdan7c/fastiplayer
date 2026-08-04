//! App-owned mapping из bounded yt-dlp topology в нейтральные ID-less playlist drafts.
//!
//! Модуль намеренно не знает о `PlaylistQueue`, allocator-е и commit transaction.
//! S17 подключит этот чистый boundary к process-lifetime Add URL orchestration.

#![allow(dead_code)] // S16 строит boundary заранее; production consumer появляется в S17.

use playlist_core::{DurableReopenLocator, SecretUrlLocator};
use service_ytdlp::{YtDlpMediaLocator, YtDlpTopology};

mod mapper;
mod model;
mod service_adapter;

use mapper::map_topology_node;
use model::{
    TopologyDraftMappingBudgets, TopologyIdentityView, TopologyMappingNode,
    TopologyNodeDescription, TopologySummaryView, YtDlpTopologyDraftIssue,
    YtDlpTopologyDraftIssueKind, YtDlpTopologyDraftMappingError, YtDlpTopologyDraftPreview,
};
use service_adapter::ServiceTopologyNode;

/// Преобразует уже извлечённую topology без queue commit и без второго URL parser-а.
pub(crate) fn map_yt_dlp_topology_to_playlist_drafts(
    exact_root_locator: &YtDlpMediaLocator,
    topology: &YtDlpTopology,
) -> Result<YtDlpTopologyDraftPreview, YtDlpTopologyDraftMappingError> {
    // Берём exact bytes только из intent-named service persistence boundary.
    let root_url =
        SecretUrlLocator::from_reopenable_url(exact_root_locator.expose_secret_for_persistence())?;
    // Root остаётся обычным exact locator: service payload нужен только extracted children.
    let durable_root_locator = DurableReopenLocator::url(root_url);
    // Production budgets совпадают с hard neutral playlist/domain limits.
    let budgets = TopologyDraftMappingBudgets::production();
    // Адаптер открывает mapper-у только intent getters service topology.
    let root_node = ServiceTopologyNode::Root(topology);

    Ok(map_topology_node(&root_node, durable_root_locator, budgets))
}

#[cfg(test)]
mod tests;
