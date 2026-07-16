//! Renderer-bound execution receipts для playlist transport intents.
//!
//! Traversal/plan остаются в `PlaylistRuntime`; здесь живут только app candidate resources,
//! coordinator request correlation и player receipts, нужные renderer owner-у.

use player_core::{
    ExactMediaTransportOutcome, ExactMediaTransportReceipt, ExactMediaTransportRequest,
    MediaInstallCancellationCause,
};
use render_wgpu_shell::Renderer;
use tracing::{debug, warn};

use super::{AppState, StrongMediaOpenPoll};
use crate::media_open::{MediaOpenRequestId, MediaOpenSourceRequest};
use crate::playlist_runtime::{
    ControllerStableIntentDispatch, PlannedPlaylistInstall, PlaylistRuntime,
};
use crate::url_service_adapter::{StartupUrlClassification, classify_playlist_url};

struct QueuedPlaylistInstall {
    install: PlannedPlaylistInstall,
    supersedes: Option<MediaOpenRequestId>,
}

/// Bounded state: один active strong request, один latest queued D53 plan и receipts.
#[derive(Default)]
pub(super) struct PlaylistTransportRuntimeState {
    active_request_id: Option<MediaOpenRequestId>,
    active_item_id: Option<playlist_core::PlaylistItemId>,
    queued_install: Option<QueuedPlaylistInstall>,
    exact_receipts: Vec<ExactMediaTransportReceipt>,
    intent_receipts: Vec<player_core::PlaybackIntentUpdateReceipt>,
}

impl AppState {
    /// Превращает exact controller plan в source request и запускает общий strong protocol.
    pub(crate) fn begin_planned_playlist_install(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        install: PlannedPlaylistInstall,
        supersedes: Option<MediaOpenRequestId>,
    ) {
        if let Some(active_request_id) = self.playlist_transport.active_request_id {
            self.playlist_transport.queued_install = Some(QueuedPlaylistInstall {
                install,
                // До player staging controller ещё не связал old request, поэтому replacement
                // принимается как обычный latest plan после cancel terminal.
                supersedes: None,
            });
            if let Err(error) = playlist_runtime.cancel_media_open_lossless(
                active_request_id,
                MediaInstallCancellationCause::Superseded,
            ) {
                warn!(error = %error, "Не удалось supersede playlist preparation");
            }
            self.mark_pending_worker_redraw();
            return;
        }
        let source_request = match self.playlist_source_request(playlist_runtime, &install) {
            Ok(request) => request,
            Err(error) => {
                playlist_runtime.report_unstaged_playlist_navigation_failure(install.item_id);
                warn!(error = %error, "Playlist transport target не прошёл source boundary");
                return;
            }
        };
        let item_id = install.item_id;
        match self.begin_playlist_source_media_strong(
            playlist_runtime,
            renderer,
            source_request,
            install,
            supersedes,
        ) {
            Ok(request_id) => {
                self.playlist_transport.active_request_id = Some(request_id);
                self.playlist_transport.active_item_id = Some(item_id);
                self.mark_pending_worker_redraw();
            }
            Err(error) => {
                playlist_runtime.report_unstaged_playlist_navigation_failure(item_id);
                warn!(error = %error, "Не удалось начать playlist navigation install");
            }
        }
    }

    /// D53 отменяет exact pre-Ready request и хранит только последний replacement plan.
    pub(crate) fn supersede_planned_playlist_install(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        expected_request_id: MediaOpenRequestId,
        cause: MediaInstallCancellationCause,
        install: PlannedPlaylistInstall,
    ) {
        self.playlist_transport.queued_install = Some(QueuedPlaylistInstall {
            install,
            supersedes: Some(expected_request_id),
        });
        if self.playlist_transport.active_request_id == Some(expected_request_id)
            && let Err(error) =
                playlist_runtime.cancel_media_open_lossless(expected_request_id, cause)
        {
            warn!(error = %error, "Не удалось отменить superseded playlist request");
        }
        self.mark_pending_worker_redraw();
    }

    /// Reserved-before-dispatch abort уже снял старый controller guard; replacement обычный.
    pub(crate) fn replace_aborted_playlist_install(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        request_id: MediaOpenRequestId,
        cause: MediaInstallCancellationCause,
        next: Option<PlannedPlaylistInstall>,
    ) {
        self.playlist_transport.queued_install = next.map(|install| QueuedPlaylistInstall {
            install,
            supersedes: None,
        });
        if let Err(error) = playlist_runtime.cancel_media_open_lossless(request_id, cause) {
            warn!(error = %error, "Не удалось завершить aborted playlist request");
        }
        self.mark_pending_worker_redraw();
    }

    /// Отправляет exact D17/D51 request и сохраняет authoritative receipt до outcome-а.
    pub(crate) fn dispatch_exact_playlist_transport(
        &mut self,
        request: ExactMediaTransportRequest,
    ) {
        match self.player_worker.exact_media_transport(request) {
            Ok(receipt) => {
                self.playlist_transport.exact_receipts.push(receipt);
                self.mark_pending_worker_redraw();
            }
            Err(error) => {
                warn!(error = %error, "Exact playlist transport не принят player worker-ом")
            }
        }
    }

