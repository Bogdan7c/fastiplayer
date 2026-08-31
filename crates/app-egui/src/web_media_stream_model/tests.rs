use super::*;

pub(super) fn candidate(height: Option<u32>, audio_only: bool) -> WebMediaCandidatePresentation {
    WebMediaCandidatePresentation {
        layout: if audio_only {
            StreamLayoutKind::AudioOnly
        } else {
            StreamLayoutKind::Muxed
        },
        width: height.map(|value| value * 16 / 9),
        height,
        frame_rate: height.map(|_| (30, 1)),
        video_bitrate: height.map(|_| 4_000_000),
        audio_bitrate: Some(128_000),
        video_codec: (!audio_only).then_some(CodecFamily::Vp9),
        audio_codec: Some(CodecFamily::Opus),
        dynamic_range: (!audio_only).then_some(web_media_core::DynamicRange::Sdr),
        containers: WebMediaContainerSummary {
            video: (!audio_only).then_some(ContainerFamily::WebM),
            audio: Some(ContainerFamily::WebM),
        },
    }
}

fn configuration(
    generation: WebMediaStreamGeneration,
    candidates: Vec<WebMediaCandidatePresentation>,
    active_candidate: WebMediaCandidatePresentation,
) -> WebMediaStreamConfiguration {
    let active_parent = exact_parent(generation);
    let candidate_selections =
        vec![WebMediaSelection::candidate(active_parent.clone()); candidates.len()];
    WebMediaStreamConfiguration {
        generation,
        active_parent,
        candidates: candidates.into(),
        candidate_selections: candidate_selections.into(),
        active_candidate,
        preference: WebMediaSelectionPreference::GlobalBestPlayable,
        component_variants: WebMediaComponentVariantConfiguration::Unavailable,
        hls_subtitle_renditions: Arc::from([]),
    }
}

fn exact_parent(generation: WebMediaStreamGeneration) -> ExactSelectionIdentity {
    let source = web_media_core::SourceIdentity::new(generation.source);
    let exact = web_media_core::CandidateIdentity::new(
        source,
        web_media_core::ExtractionGeneration::new(generation.extraction),
        web_media_core::CandidateFormatIdentity::new("active-parent")
            .expect("fixture exact identity валидна"),
    );
    let semantic = web_media_core::SemanticIdentity::new(source, "semantic-parent")
        .expect("fixture semantic identity валидна");
    ExactSelectionIdentity::new(exact, semantic).expect("fixture source lineage совпадает")
}

#[test]
fn installed_hls_subtitles_survive_configuration_clone_without_locator() {
    let generation = WebMediaStreamGeneration {
        source: 1,
        extraction: 1,
    };
    let active_candidate = candidate(Some(720), false);
    let rendition = crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition::fixture(
        "subs",
        "English",
        Some("en"),
        Some("public.accessibility.transcribes-spoken-dialog"),
        false,
    );
    let configured = configuration(generation, vec![active_candidate.clone()], active_candidate)
        .with_hls_subtitle_renditions(Arc::from([rendition]));
    let rebuilt = configured.clone();
    let [retained] = rebuilt.hls_subtitle_renditions() else {
        panic!("exact installed rendition должен сохраниться");
    };
    assert_eq!(retained.group_id(), "subs");
    assert_eq!(retained.name(), "English");
    assert_eq!(retained.language(), Some("en"));
    assert!(!format!("{retained:?}").contains("://"));
}

fn binding(scope: UrlSidebarItemScope) -> UrlSidebarItemBinding {
    UrlSidebarItemBinding {
        scope,
        item_id: None,
    }
}

#[test]
fn inactive_local_source_has_no_web_configuration() {
    let source = ActiveMediaSource::LocalFile("/private/movie.mkv".into());
    let model =
        UrlSidebarController::default().model(Some(&source), &PlayerSnapshot::empty(), None);
    assert_eq!(model, UrlSidebarModel::Inactive);
}

