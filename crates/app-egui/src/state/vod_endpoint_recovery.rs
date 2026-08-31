//! App-owned lifecycle VOD endpoint recovery после истечения подписанных URL.
//!
//! Transport сообщает только typed expiry fact. Этот модуль владеет более высоким
//! решением: переизвлечь exact logical yt-dlp source, сохранить same-lineage identity,
//! восстановить late-seek target и ограничить повторения конфигурируемым backoff-ом.

use std::time::{Duration, Instant};

use player_core::{MediaInstallCompletion, MediaInstanceId, PlaybackIntent, PlayerSnapshot};
use render_wgpu_shell::Renderer;
use tracing::{debug, warn};
use web_media_transport_api::{EndpointExpirySignal, SourceGeneration};

use super::{
    ActiveMediaSource, AppState, InstalledSingleMediaOpen, StrongMediaOpenPoll,
    playback_intent_from_snapshot,
};
use crate::media_open::MediaOpenSourceRequest;
use crate::playlist_runtime::{ActiveMediaIdentity, PlaylistRuntime};
use crate::web_media_vod_recovery::VodEndpointRecoveryAttachment;

/// Recovery state принадлежит app composition boundary, а не transport или demux crate-у.
#[derive(Default)]
pub(super) struct VodEndpointRecoveryRuntimeState {
    installed: Option<InstalledVodEndpointRecoveryBinding>,
    pending: Option<PendingVodEndpointRecoveryAttempt>,
}

/// Attachment связан только с exact Installed media instance и его logical source.
struct InstalledVodEndpointRecoveryBinding {
    source: ActiveMediaSource,
    claim_admission: InstalledVodEndpointRecoveryClaimAdmission,
}

/// Source-neutral admission state позволяет отдельно проверить runtime identity и policy fences.
struct InstalledVodEndpointRecoveryClaimAdmission {
    media_instance_id: MediaInstanceId,
    attachment: VodEndpointRecoveryAttachment,
    consecutive_attempts: u64,
    installed_at: Instant,
}

/// Одна claimed expiry generation порождает не более одной same-lineage транзакции.
struct PendingVodEndpointRecoveryAttempt {
    source: ActiveMediaSource,
    claim: VodEndpointRecoveryClaimPlan,
    strong_open_started: bool,
}

/// Полный admission plan без source reconstruction responsibility; policy snapshot неизменяем.
#[derive(Debug, Clone)]
struct VodEndpointRecoveryClaimPlan {
    expected_active: ActiveMediaIdentity,
    attachment: VodEndpointRecoveryAttachment,
    source_generation: SourceGeneration,
    restore_position: Duration,
    playback_intent: PlaybackIntent,
    policy: VodEndpointRecoveryPolicy,
    next_consecutive_attempts: u64,
    not_before: Instant,
}

/// Immutable policy snapshot не даёт live config mutation менять уже claimed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VodEndpointRecoveryPolicy {
    enabled: bool,
    max_consecutive_attempts: u64,
    initial_backoff: Duration,
    max_backoff: Duration,
    stable_reset: Duration,
}

/// Typed результат claim-а отделяет отсутствие сигнала от terminal rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VodEndpointExpiryClaimOutcome {
    NoSignal,
    Rejected,
    Claimed,
}

/// Admission ещё не содержит source и поэтому не может опубликовать half-built pending attempt.
enum VodEndpointExpiryAdmissionOutcome {
    NoSignal,
    Rejected,
    Admitted(VodEndpointRecoveryClaimPlan),
}

impl VodEndpointRecoveryPolicy {
    /// Переводит validated config в runtime units в одном месте.
    fn from_config(config: &rustiplayer_config::WebMediaConfig) -> Self {
        Self {
            enabled: config.vod_endpoint_recovery_enabled,
            max_consecutive_attempts: config.vod_endpoint_recovery_max_consecutive_attempts,
            initial_backoff: Duration::from_millis(config.vod_endpoint_recovery_initial_backoff_ms),
            max_backoff: Duration::from_millis(config.vod_endpoint_recovery_max_backoff_ms),
            stable_reset: Duration::from_millis(config.vod_endpoint_recovery_stable_reset_ms),
        }
    }

