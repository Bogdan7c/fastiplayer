use playlist_core::{
    DurableReopenLocator, PlaylistImportAvailability, PlaylistImportEntryDraft,
    PlaylistImportSourceKind, SecretUrlLocator, ServiceReopenMaterialKind,
};
use service_ytdlp::{
    YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION, YT_DLP_DURABLE_REOPEN_SERVICE_OWNER, YtDlpMediaLocator,
    parse_yt_dlp_media_locator,
};

use super::{
    TopologyDraftMappingBudgets, TopologyIdentityView, TopologyMappingNode, TopologyMetadataView,
    TopologyNodeDescription, YtDlpTopologyDraftIssueKind, map_topology_node,
};

/// Owned identity fixture, из которого generic mapper получает только borrowed view.
#[derive(Default)]
struct FakeIdentity {
    /// Stable extractor-local ID.
    extractor_id: Option<String>,
    /// Stable extractor namespace.
    extractor_key: Option<String>,
    /// Service-classified webpage locator.
    webpage_locator: Option<YtDlpMediaLocator>,
    /// Service-classified original locator.
    original_locator: Option<YtDlpMediaLocator>,
}

impl FakeIdentity {
    /// Создаёт минимальную extractor identity.
    fn extractor(extractor_key: &str, extractor_id: &str) -> Self {
        Self {
            extractor_id: Some(extractor_id.to_owned()),
            extractor_key: Some(extractor_key.to_owned()),
            webpage_locator: None,
            original_locator: None,
        }
    }

    /// Создаёт stable webpage identity из service parser-а.
    fn webpage(exact_locator: &str) -> Self {
        Self {
            webpage_locator: Some(locator(exact_locator)),
            ..Self::default()
        }
    }

    /// Возвращает production-shaped borrowed view.
    fn view(&self) -> TopologyIdentityView<'_> {
        TopologyIdentityView {
            extractor_id: self.extractor_id.as_deref(),
            extractor_key: self.extractor_key.as_deref(),
            webpage_locator: self.webpage_locator.as_ref(),
            original_locator: self.original_locator.as_ref(),
        }
    }
}

/// Owned metadata fixture.
#[derive(Default)]
struct FakeMetadata {
    /// Bounded title.
    title: Option<String>,
    /// Finite duration.
    duration: Option<std::time::Duration>,
}

impl FakeMetadata {
    /// Создаёт metadata с title и duration seconds.
    fn titled(title: &str, duration_seconds: u64) -> Self {
        Self {
            title: Some(title.to_owned()),
            duration: Some(std::time::Duration::from_secs(duration_seconds)),
        }
    }

    /// Возвращает production-shaped borrowed view.
    fn view(&self) -> TopologyMetadataView<'_> {
        TopologyMetadataView {
            title: self.title.as_deref(),
            duration: self.duration,
        }
    }
}

/// Recursive fake проверяет сам generic algorithm без process и public test constructors.
enum FakeTopologyNode {
    /// Playable video.
    Video {
        identity: FakeIdentity,
        metadata: FakeMetadata,
    },
    /// Flattened collection.
    Collection(Vec<FakeTopologyNode>),
    /// First-class compound.
    MultiVideo {
        identity: FakeIdentity,
        metadata: FakeMetadata,
        children: Vec<FakeTopologyNode>,
    },
    /// Leaf delegation.
    Delegation {
        target: YtDlpMediaLocator,
        metadata: FakeMetadata,
    },
    /// Retained unavailable child.
    Unavailable {
        identity: FakeIdentity,
        metadata: FakeMetadata,
    },
}

impl FakeTopologyNode {
    /// Создаёт обычный video fixture.
    fn video(extractor_id: &str, title: &str) -> Self {
        Self::Video {
            identity: FakeIdentity::extractor("fixture", extractor_id),
            metadata: FakeMetadata::titled(title, 10),
        }
    }

    /// Создаёт unavailable fixture с optional stable identity.
    fn unavailable(extractor_id: Option<&str>, title: &str) -> Self {
        Self::Unavailable {
            identity: extractor_id
                .map(|id| FakeIdentity::extractor("fixture", id))
                .unwrap_or_default(),
            metadata: FakeMetadata::titled(title, 0),
        }
    }
}