#[test]
fn direct_media_state_does_not_invent_format_choices() {
    let locator = crate::direct_progressive_open::classify_direct_media_url(
        "https://user:password@example.test/video.mp4?token=secret",
    )
    .expect("valid direct-media fixture");
    let source = ActiveMediaSource::Web(crate::media_open::WebMediaSourceIntent::direct(locator));
    let model =
        UrlSidebarController::default().model(Some(&source), &PlayerSnapshot::empty(), None);
    let UrlSidebarModel::DirectMedia { source_label, .. } = model else {
        panic!("direct source должен иметь отдельную модель без candidate inventory");
    };
    assert!(source_label.contains("example.test"));
    assert!(!source_label.contains("password"));
    assert!(!source_label.contains("secret"));
}

#[test]
fn audio_only_candidate_has_no_fake_resolution() {
    let audio = candidate(None, true);
    assert!(!audio.has_video());
    assert_eq!(audio.height, None);
    assert_eq!(audio.layout, StreamLayoutKind::AudioOnly);
}

#[test]
fn one_and_many_candidate_inventory_preserve_active_projection() {
    let active = candidate(Some(1080), false);
    let one: Arc<[WebMediaCandidatePresentation]> = Arc::from([active.clone()]);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0], active);

    let many: Arc<[WebMediaCandidatePresentation]> =
        Arc::from([candidate(Some(720), false), active.clone()]);
    assert_eq!(many.len(), 2);
    assert!(many.contains(&active));
}

#[test]
fn stale_generation_hides_pending_candidate_and_safe_error() {
    let controller = UrlSidebarController {
        pending_selection: Some(UrlSidebarPendingSelection::Candidate {
            parent_generation: WebMediaStreamGeneration {
                source: 4,
                extraction: 8,
            },
            candidate: candidate(Some(720), false),
        }),
        safe_error: Some(SafeErrorState {
            generation: WebMediaStreamGeneration {
                source: 4,
                extraction: 8,
            },
            error: UrlSidebarSafeError::SourceUnavailable,
        }),
        item_override: None,
    };
    let active_generation = WebMediaStreamGeneration {
        source: 4,
        extraction: 9,
    };
    let active = candidate(Some(1080), false);
    let configuration = configuration(active_generation, vec![active.clone()], active);
    let model = controller.model_from_source(
        UrlSidebarSourceProjection::WebMedia {
            ingress: web_media_core::WebMediaIngressKind::ExtractorBacked,
            source_label: "example.test",
            configuration: Some(&configuration),
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::SingleItem),
    );
    let UrlSidebarModel::CatalogBacked {
        pending_selection,
        safe_error,
        ..
    } = model
    else {
        panic!("ожидалась YtDlp model");
    };
    assert_eq!(pending_selection, None);
    assert_eq!(safe_error, None);
}

#[test]
fn stale_generation_cannot_resolve_neutral_switch_selection() {
    let current_generation = WebMediaStreamGeneration::for_test(31, 7);
    let stale_generation = WebMediaStreamGeneration::for_test(31, 6);
    let active_candidate = candidate(Some(720), false);
    let configuration = configuration(
        current_generation,
        vec![active_candidate.clone()],
        active_candidate,
    );

    assert!(
        configuration
            .selection_for_switch(stale_generation, 0)
            .is_none(),
        "stale generation не должна получить exact neutral selection"
    );
    assert!(
        configuration
            .selection_for_switch(current_generation, 0)
            .is_some(),
        "matching generation должна получить bounded selection"
    );
}

#[test]
fn current_generation_exposes_pending_candidate_and_bounded_failure() {
    let generation = WebMediaStreamGeneration {
        source: 8,
        extraction: 2,
    };
    let active = candidate(Some(1080), false);
    let pending = candidate(Some(720), false);
    let configuration = configuration(generation, vec![pending.clone(), active.clone()], active);
    let controller = UrlSidebarController {
        pending_selection: Some(UrlSidebarPendingSelection::Candidate {
            parent_generation: generation,
            candidate: pending.clone(),
        }),
        safe_error: Some(SafeErrorState {
            generation,
            error: UrlSidebarSafeError::SourceUnavailable,
        }),
        item_override: None,
    };
    let model = controller.model_from_source(
        UrlSidebarSourceProjection::WebMedia {
            ingress: web_media_core::WebMediaIngressKind::ExtractorBacked,
            source_label: "example.test",
            configuration: Some(&configuration),
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::SingleItem),
    );
    let UrlSidebarModel::CatalogBacked {
        pending_selection,
        safe_error,
        ..
    } = model
    else {
        panic!("ожидалась YtDlp model");
    };
    assert!(matches!(
        pending_selection.as_deref(),
        Some(UrlSidebarPendingSelection::Candidate { candidate, .. }) if candidate == &pending
    ));
    assert_eq!(safe_error, Some(UrlSidebarSafeError::SourceUnavailable));
}