    /// Возвращает capped exponential backoff без переполнения integer arithmetic.
    fn backoff_for_attempt(self, consecutive_attempt: u64) -> Duration {
        let exponent = consecutive_attempt.saturating_sub(1).min(63) as u32;
        let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
        let milliseconds = self.initial_backoff.as_millis().min(u128::from(u64::MAX)) as u64;
        Duration::from_millis(
            milliseconds
                .saturating_mul(multiplier)
                .min(self.max_backoff.as_millis().min(u128::from(u64::MAX)) as u64),
        )
    }
}

impl InstalledVodEndpointRecoveryClaimAdmission {
    /// Создаёт source-neutral admission state из exact Installed lifecycle facts.
    fn new(
        media_instance_id: MediaInstanceId,
        attachment: VodEndpointRecoveryAttachment,
        consecutive_attempts: u64,
        installed_at: Instant,
    ) -> Self {
        Self {
            media_instance_id,
            attachment,
            consecutive_attempts,
            installed_at,
        }
    }

    /// Не заставляет composition boundary снимать snapshots без armed expiry.
    fn has_pending_expiry_signal(&self) -> bool {
        self.attachment.is_recovery_pending()
    }

    /// Claims signal и строит immutable plan только после config и обеих identity fences.
    fn admit_claim_from_runtime_facts(
        &self,
        config: &rustiplayer_config::WebMediaConfig,
        player_snapshot: &PlayerSnapshot,
        expected_active: Option<ActiveMediaIdentity>,
        now: Instant,
    ) -> VodEndpointExpiryAdmissionOutcome {
        if !self.attachment.is_recovery_pending() {
            return VodEndpointExpiryAdmissionOutcome::NoSignal;
        }
        if player_snapshot.media_instance_id != Some(self.media_instance_id)
            || expected_active
                .as_ref()
                .map(|identity| identity.media_instance_id())
                != Some(self.media_instance_id)
        {
            self.attachment.mark_recovery_failed();
            return VodEndpointExpiryAdmissionOutcome::Rejected;
        }
        let policy = VodEndpointRecoveryPolicy::from_config(config);
        let consecutive_attempts =
            if now.saturating_duration_since(self.installed_at) >= policy.stable_reset {
                0
            } else {
                self.consecutive_attempts
            };
        if !policy.enabled || consecutive_attempts >= policy.max_consecutive_attempts {
            warn!(
                enabled = policy.enabled,
                consecutive_attempts,
                max_consecutive_attempts = policy.max_consecutive_attempts,
                "VOD endpoint recovery budget исчерпан; публикуем исходную transport ошибку"
            );
            self.attachment.mark_recovery_failed();
            return VodEndpointExpiryAdmissionOutcome::Rejected;
        }
        let Some(signal) = self.attachment.claim_pending_signal() else {
            return VodEndpointExpiryAdmissionOutcome::NoSignal;
        };
        let expected_active = expected_active
            .expect("identity fence допускает admission только с exact active identity");
        let restore_position = player_snapshot
            .timeline
            .target_position
            .map(|target| target.as_duration())
            .unwrap_or(player_snapshot.current_position);
        let next_consecutive_attempts = consecutive_attempts.saturating_add(1);
        let backoff = policy.backoff_for_attempt(next_consecutive_attempts);
        debug_vod_expiry_claim(
            &signal,
            next_consecutive_attempts,
            backoff,
            restore_position,
        );
        VodEndpointExpiryAdmissionOutcome::Admitted(VodEndpointRecoveryClaimPlan {
            expected_active,
            attachment: self.attachment.clone(),
            source_generation: signal.source_generation(),
            restore_position,
            playback_intent: playback_intent_from_snapshot(player_snapshot),
            policy,
            next_consecutive_attempts,
            not_before: now + backoff,
        })
    }
}

