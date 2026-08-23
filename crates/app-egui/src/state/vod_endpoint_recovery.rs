//! App-owned lifecycle VOD endpoint recovery после истечения подписанных URL.
//!
//! Transport сообщает только typed expiry fact. Этот модуль владеет более высоким
//! решением: переизвлечь exact logical yt-dlp source, сохранить same-lineage identity,
//! восстановить late-seek target и ограничить повторения конфигурируемым backoff-ом.

use std::time::{Duration, Instant};

use player_core::{MediaInstallCompletion, MediaInstanceId, PlaybackIntent};
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
    media_instance_id: MediaInstanceId,
    source: ActiveMediaSource,
    attachment: VodEndpointRecoveryAttachment,
    consecutive_attempts: u64,
    installed_at: Instant,
}

/// Одна claimed expiry generation порождает не более одной same-lineage транзакции.
struct PendingVodEndpointRecoveryAttempt {
    expected_active: ActiveMediaIdentity,
    source: ActiveMediaSource,
    attachment: VodEndpointRecoveryAttachment,
    source_generation: SourceGeneration,
    restore_position: Duration,
    playback_intent: PlaybackIntent,
    policy: VodEndpointRecoveryPolicy,
    next_consecutive_attempts: u64,
    not_before: Instant,
    strong_open_started: bool,
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

impl VodEndpointRecoveryPolicy {
    /// Переводит validated config в runtime units в одном месте.
    fn from_config(config: &rustiplayer_config::YtDlpConfig) -> Self {
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
            .map_or(0, |pending| pending.next_consecutive_attempts);
        self.vod_endpoint_recovery.installed = Some(InstalledVodEndpointRecoveryBinding {
            media_instance_id,
            source: installed.source.clone(),
            attachment,
            consecutive_attempts,
            installed_at: Instant::now(),
        });
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
        self.vod_endpoint_recovery = VodEndpointRecoveryRuntimeState {
            installed: attachment.map(|attachment| InstalledVodEndpointRecoveryBinding {
                media_instance_id,
                source,
                attachment,
                consecutive_attempts: 0,
                installed_at: Instant::now(),
            }),
            pending: None,
        };
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
        let Some(binding) = self.vod_endpoint_recovery.installed.as_ref() else {
            return;
        };
        if !binding.attachment.is_recovery_pending() {
            return;
        }
        let media_instance_id = binding.media_instance_id;
        let source = binding.source.clone();
        let attachment = binding.attachment.clone();
        let installed_at = binding.installed_at;
        let installed_consecutive_attempts = binding.consecutive_attempts;
        let policy = VodEndpointRecoveryPolicy::from_config(&self.committed_app_config().yt_dlp);
        let snapshot = self.refresh_player_snapshot();
        let expected_active = playlist_runtime.playlist_view_snapshot().active_media();
        if snapshot.media_instance_id != Some(media_instance_id)
            || expected_active.map(ActiveMediaIdentity::media_instance_id)
                != Some(media_instance_id)
        {
            attachment.mark_recovery_failed();
            return;
        }
        let consecutive_attempts = if installed_at.elapsed() >= policy.stable_reset {
            0
        } else {
            installed_consecutive_attempts
        };
        if !policy.enabled || consecutive_attempts >= policy.max_consecutive_attempts {
            warn!(
                enabled = policy.enabled,
                consecutive_attempts,
                max_consecutive_attempts = policy.max_consecutive_attempts,
                "VOD endpoint recovery budget исчерпан; публикуем исходную transport ошибку"
            );
            attachment.mark_recovery_failed();
            return;
        }
        let Some(signal) = attachment.claim_pending_signal() else {
            return;
        };
        let Some(expected_active) = expected_active else {
            attachment.mark_recovery_failed();
            return;
        };
        let next_consecutive_attempts = consecutive_attempts.saturating_add(1);
        let backoff = policy.backoff_for_attempt(next_consecutive_attempts);
        let restore_position = snapshot
            .timeline
            .target_position
            .map(|target| target.as_duration())
            .unwrap_or(snapshot.current_position);
        debug_vod_expiry_claim(
            &signal,
            next_consecutive_attempts,
            backoff,
            restore_position,
        );
        self.vod_endpoint_recovery.pending = Some(PendingVodEndpointRecoveryAttempt {
            expected_active,
            source,
            attachment,
            source_generation: signal.source_generation(),
            restore_position,
            playback_intent: playback_intent_from_snapshot(&snapshot),
            policy,
            next_consecutive_attempts,
            not_before: Instant::now() + backoff,
            strong_open_started: false,
        });
        self.mark_pending_worker_redraw();
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
            if active_media != Some(pending.expected_active)
                || snapshot.media_instance_id != Some(pending.expected_active.media_instance_id())
                || pending.attachment.pending_source_generation() != Some(pending.source_generation)
            {
                debug!(
                    source_generation = pending.source_generation.value(),
                    "VOD endpoint recovery отменён exact identity/generation fence-ом"
                );
                pending.attachment.mark_recovery_failed();
                return;
            }
            if Instant::now() < pending.not_before {
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
                    pending.attachment.mark_recovery_failed();
                    return;
                }
            };
            if let Err(error) = self.begin_vod_endpoint_recovery_strong(
                playlist_runtime,
                renderer,
                source_request,
                pending.expected_active,
                pending.playback_intent,
                pending.restore_position,
            ) {
                warn!(
                    error = %error,
                    source_generation = pending.source_generation.value(),
                    "Не удалось запустить VOD endpoint recovery"
                );
                pending.attachment.mark_recovery_failed();
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
                        .map_or(0, |pending| pending.next_consecutive_attempts),
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
                    && pending.next_consecutive_attempts < pending.policy.max_consecutive_attempts
                {
                    pending.next_consecutive_attempts =
                        pending.next_consecutive_attempts.saturating_add(1);
                    let retry_backoff = pending
                        .policy
                        .backoff_for_attempt(pending.next_consecutive_attempts);
                    pending.not_before = Instant::now() + retry_backoff;
                    pending.strong_open_started = false;
                    warn!(
                        error = %error,
                        next_consecutive_attempt = pending.next_consecutive_attempts,
                        retry_backoff_ms = retry_backoff.as_millis(),
                        "VOD endpoint recovery pre-barrier failure будет повторён"
                    );
                    self.vod_endpoint_recovery.pending = Some(pending);
                } else {
                    warn!(error = %error, "VOD endpoint recovery завершился terminal failure");
                    pending.attachment.mark_recovery_failed();
                }
            }
        }
    }

    /// Создаёт request только из logical source и exact semantic candidate selection.
    fn vod_recovery_source_request(
        &self,
        source: &ActiveMediaSource,
    ) -> Result<MediaOpenSourceRequest, &'static str> {
        let config = self.committed_app_config();
        let ActiveMediaSource::YtDlpUrl {
            source_locator,
            candidate_selection,
            composed_selection,
            stream_configuration,
            ..
        } = source.physical_source()
        else {
            return Err("installed recovery attachment не принадлежит yt-dlp source");
        };
        let capabilities = self
            .system_capabilities_snapshot
            .clone()
            .ok_or("system capabilities snapshot отсутствует")?;
        let selection_intent = match composed_selection {
            Some(composed) => crate::web_media_open::YtDlpCandidateOpenIntent::composed(
                composed.clone(),
                candidate_selection.clone(),
                stream_configuration.preference(),
            ),
            None => crate::web_media_open::YtDlpCandidateOpenIntent::exact_preserving_installed_stream_configuration(
                candidate_selection.clone(),
                stream_configuration,
            ),
        };
        let physical_request = MediaOpenSourceRequest::YtDlp {
            locator: source_locator.clone(),
            selection_intent,
            network_config: config.network,
            yt_dlp_config: config.yt_dlp,
            demux_config: config.player.demux,
            preferred_video_codec_order: config.player.preferred_video_codec_order,
            system_capabilities: Box::new(capabilities),
            audio_capabilities: self.audio_decode_capability_snapshot(),
        };
        Ok(source.wrap_reopen_request(physical_request))
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
