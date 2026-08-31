use std::num::NonZeroU64;

use media_core::MediaTime;
use player_core::{MediaInstallRequestId, PlaybackState, PlayerSnapshot};
use rustiplayer_config::WebMediaConfig;
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_transport_api::{
    EndpointExpiryObserver, EndpointExpiryReason, EndpointExpiryResourceKind,
    MediaComponentIdentity, MediaComponentRole,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::media_open::MediaOpenRequestId;

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity должна быть non-zero")
}

fn expiry_signal(generation: u64) -> EndpointExpirySignal {
    EndpointExpirySignal::new(
        MediaComponentIdentity::new(
            CandidateIdentity::new(
                SourceIdentity::new(71),
                ExtractionGeneration::new(generation),
                CandidateFormatIdentity::new("recovery-policy-fixture")
                    .expect("format identity должна быть valid"),
            ),
            SemanticIdentity::new(SourceIdentity::new(71), "recovery-policy-semantic")
                .expect("semantic identity должна быть valid"),
            MediaComponentRole::Muxed,
        )
        .expect("component и semantic identity должны принадлежать одной source lineage"),
        SourceGeneration::new(generation),
        EndpointExpiryResourceKind::MediaSegment,
        EndpointExpiryReason::AuthorizationExpired,
    )
}

fn media_instance_id(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(non_zero(value))
}

fn playlist_runtime_with_installed_media(value: u64) -> (PlaylistRuntime, ActiveMediaIdentity) {
    let mut playlist_runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    playlist_runtime.resolve_missing_state_for_test();
    let binding = playlist_runtime
        .bind_resumed_app_state()
        .expect("test runtime принимает resumed AppState binding");
    let direct_source = ActiveMediaSource::DirectMediaUrl(
        service_direct_media::parse_direct_media_url(&format!(
            "https://media.example.test/recovery-identity-{value}.mp4"
        ))
        .expect("direct identity fixture должна быть valid"),
    );
    let active_media = playlist_runtime
        .register_successful_strong_install(
            MediaOpenRequestId::from_non_zero(non_zero(value)),
            MediaInstallRequestId::from_non_zero(non_zero(value)),
            media_instance_id(value),
            binding,
            direct_source,
            PlaybackIntent::StartPlaying,
        )
        .expect("production playlist owner должен зарегистрировать strong install");
    (playlist_runtime, active_media)
}

fn player_snapshot_for(media_instance_id: MediaInstanceId) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.media_instance_id = Some(media_instance_id);
    snapshot.playback_state = PlaybackState::Playing;
    snapshot.current_position = Duration::from_secs(17);
    snapshot.timeline.current_position = MediaTime::from_secs(17);
    snapshot.timeline.target_position = Some(MediaTime::from_secs(42));
    snapshot.source_label = Some("neighbor-player-state".to_owned());
    snapshot.volume = 0.37;
    snapshot.muted = true;
    snapshot
}

fn armed_admission(
    media_instance_id: MediaInstanceId,
    now: Instant,
    installed_age: Duration,
    consecutive_attempts: u64,
    generation: u64,
) -> InstalledVodEndpointRecoveryClaimAdmission {
    let attachment = VodEndpointRecoveryAttachment::new();
    attachment.arm_after_candidate_finalization();
    attachment.observe_endpoint_expiry(expiry_signal(generation));
    InstalledVodEndpointRecoveryClaimAdmission::new(
        media_instance_id,
        attachment,
        consecutive_attempts,
        now - installed_age,
    )
}

fn requested_config() -> WebMediaConfig {
    WebMediaConfig {
        vod_endpoint_recovery_enabled: true,
        vod_endpoint_recovery_max_consecutive_attempts: 5,
        vod_endpoint_recovery_initial_backoff_ms: 400,
        vod_endpoint_recovery_max_backoff_ms: 700,
        vod_endpoint_recovery_stable_reset_ms: 5_000,
        ..WebMediaConfig::default()
    }
}