impl VodEndpointRecoveryRuntimeState {
    /// Заменяет exact Installed binding, сохраняя создание полей внутри owner-а recovery state.
    fn bind_installed_runtime_facts(
        &mut self,
        media_instance_id: MediaInstanceId,
        source: ActiveMediaSource,
        attachment: VodEndpointRecoveryAttachment,
        consecutive_attempts: u64,
        installed_at: Instant,
    ) {
        self.installed = Some(InstalledVodEndpointRecoveryBinding {
            source,
            claim_admission: InstalledVodEndpointRecoveryClaimAdmission::new(
                media_instance_id,
                attachment,
                consecutive_attempts,
                installed_at,
            ),
        });
    }

    /// Не заставляет app снимать player/playlist snapshots без armed expiry.
    fn has_pending_expiry_signal(&self) -> bool {
        self.installed
            .as_ref()
            .is_some_and(|binding| binding.claim_admission.has_pending_expiry_signal())
    }

    /// Атомарно добавляет real Installed source только к полностью admitted owned plan-у.
    fn claim_pending_expiry_from_runtime_facts(
        &mut self,
        config: &rustiplayer_config::WebMediaConfig,
        player_snapshot: &PlayerSnapshot,
        expected_active: Option<ActiveMediaIdentity>,
        now: Instant,
    ) -> VodEndpointExpiryClaimOutcome {
        let Some(binding) = self.installed.as_ref() else {
            return VodEndpointExpiryClaimOutcome::NoSignal;
        };
        // Clone может аллоцировать, поэтому выполняется до consume единственного expiry signal-а.
        let source = binding.source.clone();
        match binding.claim_admission.admit_claim_from_runtime_facts(
            config,
            player_snapshot,
            expected_active,
            now,
        ) {
            VodEndpointExpiryAdmissionOutcome::NoSignal => VodEndpointExpiryClaimOutcome::NoSignal,
            VodEndpointExpiryAdmissionOutcome::Rejected => VodEndpointExpiryClaimOutcome::Rejected,
            VodEndpointExpiryAdmissionOutcome::Admitted(claim) => {
                self.pending = Some(PendingVodEndpointRecoveryAttempt {
                    source,
                    claim,
                    strong_open_started: false,
                });
                VodEndpointExpiryClaimOutcome::Claimed
            }
        }
    }
}

impl AppState {
    /// Привязывает recovery gate только к exact Installed result, никогда к Prepared candidate-у.
    pub(super) fn bind_installed_vod_endpoint_recovery(
        &mut self,
        installed: &InstalledSingleMediaOpen,
    ) {
        let MediaInstallCompletion::Installed {
            media_instance_id, ..
        } = installed.completion
        else {
            self.vod_endpoint_recovery = VodEndpointRecoveryRuntimeState::default();
            return;
        };
        let Some(attachment) = installed.descriptor().vod_endpoint_recovery() else {
            self.vod_endpoint_recovery = VodEndpointRecoveryRuntimeState::default();
            return;
        };
        let consecutive_attempts = self
            .vod_endpoint_recovery
            .pending
            .as_ref()
            .filter(|pending| pending.strong_open_started)
            .map_or(0, |pending| pending.claim.next_consecutive_attempts);
        self.vod_endpoint_recovery.bind_installed_runtime_facts(
            media_instance_id,
            installed.source.clone(),
            attachment,
            consecutive_attempts,
            Instant::now(),
        );
    }

    /// Сбрасывает runtime-only attachment при install path без полного descriptor-а.
    pub(super) fn clear_installed_vod_endpoint_recovery(&mut self) {
        self.vod_endpoint_recovery = VodEndpointRecoveryRuntimeState::default();
    }

    /// Suspend/resume protocol уже владеет exact instance, но не возвращает общий Installed envelope.
    pub(super) fn bind_resumed_vod_endpoint_recovery(
        &mut self,
        media_instance_id: MediaInstanceId,
        source: ActiveMediaSource,
        attachment: Option<VodEndpointRecoveryAttachment>,
    ) {
        self.vod_endpoint_recovery = VodEndpointRecoveryRuntimeState::default();
        if let Some(attachment) = attachment {
            self.vod_endpoint_recovery.bind_installed_runtime_facts(
                media_instance_id,
                source,
                attachment,
                0,
                Instant::now(),
            );
        }
    }