impl TopologyMappingNode for FakeTopologyNode {
    fn describe(&self) -> TopologyNodeDescription<'_> {
        match self {
            Self::Video { identity, metadata } => TopologyNodeDescription::Video {
                identity: identity.view(),
                metadata: metadata.view(),
            },
            Self::Collection(_) => TopologyNodeDescription::Collection,
            Self::MultiVideo {
                identity, metadata, ..
            } => TopologyNodeDescription::MultiVideo {
                identity: identity.view(),
                metadata: metadata.view(),
            },
            Self::Delegation { target, metadata } => TopologyNodeDescription::Delegation {
                target,
                metadata: metadata.view(),
            },
            Self::Unavailable { identity, metadata } => TopologyNodeDescription::Unavailable {
                identity: identity.view(),
                metadata: metadata.view(),
            },
        }
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&Self)) {
        match self {
            Self::Collection(children) | Self::MultiVideo { children, .. } => {
                for child in children {
                    visitor(child);
                }
            }
            Self::Video { .. } | Self::Delegation { .. } | Self::Unavailable { .. } => {}
        }
    }
}

/// Парсит locator только service parser-ом, как production registry.
fn locator(exact_locator: &str) -> YtDlpMediaLocator {
    parse_yt_dlp_media_locator(exact_locator).expect("fixture locator должен быть valid")
}

/// Создаёт exact root durable locator без normalization.
fn durable_root(exact_locator: &str) -> DurableReopenLocator {
    DurableReopenLocator::url(
        SecretUrlLocator::from_reopenable_url(exact_locator)
            .expect("fixture root locator должен быть non-empty"),
    )
}

/// Запускает mapper с production-like root и caller-defined focused budgets.
fn map_fake(
    root: &FakeTopologyNode,
    retained_items: usize,
    issues: usize,
) -> super::YtDlpTopologyDraftPreview {
    map_topology_node(
        root,
        durable_root("https://root.invalid/watch?token=exact#fragment"),
        TopologyDraftMappingBudgets {
            retained_items,
            issues,
        },
    )
}

/// Возвращает единственный compound либо завершает тест с понятной ошибкой.
fn only_compound(
    preview: &super::YtDlpTopologyDraftPreview,
) -> &playlist_core::PlaylistCompoundImportDraft {
    let mut entries = preview.entries();
    let entry = entries.next().expect("ожидается один compound");
    assert!(entries.next().is_none(), "лишние top-level entries");
    let PlaylistImportEntryDraft::Compound(compound) = entry else {
        panic!("ожидается first-class compound");
    };
    compound
}

#[test]
fn multi_video_preserves_group_summary_part_order_and_unavailable_state() {
    let root = FakeTopologyNode::MultiVideo {
        identity: FakeIdentity::extractor("fixture", "group"),
        metadata: FakeMetadata::titled("Concert", 120),
        children: vec![
            FakeTopologyNode::video("part-a", "Part A"),
            FakeTopologyNode::Delegation {
                target: locator("https://delegate.invalid/item?id=2"),
                metadata: FakeMetadata::titled("Part B", 20),
            },
            FakeTopologyNode::unavailable(Some("part-c"), "Part C"),
        ],
    };

    let preview = map_fake(&root, 10, 10);
    let compound = only_compound(&preview);
    let parts = compound.parts().collect::<Vec<_>>();

    assert_eq!(compound.cached_summary().title(), Some("Concert"));
    assert_eq!(
        compound.cached_summary().duration(),
        Some(media_core::MediaDuration::from_secs(120))
    );
    assert_eq!(parts.len(), 3);
    assert_eq!(
        compound.provenance().source_kind(),
        PlaylistImportSourceKind::Service
    );
    assert_eq!(
        compound
            .provenance()
            .root_locator()
            .expose_url_for_reopen()
            .expect("group provenance должна хранить exact root")
            .expose_secret_for_persistence(),
        "https://root.invalid/watch?token=exact#fragment"
    );
    assert_eq!(parts[0].cached_metadata().title(), Some("Part A"));
    assert_eq!(parts[1].cached_metadata().title(), Some("Part B"));
    assert_eq!(parts[2].cached_metadata().title(), Some("Part C"));
    assert_eq!(
        parts[0].availability(),
        PlaylistImportAvailability::Available
    );
    assert_eq!(
        parts[1].availability(),
        PlaylistImportAvailability::Available
    );
    assert_eq!(
        parts[2].availability(),
        PlaylistImportAvailability::Unavailable
    );
    assert_eq!(preview.retained_item_count(), 3);
    assert_eq!(preview.issues().len(), 0);
}

