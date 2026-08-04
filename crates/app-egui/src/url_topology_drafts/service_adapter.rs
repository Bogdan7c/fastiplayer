//! Zero-allocation adapter от public service-ytdlp topology к intent mapping contract.

use service_ytdlp::{
    YtDlpTopology, YtDlpTopologyEntry, YtDlpTopologyIdentity, YtDlpTopologySummary,
};

use super::{
    TopologyIdentityView, TopologyMappingNode, TopologyNodeDescription, TopologySummaryView,
};

impl<'identity> From<&'identity YtDlpTopologyIdentity> for TopologyIdentityView<'identity> {
    fn from(identity: &'identity YtDlpTopologyIdentity) -> Self {
        Self {
            extractor_id: identity.extractor_id(),
            extractor_key: identity.extractor_key(),
            webpage_locator: identity.webpage_locator(),
            original_locator: identity.original_locator(),
        }
    }
}

impl<'summary> From<&'summary YtDlpTopologySummary> for TopologySummaryView<'summary> {
    fn from(summary: &'summary YtDlpTopologySummary) -> Self {
        Self {
            title: summary.title(),
            duration: summary.duration(),
        }
    }
}

/// Zero-allocation wrapper над public service topology/root entry enums.
#[derive(Clone, Copy)]
pub(super) enum ServiceTopologyNode<'topology> {
    /// Authoritative root result.
    Root(&'topology YtDlpTopology),
    /// Один retained child.
    Entry(&'topology YtDlpTopologyEntry),
}

impl TopologyMappingNode for ServiceTopologyNode<'_> {
    fn describe(&self) -> TopologyNodeDescription<'_> {
        match self {
            Self::Root(YtDlpTopology::Video(video)) => TopologyNodeDescription::Video {
                identity: video.identity().into(),
                metadata: video.summary().into(),
            },
            Self::Root(YtDlpTopology::Playlist(_)) => TopologyNodeDescription::Collection,
            Self::Root(YtDlpTopology::MultiVideo(multi_video)) => {
                TopologyNodeDescription::MultiVideo {
                    identity: multi_video.root_video().identity().into(),
                    metadata: multi_video.root_video().summary().into(),
                }
            }
            Self::Root(YtDlpTopology::Delegation(delegation)) => {
                TopologyNodeDescription::Delegation {
                    target: delegation.target(),
                    metadata: delegation.wrapper_summary().into(),
                }
            }
            Self::Entry(YtDlpTopologyEntry::Video(video)) => TopologyNodeDescription::Video {
                identity: video.identity().into(),
                metadata: video.summary().into(),
            },
            Self::Entry(YtDlpTopologyEntry::Playlist(_)) => TopologyNodeDescription::Collection,
            Self::Entry(YtDlpTopologyEntry::MultiVideo(multi_video)) => {
                TopologyNodeDescription::MultiVideo {
                    identity: multi_video.root_video().identity().into(),
                    metadata: multi_video.root_video().summary().into(),
                }
            }
            Self::Entry(YtDlpTopologyEntry::Delegation(delegation)) => {
                TopologyNodeDescription::Delegation {
                    target: delegation.target(),
                    metadata: delegation.wrapper_summary().into(),
                }
            }
            Self::Entry(YtDlpTopologyEntry::Unavailable(unavailable)) => {
                TopologyNodeDescription::Unavailable {
                    identity: unavailable.identity().into(),
                    metadata: unavailable.summary().into(),
                }
            }
        }
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&Self)) {
        match self {
            Self::Root(YtDlpTopology::Playlist(collection))
            | Self::Entry(YtDlpTopologyEntry::Playlist(collection)) => {
                for entry in collection.iter_entries() {
                    let child = Self::Entry(entry);
                    visitor(&child);
                }
            }
            Self::Root(YtDlpTopology::MultiVideo(multi_video))
            | Self::Entry(YtDlpTopologyEntry::MultiVideo(multi_video)) => {
                for entry in multi_video.iter_entries() {
                    let child = Self::Entry(entry);
                    visitor(&child);
                }
            }
            Self::Root(YtDlpTopology::Video(_) | YtDlpTopology::Delegation(_))
            | Self::Entry(
                YtDlpTopologyEntry::Video(_)
                | YtDlpTopologyEntry::Delegation(_)
                | YtDlpTopologyEntry::Unavailable(_),
            ) => {}
        }
    }
}