    /// Продвигает recovery не более чем на один strong-open poll step за UI frame.
    pub(crate) fn poll_vod_endpoint_recovery(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) {
        if self.vod_endpoint_recovery.pending.is_some() {
            self.poll_claimed_vod_endpoint_recovery(playlist_runtime, renderer);
            return;
        }
        self.claim_vod_endpoint_expiry(playlist_runtime);
    }

    /// Redraw scheduler обязан продолжать poll во время backoff и strong transaction.
    #[must_use]
    pub(super) fn has_pending_vod_endpoint_recovery(&self) -> bool {
        self.vod_endpoint_recovery.pending.is_some()
    }

    /// Claims signal только после exact instance fence и budget admission.
    fn claim_vod_endpoint_expiry(&mut self, playlist_runtime: &PlaylistRuntime) {
        if !self.vod_endpoint_recovery.has_pending_expiry_signal() {
            return;
        }
        let web_media_config = self.committed_app_config().web_media;
        let snapshot = self.refresh_player_snapshot();
        let expected_active = playlist_runtime.playlist_view_snapshot().active_media();
        let outcome = self
            .vod_endpoint_recovery
            .claim_pending_expiry_from_runtime_facts(
                &web_media_config,
                &snapshot,
                expected_active,
                Instant::now(),
            );
        if outcome == VodEndpointExpiryClaimOutcome::Claimed {
            self.mark_pending_worker_redraw();
        }
    }

    /// Запускает exact re-extraction после backoff и затем использует общий staged install.
    fn poll_claimed_vod_endpoint_recovery(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) {
        let Some(mut pending) = self.vod_endpoint_recovery.pending.take() else {
            return;
        };
        if !pending.strong_open_started {
            let active_media = playlist_runtime.playlist_view_snapshot().active_media();
            let snapshot = self.refresh_player_snapshot();
            if active_media != Some(pending.claim.expected_active)
                || snapshot.media_instance_id
                    != Some(pending.claim.expected_active.media_instance_id())
                || pending.claim.attachment.pending_source_generation()
                    != Some(pending.claim.source_generation)
            {
                debug!(
                    source_generation = pending.claim.source_generation.value(),
                    "VOD endpoint recovery отменён exact identity/generation fence-ом"
                );
                pending.claim.attachment.mark_recovery_failed();
                return;
            }
            if Instant::now() < pending.claim.not_before {
                self.vod_endpoint_recovery.pending = Some(pending);
                return;
            }
            if self.has_pending_prepared_media_strong() {
                self.vod_endpoint_recovery.pending = Some(pending);
                return;
            }
            let source_request = match self.vod_recovery_source_request(&pending.source) {
                Ok(source_request) => source_request,
                Err(reason) => {
                    warn!(reason, "Не удалось построить exact VOD recovery request");
                    pending.claim.attachment.mark_recovery_failed();
                    return;
                }
            };
            if let Err(error) = self.begin_vod_endpoint_recovery_strong(
                playlist_runtime,
                renderer,
                source_request,
                pending.claim.expected_active,
                pending.claim.playback_intent,
                pending.claim.restore_position,
            ) {
                warn!(
                    error = %error,
                    source_generation = pending.claim.source_generation.value(),
                    "Не удалось запустить VOD endpoint recovery"
                );
                pending.claim.attachment.mark_recovery_failed();
                return;
            }
            pending.strong_open_started = true;
        }
        self.vod_endpoint_recovery.pending = Some(pending);
        match self.poll_prepared_media_strong(playlist_runtime) {
            StrongMediaOpenPoll::Pending => {}
            StrongMediaOpenPoll::Installed(installed) => {
                debug!(
                    consecutive_attempt = self
                        .vod_endpoint_recovery
                        .pending
                        .as_ref()
                        .map_or(0, |pending| pending.claim.next_consecutive_attempts),
                    "VOD endpoint recovery установил fresh candidate той же lineage"
                );
                // Exact strong commit уже вызвал `record_installed_media` и привязал новый gate.
                drop(installed);
                self.vod_endpoint_recovery.pending = None;
            }
            StrongMediaOpenPoll::Failed(error) => {
                let Some(mut pending) = self.vod_endpoint_recovery.pending.take() else {
                    return;
                };
                if error.allows_vod_endpoint_recovery_retry()
                    && pending.claim.next_consecutive_attempts
                        < pending.claim.policy.max_consecutive_attempts
                {
                    pending.claim.next_consecutive_attempts =
                        pending.claim.next_consecutive_attempts.saturating_add(1);
                    let retry_backoff = pending
                        .claim
                        .policy
                        .backoff_for_attempt(pending.claim.next_consecutive_attempts);
                    pending.claim.not_before = Instant::now() + retry_backoff;
                    pending.strong_open_started = false;
                    warn!(
                        error = %error,
                        next_consecutive_attempt = pending.claim.next_consecutive_attempts,
                        retry_backoff_ms = retry_backoff.as_millis(),
                        "VOD endpoint recovery pre-barrier failure будет повторён"
                    );
                    self.vod_endpoint_recovery.pending = Some(pending);
                } else {
                    warn!(error = %error, "VOD endpoint recovery завершился terminal failure");
                    pending.claim.attachment.mark_recovery_failed();
                }
            }
        }
    }