#[test]
fn one_retained_part_stays_compound_and_zero_parts_publish_only_issues() {
    let one_part = FakeTopologyNode::MultiVideo {
        identity: FakeIdentity::extractor("fixture", "one-group"),
        metadata: FakeMetadata::titled("One", 10),
        children: vec![FakeTopologyNode::video("only", "Only part")],
    };
    let one_preview = map_fake(&one_part, 10, 10);

    assert_eq!(only_compound(&one_preview).retained_part_count(), 1);

    let zero_parts = FakeTopologyNode::MultiVideo {
        identity: FakeIdentity::extractor("fixture", "empty-group"),
        metadata: FakeMetadata::titled("Empty", 0),
        children: vec![FakeTopologyNode::unavailable(None, "Missing")],
    };
    let zero_preview = map_fake(&zero_parts, 10, 10);
    let issue_kinds = zero_preview
        .issues()
        .map(super::YtDlpTopologyDraftIssue::kind)
        .collect::<Vec<_>>();

    assert_eq!(zero_preview.entries().len(), 0);
    assert_eq!(zero_preview.retained_item_count(), 0);
    assert_eq!(
        issue_kinds,
        vec![
            YtDlpTopologyDraftIssueKind::MissingStableIdentity,
            YtDlpTopologyDraftIssueKind::CompoundWithoutRetainedParts,
        ]
    );
}

#[test]
fn duplicate_extractor_ids_remain_distinct_ordered_singles() {
    let root = FakeTopologyNode::Collection(vec![
        FakeTopologyNode::video("duplicate", "First"),
        FakeTopologyNode::video("duplicate", "Second"),
    ]);

    let preview = map_fake(&root, 10, 10);
    let singles = preview
        .entries()
        .map(|entry| match entry {
            PlaylistImportEntryDraft::Single(single) => single,
            PlaylistImportEntryDraft::Compound(_) => panic!("неожиданный compound"),
        })
        .collect::<Vec<_>>();
    let first_payload = singles[0]
        .reopen_locator()
        .expose_service_payload_for_reopen()
        .expect("extractor identity должна быть service-owned");
    let second_payload = singles[1]
        .reopen_locator()
        .expose_service_payload_for_reopen()
        .expect("extractor identity должна быть service-owned");

    assert_eq!(singles.len(), 2);
    assert_eq!(singles[0].cached_metadata().title(), Some("First"));
    assert_eq!(singles[1].cached_metadata().title(), Some("Second"));
    assert_eq!(
        first_payload.expose_payload_for_reopen(),
        second_payload.expose_payload_for_reopen()
    );
}

#[test]
fn nested_collections_flatten_but_delegation_remains_one_leaf() {
    let root = FakeTopologyNode::Collection(vec![
        FakeTopologyNode::video("first", "First"),
        FakeTopologyNode::Collection(vec![FakeTopologyNode::Collection(vec![
            FakeTopologyNode::video("nested", "Nested"),
        ])]),
        FakeTopologyNode::Delegation {
            target: locator("https://delegate.invalid/leaf?opaque=1"),
            metadata: FakeMetadata::titled("Delegated", 30),
        },
    ]);

    let preview = map_fake(&root, 10, 10);
    let titles = preview
        .entries()
        .map(|entry| match entry {
            PlaylistImportEntryDraft::Single(single) => {
                single.cached_metadata().title().unwrap_or_default()
            }
            PlaylistImportEntryDraft::Compound(_) => panic!("неожиданный compound"),
        })
        .collect::<Vec<_>>();

    assert_eq!(titles, vec!["First", "Nested", "Delegated"]);
    assert_eq!(preview.retained_item_count(), 3);
}

#[test]
fn root_uses_exact_locator_while_extracted_child_uses_versioned_service_payload() {
    let exact_root = "https://root.invalid/watch?token=exact#fragment";
    let root_video = FakeTopologyNode::Video {
        identity: FakeIdentity::default(),
        metadata: FakeMetadata::titled("Root", 1),
    };
    let root_preview = map_topology_node(
        &root_video,
        durable_root(exact_root),
        TopologyDraftMappingBudgets {
            retained_items: 10,
            issues: 10,
        },
    );
    let PlaylistImportEntryDraft::Single(root_single) =
        root_preview.entries().next().expect("root single")
    else {
        panic!("root video должен стать Single");
    };

    assert_eq!(
        root_single
            .reopen_locator()
            .expose_url_for_reopen()
            .expect("root должен остаться exact URL")
            .expose_secret_for_persistence(),
        exact_root
    );

    let child_exact = "https://child.invalid/watch?v=42&token=secret";
    let collection = FakeTopologyNode::Collection(vec![FakeTopologyNode::Video {
        identity: FakeIdentity::webpage(child_exact),
        metadata: FakeMetadata::titled("Child", 2),
    }]);
    let child_preview = map_fake(&collection, 10, 10);
    let PlaylistImportEntryDraft::Single(child_single) =
        child_preview.entries().next().expect("child single")
    else {
        panic!("child video должен стать Single");
    };
    let service_payload = child_single
        .reopen_locator()
        .expose_service_payload_for_reopen()
        .expect("extracted child должен сохранить service ownership");

    assert_eq!(
        service_payload.service_owner(),
        YT_DLP_DURABLE_REOPEN_SERVICE_OWNER
    );
    assert_eq!(
        service_payload
            .payload_version()
            .expose_value_for_persistence(),
        YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION
    );
    assert_eq!(
        service_payload.material_kind(),
        ServiceReopenMaterialKind::StableWebpageIdentity
    );
    assert_eq!(
        service_payload.expose_payload_for_reopen(),
        child_exact.as_bytes()
    );
}