#[test]
fn candidate_switch_selector_is_single_flight_and_pre_barrier_failure_restores_it() {
    let generation = WebMediaStreamGeneration {
        source: 21,
        extraction: 4,
    };
    let pending = candidate(Some(720), false);
    let pending_selection = UrlSidebarPendingSelection::Candidate {
        parent_generation: generation,
        candidate: pending,
    };
    let mut controller = UrlSidebarController::default();

    assert_eq!(
        controller.record_switch_started(pending_selection.clone()),
        Ok(())
    );
    assert_eq!(
        controller.record_switch_started(pending_selection.clone()),
        Err(UrlSidebarTransitionError::Busy)
    );
    assert!(
        !controller
            .record_switch_start_rejected(generation, UrlSidebarSafeError::SameItemSwitchBusy,)
    );
    assert_eq!(
        controller.pending_selection.as_ref(),
        Some(&pending_selection)
    );
    assert!(controller.record_switch_failed(
        &pending_selection,
        generation,
        UrlSidebarSafeError::SameItemSwitchCancelled,
    ));
    assert!(controller.pending_selection.is_none());
    assert_eq!(
        controller.safe_error.as_ref().map(|error| error.error),
        Some(UrlSidebarSafeError::SameItemSwitchCancelled)
    );
}

#[test]
fn detached_installed_switch_publishes_runtime_override_for_fresh_generation() {
    let previous_generation = WebMediaStreamGeneration {
        source: 22,
        extraction: 7,
    };
    let installed_generation = WebMediaStreamGeneration {
        source: 22,
        extraction: 8,
    };
    let active = candidate(Some(1440), false);
    let configuration = configuration(installed_generation, vec![active.clone()], active);
    let mut controller = UrlSidebarController::default();
    let pending_selection = UrlSidebarPendingSelection::Candidate {
        parent_generation: previous_generation,
        candidate: candidate(Some(1440), false),
    };
    controller
        .record_switch_started(pending_selection)
        .expect("selector должен стать pending");

    controller.record_candidate_switch_installed(installed_generation, None, Some(1440));
    let model = controller.model_from_source(
        UrlSidebarSourceProjection::WebMedia {
            ingress: web_media_core::WebMediaIngressKind::ExtractorBacked,
            source_label: "example.test",
            configuration: Some(&configuration),
        },
        &PlayerSnapshot::empty(),
        UrlSidebarItemBinding {
            scope: UrlSidebarItemScope::Detached,
            item_id: None,
        },
    );

    assert!(matches!(
        model,
        UrlSidebarModel::CatalogBacked {
            preference: WebMediaSelectionPreference::ItemOverride(Some(1440)),
            pending_selection: None,
            ..
        }
    ));
}

#[test]
fn component_completion_keeps_existing_item_override_unchanged() {
    let installed_generation = WebMediaStreamGeneration {
        source: 23,
        extraction: 9,
    };
    let active = candidate(Some(1440), false);
    let configuration = configuration(installed_generation, vec![active.clone()], active);
    let mut controller = UrlSidebarController {
        pending_selection: None,
        safe_error: None,
        item_override: Some(ItemOverrideState {
            source_lineage: installed_generation.source,
            item_id: None,
            preferred_height: Some(1440),
        }),
    };
    controller.record_component_switch_installed();

    let model = controller.model_from_source(
        UrlSidebarSourceProjection::WebMedia {
            ingress: web_media_core::WebMediaIngressKind::ExtractorBacked,
            source_label: "example.test",
            configuration: Some(&configuration),
        },
        &PlayerSnapshot::empty(),
        UrlSidebarItemBinding {
            scope: UrlSidebarItemScope::Detached,
            item_id: None,
        },
    );

    assert!(matches!(
        model,
        UrlSidebarModel::CatalogBacked {
            preference: WebMediaSelectionPreference::ItemOverride(Some(1440)),
            pending_selection: None,
            ..
        }
    ));
}

