use super::*;

fn candidate(height: Option<u32>, audio_only: bool) -> WebMediaCandidatePresentation {
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
    WebMediaStreamConfiguration {
        generation,
        candidates: candidates.into(),
        active_candidate,
        preference: WebMediaSelectionPreference::GlobalBestPlayable,
    }
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
    let locator = service_direct_media::parse_direct_media_url(
        "https://user:password@example.test/video.mp4?token=secret",
    )
    .expect("valid direct-media fixture");
    let source = ActiveMediaSource::DirectMediaUrl(locator);
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
        pending_candidate: Some(PendingCandidateState {
            generation: WebMediaStreamGeneration {
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
        UrlSidebarSourceProjection::YtDlp {
            source_label: "example.test",
            configuration: &configuration,
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::SingleItem),
    );
    let UrlSidebarModel::YtDlp {
        pending_candidate,
        safe_error,
        ..
    } = model
    else {
        panic!("ожидалась YtDlp model");
    };
    assert_eq!(pending_candidate, None);
    assert_eq!(safe_error, None);
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
        pending_candidate: Some(PendingCandidateState {
            generation,
            candidate: pending.clone(),
        }),
        safe_error: Some(SafeErrorState {
            generation,
            error: UrlSidebarSafeError::SourceUnavailable,
        }),
        item_override: None,
    };
    let model = controller.model_from_source(
        UrlSidebarSourceProjection::YtDlp {
            source_label: "example.test",
            configuration: &configuration,
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::SingleItem),
    );
    assert!(matches!(
        model,
        UrlSidebarModel::YtDlp {
            pending_candidate: Some(candidate),
            safe_error: Some(UrlSidebarSafeError::SourceUnavailable),
            ..
        } if candidate == pending
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
        pending_candidate: None,
        safe_error: None,
        item_override: Some(ItemOverrideState {
            source_lineage: generation.source,
            item_id,
            preferred_height: Some(720),
        }),
    };
    let exact_model = controller.model_from_source(
        UrlSidebarSourceProjection::YtDlp {
            source_label: "example.test",
            configuration: &configuration,
        },
        &PlayerSnapshot::empty(),
        UrlSidebarItemBinding {
            scope: UrlSidebarItemScope::SingleItem,
            item_id: Some(item_id),
        },
    );
    assert!(matches!(
        exact_model,
        UrlSidebarModel::YtDlp {
            preference: WebMediaSelectionPreference::ItemOverride(Some(720)),
            ..
        }
    ));

    let detached_model = controller.model_from_source(
        UrlSidebarSourceProjection::YtDlp {
            source_label: "example.test",
            configuration: &configuration,
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::Detached),
    );
    assert!(matches!(
        detached_model,
        UrlSidebarModel::YtDlp {
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
        UrlSidebarSourceProjection::YtDlp {
            source_label: "example.test",
            configuration: &configuration,
        },
        &PlayerSnapshot::empty(),
        binding(UrlSidebarItemScope::CompoundPart),
    );
    assert!(matches!(
        model,
        UrlSidebarModel::YtDlp {
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
        UrlSidebarSourceProjection::YtDlp {
            source_label: locator.safe_label(),
            configuration: &configuration,
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