#[test]
fn ephemeral_transport_material_is_rejected_before_it_can_enter_a_draft() {
    let ephemeral_result = DurableReopenLocator::from_service_payload(
        YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
        YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION,
        ServiceReopenMaterialKind::SignedEndpoint,
        b"https://cdn.invalid/signed?token=secret".to_vec(),
    );

    assert!(ephemeral_result.is_err());
}

#[test]
fn mapping_budgets_are_bounded_and_do_not_publish_partial_compounds() {
    let oversized_group = FakeTopologyNode::MultiVideo {
        identity: FakeIdentity::extractor("fixture", "bounded-group"),
        metadata: FakeMetadata::titled("Bounded", 10),
        children: vec![
            FakeTopologyNode::video("one", "One"),
            FakeTopologyNode::video("two", "Two"),
            FakeTopologyNode::video("three", "Three"),
        ],
    };
    let group_preview = map_fake(&oversized_group, 2, 10);

    assert_eq!(group_preview.entries().len(), 0);
    assert_eq!(
        group_preview.issues().next().map(|issue| issue.kind()),
        Some(YtDlpTopologyDraftIssueKind::CompoundPartLimitExceeded)
    );

    let issue_heavy_collection = FakeTopologyNode::Collection(vec![
        FakeTopologyNode::unavailable(None, "One"),
        FakeTopologyNode::unavailable(None, "Two"),
        FakeTopologyNode::unavailable(None, "Three"),
        FakeTopologyNode::unavailable(None, "Four"),
    ]);
    let issue_preview = map_fake(&issue_heavy_collection, 10, 2);
    let issue_paths = issue_preview
        .issues()
        .map(|issue| issue.path().to_vec())
        .collect::<Vec<_>>();

    assert_eq!(issue_preview.issues().len(), 2);
    assert_eq!(issue_preview.omitted_issue_count(), 2);
    assert_eq!(issue_paths, vec![vec![1], vec![2]]);
}

#[test]
fn retained_unavailable_child_obeys_aggregate_item_bound() {
    let root = FakeTopologyNode::Collection(vec![
        FakeTopologyNode::unavailable(Some("kept"), "Kept"),
        FakeTopologyNode::unavailable(Some("bounded-out"), "Bounded out"),
    ]);

    let preview = map_fake(&root, 1, 10);
    let PlaylistImportEntryDraft::Single(single) = preview
        .entries()
        .next()
        .expect("first unavailable retained")
    else {
        panic!("unavailable stable child должен остаться Single");
    };

    assert_eq!(
        single.availability(),
        PlaylistImportAvailability::Unavailable
    );
    assert_eq!(preview.retained_item_count(), 1);
    assert_eq!(
        preview.issues().next().map(|issue| issue.kind()),
        Some(YtDlpTopologyDraftIssueKind::RetainedItemLimitExceeded)
    );
}

#[test]
fn production_mapping_source_has_no_second_url_parser_queue_authority_or_ephemeral_material() {
    let facade_source = include_str!("../url_topology_drafts.rs");
    let mapper_source = include_str!("mapper.rs");
    let adapter_source = include_str!("service_adapter.rs");
    let production_source = [facade_source, mapper_source, adapter_source].join("\n");

    for forbidden_fragment in [
        "url::",
        "parse_yt_dlp",
        "use playlist_core::PlaylistQueue",
        "playlist_core::PlaylistQueue",
        "PlaylistItemId",
        "FormatUrl",
        "ManifestUrl",
        "FragmentUrl",
        "KeyUrl",
        "SignedEndpoint",
        "Headers",
        "Cookies",
        "AuthorizationOrSession",
    ] {
        assert!(
            !production_source.contains(forbidden_fragment),
            "production mapper не должен содержать `{forbidden_fragment}`"
        );
    }
}