    /// D52 обновляет staged request и post-Installed intent snapshot без fallback dispatch.
    pub(crate) fn apply_playlist_stable_intent_dispatch(
        &mut self,
        playlist_runtime: &PlaylistRuntime,
        dispatch: ControllerStableIntentDispatch,
    ) {
        if let Some(update) = dispatch.pending_update {
            match playlist_runtime.apply_stable_pending_intent_update(&dispatch) {
                Ok(Some((request_id, receipt))) => {
                    self.update_pending_strong_playlist_intent(
                        request_id,
                        update.revision,
                        update.intent,
                    );
                    self.playlist_transport.intent_receipts.push(receipt);
                }
                Ok(None) => {}
                Err(error) => warn!(error = %error, "D52 playlist intent update отклонён"),
            }
        }
        if let Some(request) = dispatch.exact_current {
            self.dispatch_exact_playlist_transport(request);
        }
    }

    /// Продвигает navigation-owned strong request и неблокирующе drain-ит exact receipts.
    pub(crate) fn poll_playlist_transport(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) {
        self.poll_exact_playlist_transport_receipts();
        self.poll_playlist_intent_receipts();
        let Some(active_request_id) = self.playlist_transport.active_request_id else {
            return;
        };
        let active_item_id = self
            .playlist_transport
            .active_item_id
            .expect("active playlist request always retains its exact item ID");
        match self.poll_prepared_media_strong(playlist_runtime) {
            StrongMediaOpenPoll::Pending => {}
            StrongMediaOpenPoll::Installed(installed) => {
                self.record_installed_media_source(installed.source);
                self.playlist_transport.active_request_id = None;
                self.playlist_transport.active_item_id = None;
                if let Some(queued) = self.playlist_transport.queued_install.take() {
                    // Installed old request уже завершил supersede boundary; latest D53 plan
                    // начинает обычную новую транзакцию от нового committed current.
                    self.begin_planned_playlist_install(
                        playlist_runtime,
                        renderer,
                        queued.install,
                        None,
                    );
                }
            }
            StrongMediaOpenPoll::Failed(error) => {
                let failed_request_id = error.terminal_request_id().unwrap_or(active_request_id);
                self.playlist_transport.active_request_id = None;
                self.playlist_transport.active_item_id = None;
                if let Some(queued) = self.playlist_transport.queued_install.take() {
                    self.begin_planned_playlist_install(
                        playlist_runtime,
                        renderer,
                        queued.install,
                        queued.supersedes,
                    );
                } else {
                    playlist_runtime
                        .report_playlist_navigation_failure(failed_request_id, active_item_id);
                    warn!(error = %error, "Playlist navigation install завершился ошибкой");
                }
            }
        }
    }

    pub(crate) fn has_pending_playlist_transport(&self) -> bool {
        self.playlist_transport.active_request_id.is_some()
            || !self.playlist_transport.exact_receipts.is_empty()
            || !self.playlist_transport.intent_receipts.is_empty()
    }

    fn playlist_source_request(
        &self,
        playlist_runtime: &PlaylistRuntime,
        install: &PlannedPlaylistInstall,
    ) -> Result<MediaOpenSourceRequest, &'static str> {
        let locator = playlist_runtime
            .locator_for_planned_install(install)
            .map_err(|_| "stale playlist target")?;
        let config = self.committed_app_config();
        if let Some(local) = locator.as_local() {
            let path = local
                .expose_native_path_for_open()
                .ok_or("local path is unavailable on this platform")?;
            return Ok(MediaOpenSourceRequest::Local {
                path: path.to_path_buf(),
                expected_fingerprint: None,
                demux_config: config.player.demux,
            });
        }
        let secret_url = locator
            .as_secret_url()
            .ok_or("unsupported playlist locator")?;
        let classified = classify_playlist_url(secret_url);
        let StartupUrlClassification::Supported(locator) = classified else {
            return Err("persisted URL is no longer supported");
        };
        let capabilities = self
            .system_capabilities_snapshot
            .as_ref()
            .ok_or("system capabilities are unavailable")?;
        locator
            .into_media_open_source_request(&config, capabilities)
            .map_err(|_| "URL service rejected committed configuration")
    }

    fn poll_exact_playlist_transport_receipts(&mut self) {
        let had_pending_receipts = !self.playlist_transport.exact_receipts.is_empty();
        self.playlist_transport
            .exact_receipts
            .retain(|receipt| match receipt.try_take_outcome() {
                Ok(None) => true,
                Ok(Some(ExactMediaTransportOutcome::Applied { .. })) => false,
                Ok(Some(outcome)) => {
                    warn!(
                        ?outcome,
                        "Exact playlist transport завершился typed failure/stale outcome"
                    );
                    false
                }
                Err(error) => {
                    warn!(error = %error, "Exact playlist transport потерял owner outcome");
                    false
                }
            });
        if had_pending_receipts && self.playlist_transport.exact_receipts.is_empty() {
            debug!("Exact playlist transport receipts drained");
        }
    }

    fn poll_playlist_intent_receipts(&mut self) {
        self.playlist_transport
            .intent_receipts
            .retain(|receipt| match receipt.try_outcome() {
                None => true,
                Some(
                    player_core::PlaybackIntentUpdateOutcome::AppliedToStaged
                    | player_core::PlaybackIntentUpdateOutcome::AppliedToInstalled { .. },
                ) => false,
                Some(outcome) => {
                    warn!(
                        ?outcome,
                        "D52 playlist intent получил stale/rejected outcome"
                    );
                    false
                }
            });
    }
}