#[test]
fn item_override_requires_exact_item_and_source_lineage() {
    let item_id = playlist_core::PlaylistItemId::from_persistence_value(17)
        .expect("non-zero fixture Item ID");
    let generation = WebMediaStreamGeneration {
        source: 21,
        extraction: 4,
    };
    let active = candidate(Some(1080), false);
    let configuration = configuration(generation, vec![active.clone()], active);
    let controller = UrlSidebarController {
        pending_selection: None,
        safe_error: None,
        item_override: Some(ItemOverrideState {
            source_lineage: generation.source,
            item_id: Some(item_id),
            preferred_height: Some(720),
        }),
    };
    let exact_model = controller.model_from_source(
        UrlSidebarSourceProjection::WebMedia {
            ingress: web_media_core::WebMediaIngressKind::ExtractorBacked,
            source_label: "example.test",
            configuration: Some(&configuration),
        },
        &PlayerSnapshot::empty(),
        UrlSidebarItemBinding {
            scope: UrlSidebarItemScope::SingleItem,
            item_id: Some(item_id),
        },
    );
    assert!(matches!(
        exact_model,
        UrlSidebarModel::CatalogBacked {
            preference: WebMediaSelectionPreference::ItemOverride(Some(720)),
            ..
        }
    ));

    let detached_model = controller.model_from_source(
        UrlSidebarSourceProjection::WebMedia {
            ingress: web_media_core::WebMediaIngressKind::ExtractorBacked,
            source_label: "example.test",
            configuration: Some(&configuration),
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::Detached),
    );
    assert!(matches!(
        detached_model,
        UrlSidebarModel::CatalogBacked {
            preference: WebMediaSelectionPreference::GlobalBestPlayable,
            ..
        }
    ));
}

#[test]
fn preference_distinguishes_global_default_and_item_override() {
    assert_ne!(
        WebMediaSelectionPreference::GlobalBestPlayable,
        WebMediaSelectionPreference::ItemOverride(None)
    );
    assert_ne!(
        WebMediaSelectionPreference::GlobalPreferredHeight(2160),
        WebMediaSelectionPreference::ItemOverride(Some(2160))
    );
}

#[test]
fn safe_error_model_contains_no_arbitrary_error_text() {
    let debug = format!("{:?}", UrlSidebarSafeError::SourceUnavailable);
    assert_eq!(debug, "SourceUnavailable");
    assert!(!debug.contains("https://"));
    assert!(!debug.contains("Cookie"));
}

#[test]
fn group_part_scope_is_first_class_and_not_a_fake_single_item() {
    let generation = WebMediaStreamGeneration {
        source: 11,
        extraction: 3,
    };
    let active = candidate(Some(720), false);
    let configuration = configuration(generation, vec![active.clone()], active);
    let model = UrlSidebarController::default().model_from_source(
        UrlSidebarSourceProjection::WebMedia {
            ingress: web_media_core::WebMediaIngressKind::ExtractorBacked,
            source_label: "example.test",
            configuration: Some(&configuration),
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::CompoundPart),
    );
    assert!(matches!(
        model,
        UrlSidebarModel::CatalogBacked {
            item_scope: UrlSidebarItemScope::CompoundPart,
            ..
        }
    ));
}

#[test]
fn secret_safe_model_never_contains_locator_path_query_or_userinfo() {
    let locator = service_ytdlp::parse_yt_dlp_media_locator(
        "https://user:password@example.test/private/watch?token=secret#fragment",
    )
    .expect("valid YtDlp fixture");
    let generation = WebMediaStreamGeneration {
        source: 5,
        extraction: 1,
    };
    let active = candidate(Some(1080), false);
    let configuration = configuration(generation, vec![active.clone()], active);
    let model = UrlSidebarController::default().model_from_source(
        UrlSidebarSourceProjection::WebMedia {
            ingress: web_media_core::WebMediaIngressKind::ExtractorBacked,
            source_label: locator.safe_label(),
            configuration: Some(&configuration),
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::Detached),
    );
    let debug = format!("{model:?}");
    assert!(debug.contains("example.test"));
    assert!(!debug.contains("password"));
    assert!(!debug.contains("private"));
    assert!(!debug.contains("token"));
    assert!(!debug.contains("fragment"));
}