fn admitted_plan(outcome: VodEndpointExpiryAdmissionOutcome) -> VodEndpointRecoveryClaimPlan {
    let VodEndpointExpiryAdmissionOutcome::Admitted(plan) = outcome else {
        panic!("matching runtime facts должны создать admitted recovery plan");
    };
    plan
}

fn playlist_observables(
    playlist_runtime: &PlaylistRuntime,
) -> (u64, u64, usize, Option<ActiveMediaIdentity>, bool) {
    let snapshot = playlist_runtime.playlist_view_snapshot();
    (
        snapshot.revision().get(),
        snapshot.structural_revision().get(),
        snapshot.len(),
        snapshot.active_media(),
        snapshot.has_active_tombstone(),
    )
}

#[test]
fn next_admission_samples_all_five_policy_values_from_real_runtime_facts() {
    let now = Instant::now();
    let (playlist_runtime, active_media) = playlist_runtime_with_installed_media(1_091);
    let player_snapshot = player_snapshot_for(active_media.media_instance_id());
    let player_snapshot_before = player_snapshot.clone();
    let playlist_before = playlist_observables(&playlist_runtime);
    let admission = armed_admission(
        active_media.media_instance_id(),
        now,
        Duration::from_secs(1),
        2,
        91,
    );

    let plan = admitted_plan(admission.admit_claim_from_runtime_facts(
        &requested_config(),
        &player_snapshot,
        playlist_runtime.playlist_view_snapshot().active_media(),
        now,
    ));

    assert_eq!(
        plan.policy,
        VodEndpointRecoveryPolicy {
            enabled: true,
            max_consecutive_attempts: 5,
            initial_backoff: Duration::from_millis(400),
            max_backoff: Duration::from_millis(700),
            stable_reset: Duration::from_millis(5_000),
        }
    );
    assert_eq!(plan.expected_active, active_media);
    assert_eq!(plan.next_consecutive_attempts, 3);
    assert_eq!(plan.not_before, now + Duration::from_millis(700));
    assert_eq!(plan.source_generation, SourceGeneration::new(91));
    assert_eq!(plan.restore_position, Duration::from_secs(42));
    assert_eq!(plan.playback_intent, PlaybackIntent::StartPlaying);
    assert_eq!(player_snapshot, player_snapshot_before);
    assert_eq!(playlist_observables(&playlist_runtime), playlist_before);
    assert_eq!(
        admission.media_instance_id,
        active_media.media_instance_id()
    );
    assert_eq!(admission.consecutive_attempts, 2);
}

#[test]
fn player_and_playlist_identity_mismatches_are_independently_rejected() {
    let now = Instant::now();
    let (matching_playlist, matching_active) = playlist_runtime_with_installed_media(1_092);
    let mut mismatching_player = player_snapshot_for(matching_active.media_instance_id());
    mismatching_player.media_instance_id = Some(media_instance_id(9_092));
    let player_mismatch = armed_admission(
        matching_active.media_instance_id(),
        now,
        Duration::from_secs(1),
        0,
        92,
    );

    assert!(matches!(
        player_mismatch.admit_claim_from_runtime_facts(
            &requested_config(),
            &mismatching_player,
            matching_playlist.playlist_view_snapshot().active_media(),
            now,
        ),
        VodEndpointExpiryAdmissionOutcome::Rejected
    ));
    assert!(!player_mismatch.has_pending_expiry_signal());

    let installed_media = media_instance_id(1_093);
    let matching_player = player_snapshot_for(installed_media);
    let (mismatching_playlist, mismatching_active) = playlist_runtime_with_installed_media(9_093);
    let playlist_before = playlist_observables(&mismatching_playlist);
    let playlist_mismatch = armed_admission(installed_media, now, Duration::from_secs(1), 0, 93);

    assert_ne!(mismatching_active.media_instance_id(), installed_media);
    assert!(matches!(
        playlist_mismatch.admit_claim_from_runtime_facts(
            &requested_config(),
            &matching_player,
            mismatching_playlist.playlist_view_snapshot().active_media(),
            now,
        ),
        VodEndpointExpiryAdmissionOutcome::Rejected
    ));
    assert!(!playlist_mismatch.has_pending_expiry_signal());
    assert_eq!(playlist_observables(&mismatching_playlist), playlist_before);
}