    fn vod_recovery_source_request(
        &self,
        source: &ActiveMediaSource,
    ) -> Result<MediaOpenSourceRequest, &'static str> {
        let config = self.committed_app_config();
        let web_intent = source
            .web_intent()
            .filter(|intent| {
                intent.recovery()
                    == web_media_core::WebMediaRecoveryStrategy::FreshExtractionAndRematch
            })
            .ok_or("installed recovery attachment не принадлежит extractor source")?;
        let capabilities = self
            .system_capabilities_snapshot
            .as_ref()
            .ok_or("system capabilities snapshot отсутствует")?;
        let settings = crate::media_open::WebMediaOpenSettings::from_app_config(
            &config,
            capabilities,
            self.audio_decode_capability_snapshot(),
        );
        let request = web_intent
            .controlled_reopen_request(config.network.clone(), config.player.demux, Some(settings))
            .ok_or("extractor controlled reopen settings отсутствуют")?;
        Ok(source.wrap_reopen_request(MediaOpenSourceRequest::Web(request)))
    }
}

/// Structured logging не раскрывает endpoint или request headers.
fn debug_vod_expiry_claim(
    signal: &EndpointExpirySignal,
    consecutive_attempt: u64,
    backoff: Duration,
    restore_position: Duration,
) {
    debug!(
        component = ?signal.component(),
        source_generation = signal.source_generation().value(),
        resource_kind = ?signal.resource_kind(),
        reason = ?signal.reason(),
        consecutive_attempt,
        backoff_ms = backoff.as_millis(),
        restore_position_ms = restore_position.as_millis(),
        "Claimed typed VOD endpoint expiry signal"
    );
}

#[cfg(test)]
#[path = "vod_endpoint_recovery_claim_policy_tests.rs"]
mod claim_policy_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_is_exponential_capped_and_starts_at_initial_delay() {
        let policy = VodEndpointRecoveryPolicy {
            enabled: true,
            max_consecutive_attempts: 5,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_millis(900),
            stable_reset: Duration::from_secs(30),
        };

        assert_eq!(policy.backoff_for_attempt(1), Duration::from_millis(250));
        assert_eq!(policy.backoff_for_attempt(2), Duration::from_millis(500));
        assert_eq!(policy.backoff_for_attempt(3), Duration::from_millis(900));
        assert_eq!(policy.backoff_for_attempt(63), Duration::from_millis(900));
    }
}