#[test]
fn disabled_policy_rejects_expiry_without_an_admitted_plan() {
    let now = Instant::now();
    let (playlist_runtime, active_media) = playlist_runtime_with_installed_media(1_094);
    let player_snapshot = player_snapshot_for(active_media.media_instance_id());
    let admission = armed_admission(
        active_media.media_instance_id(),
        now,
        Duration::from_secs(1),
        0,
        94,
    );
    let mut disabled_config = requested_config();
    disabled_config.vod_endpoint_recovery_enabled = false;

    assert!(matches!(
        admission.admit_claim_from_runtime_facts(
            &disabled_config,
            &player_snapshot,
            playlist_runtime.playlist_view_snapshot().active_media(),
            now,
        ),
        VodEndpointExpiryAdmissionOutcome::Rejected
    ));
    assert!(!admission.has_pending_expiry_signal());
}

#[test]
fn attempt_budget_cap_backoff_and_stable_reset_are_decided_at_admission() {
    let now = Instant::now();
    let (playlist_runtime, active_media) = playlist_runtime_with_installed_media(1_095);
    let player_snapshot = player_snapshot_for(active_media.media_instance_id());
    let expected_active = playlist_runtime.playlist_view_snapshot().active_media();
    let config = requested_config();
    let capped_backoff = armed_admission(
        active_media.media_instance_id(),
        now,
        Duration::from_secs(1),
        2,
        95,
    );
    let capped_plan = admitted_plan(capped_backoff.admit_claim_from_runtime_facts(
        &config,
        &player_snapshot,
        expected_active,
        now,
    ));
    assert_eq!(capped_plan.next_consecutive_attempts, 3);
    assert_eq!(capped_plan.not_before, now + Duration::from_millis(700));

    let exhausted = armed_admission(
        active_media.media_instance_id(),
        now,
        Duration::from_secs(1),
        5,
        96,
    );
    assert!(matches!(
        exhausted.admit_claim_from_runtime_facts(&config, &player_snapshot, expected_active, now,),
        VodEndpointExpiryAdmissionOutcome::Rejected
    ));

    let stable = armed_admission(
        active_media.media_instance_id(),
        now,
        Duration::from_secs(6),
        5,
        97,
    );
    let stable_plan = admitted_plan(stable.admit_claim_from_runtime_facts(
        &config,
        &player_snapshot,
        expected_active,
        now,
    ));
    assert_eq!(stable_plan.next_consecutive_attempts, 1);
    assert_eq!(stable_plan.not_before, now + Duration::from_millis(400));
}

#[test]
fn later_config_change_cannot_mutate_or_cancel_the_admitted_policy_snapshot() {
    let now = Instant::now();
    let (playlist_runtime, active_media) = playlist_runtime_with_installed_media(1_098);
    let player_snapshot = player_snapshot_for(active_media.media_instance_id());
    let expected_active = playlist_runtime.playlist_view_snapshot().active_media();
    let admission = armed_admission(
        active_media.media_instance_id(),
        now,
        Duration::from_secs(1),
        0,
        98,
    );
    let plan = admitted_plan(admission.admit_claim_from_runtime_facts(
        &requested_config(),
        &player_snapshot,
        expected_active,
        now,
    ));
    let immutable_policy = plan.policy;
    let immutable_not_before = plan.not_before;
    let immutable_attempts = plan.next_consecutive_attempts;
    let mut later_config = requested_config();
    later_config.vod_endpoint_recovery_enabled = false;
    later_config.vod_endpoint_recovery_max_consecutive_attempts = 1;
    later_config.vod_endpoint_recovery_initial_backoff_ms = 1;
    later_config.vod_endpoint_recovery_max_backoff_ms = 1;
    later_config.vod_endpoint_recovery_stable_reset_ms = 1;

    let later_policy = VodEndpointRecoveryPolicy::from_config(&later_config);
    assert!(!later_policy.enabled);
    assert_ne!(later_policy, immutable_policy);
    assert_eq!(plan.policy, immutable_policy);
    assert_eq!(plan.not_before, immutable_not_before);
    assert_eq!(plan.next_consecutive_attempts, immutable_attempts);
    assert!(plan.policy.enabled);
}

#[test]
fn app_claim_source_order_preserves_fast_gate_atomic_source_attach_and_claimed_only_redraw() {
    let source = include_str!("vod_endpoint_recovery.rs");
    let app_claim_start = source
        .find("fn claim_vod_endpoint_expiry")
        .expect("AppState claim function должна существовать");
    let app_claim_end = source[app_claim_start..]
        .find("/// Запускает exact re-extraction")
        .map(|offset| app_claim_start + offset)
        .expect("следующий recovery method ограничивает function body");
    let app_claim = &source[app_claim_start..app_claim_end];
    let gate = app_claim
        .find("has_pending_expiry_signal")
        .expect("no-signal gate должен существовать");
    let config = app_claim
        .find("committed_app_config")
        .expect("committed config read должен существовать");
    let player = app_claim
        .find("refresh_player_snapshot")
        .expect("player snapshot read должен существовать");
    let playlist = app_claim
        .find("playlist_view_snapshot")
        .expect("playlist identity read должен существовать");
    let owner_claim = app_claim
        .find("claim_pending_expiry_from_runtime_facts")
        .expect("owner claim должен существовать");
    let claimed_redraw = app_claim
        .find("if outcome == VodEndpointExpiryClaimOutcome::Claimed")
        .expect("redraw должен быть fenced typed Claimed outcome-ом");
    assert!(gate < config && config < player && player < playlist && playlist < owner_claim);
    assert!(owner_claim < claimed_redraw);

    let owner_claim_start = source
        .find("fn claim_pending_expiry_from_runtime_facts")
        .expect("runtime owner claim function должна существовать");
    let owner_claim_end = source[owner_claim_start..]
        .find("impl AppState")
        .map(|offset| owner_claim_start + offset)
        .expect("AppState impl ограничивает owner claim body");
    let owner_claim = &source[owner_claim_start..owner_claim_end];
    let source_clone = owner_claim
        .find("let source = binding.source.clone()")
        .expect("real Installed source должен клонироваться до signal consume");
    let admission = owner_claim
        .find("admit_claim_from_runtime_facts")
        .expect("source-neutral admission должен вызываться owner-ом");
    let pending_commit = owner_claim
        .find("self.pending = Some(PendingVodEndpointRecoveryAttempt")
        .expect("admitted plan должен атомарно публиковаться как pending");
    assert!(source_clone < admission && admission < pending_commit);
    assert!(owner_claim.contains("source,"));
    assert!(owner_claim.contains("claim,"));

    let poll_start = source
        .find("pub(crate) fn poll_vod_endpoint_recovery")
        .expect("production recovery poll должен существовать");
    let poll_end = source[poll_start..]
        .find("/// Redraw scheduler")
        .map(|offset| poll_start + offset)
        .expect("следующий method ограничивает recovery poll body");
    let poll = &source[poll_start..poll_end];
    let pending_gate = poll
        .find("self.vod_endpoint_recovery.pending.is_some()")
        .expect("pending attempt должен иметь приоритет над новым claim");
    let poll_pending = poll
        .find("self.poll_claimed_vod_endpoint_recovery")
        .expect("pending attempt должен продолжаться через owned policy");
    let next_claim = poll
        .find("self.claim_vod_endpoint_expiry")
        .expect("natural expiry claim должен оставаться fallback path-ом");
    assert!(pending_gate < poll_pending && poll_pending < next_claim);
    assert!(poll[poll_pending..next_claim].contains("return;"));
}
