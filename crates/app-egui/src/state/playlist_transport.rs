//! Renderer-bound execution receipts для playlist transport intents.
//!
//! Traversal/plan остаются в `PlaylistRuntime`; здесь живут только app candidate resources,
//! coordinator request correlation и player receipts, нужные renderer owner-у.

use player_core::{
    ExactMediaTransportOutcome, ExactMediaTransportReceipt, ExactMediaTransportRequest,
    ExactTimelineSeekOutcome, ExactTimelineSeekReceipt, ExactTimelineSeekRequest,
    MediaInstallCancellationCause,
};
use render_wgpu_shell::Renderer;
use tracing::{debug, warn};

use super::{AppState, StrongMediaOpenError, StrongMediaOpenPoll};
use crate::media_open::{MediaOpenRequestId, MediaOpenSourceRequest};
use crate::playlist_runtime::{
    ControllerStableIntentDispatch, PlannedPlaylistInstall, PlaylistRuntime,
};
use crate::url_service_adapter::{StartupUrlClassification, classify_playlist_url};

struct QueuedPlaylistInstall {
    install: PlannedPlaylistInstall,
    supersedes: Option<MediaOpenRequestId>,
}

/// Receipt хранит controller intent, который разрешено commit-ить только после owner outcome.
struct PendingExactTransportReceipt {
    receipt: ExactMediaTransportReceipt,
    purpose: ExactTransportReceiptPurpose,
}

/// Один receipt driver обслуживает transport и полный Clear reset без смешения lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExactTransportReceiptPurpose {
    OrdinaryTransport,
    NeutralStop,
    ClearMediaReset {
        requested_media_instance_id: player_core::MediaInstanceId,
    },
}

/// Bounded state: один active strong request, один latest queued D53 plan и receipts.
#[derive(Default)]
pub(super) struct PlaylistTransportRuntimeState {
    active_request_id: Option<MediaOpenRequestId>,
    active_item_id: Option<playlist_core::PlaylistItemId>,
    queued_install: Option<QueuedPlaylistInstall>,
    exact_receipts: Vec<PendingExactTransportReceipt>,
    timeline_seek_receipts: Vec<ExactTimelineSeekReceipt>,
    intent_receipts: Vec<player_core::PlaybackIntentUpdateReceipt>,
}

/// Startup first-item start сохраняет source-admission и coordinator failure отдельно.
#[derive(Debug)]
pub(crate) enum StartupPlaylistInstallStartError {
    /// Committed locator/window/config не построили source request.
    Source(&'static str),
    /// Общий strong-open protocol отклонил start.
    Strong(StrongMediaOpenError),
}

impl std::fmt::Display for StartupPlaylistInstallStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(reason) => write!(formatter, "startup playlist source rejected: {reason}"),
            Self::Strong(error) => {
                write!(formatter, "startup playlist strong-open rejected: {error}")
            }
        }
    }
}

impl std::error::Error for StartupPlaylistInstallStartError {}

impl AppState {
    /// Запускает exact post-commit first item без normal transport queue и sibling scan.
    pub(crate) fn begin_startup_playlist_install(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        install: PlannedPlaylistInstall,
    ) -> Result<(), StartupPlaylistInstallStartError> {
        let item_id = install.item_id;
        let source_request = match self.playlist_source_request(playlist_runtime, &install) {
            Ok(source_request) => source_request,
            Err(error) => {
                playlist_runtime.report_unstaged_playlist_navigation_failure(item_id);
                return Err(StartupPlaylistInstallStartError::Source(error));
            }
        };
        match self.begin_playlist_source_media_strong(
            playlist_runtime,
            renderer,
            source_request,
            install,
            None,
        ) {
            Ok(_) => Ok(()),
            Err(error) => {
                playlist_runtime.report_unstaged_playlist_navigation_failure(item_id);
                Err(StartupPlaylistInstallStartError::Strong(error))
            }
        }
    }

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
        let purpose = if matches!(
            request.action,
            player_core::ExactMediaTransportAction::NeutralStop
        ) {
            ExactTransportReceiptPurpose::NeutralStop
        } else {
            ExactTransportReceiptPurpose::OrdinaryTransport
        };
        match self.player_worker.exact_media_transport(request) {
            Ok(receipt) => {
                self.playlist_transport
                    .exact_receipts
                    .push(PendingExactTransportReceipt { receipt, purpose });
                self.mark_pending_worker_redraw();
            }
            Err(error) => {
                warn!(error = %error, "Exact playlist transport не принят player worker-ом")
            }
        }
    }

    /// Ставит correlated seek и хранит receipt до terminal player owner outcome-а.
    pub(crate) fn dispatch_exact_timeline_seek(&mut self, request: ExactTimelineSeekRequest) {
        match self.player_worker.exact_timeline_seek(request) {
            Ok(receipt) => {
                self.playlist_transport.timeline_seek_receipts.push(receipt);
                self.mark_pending_worker_redraw();
            }
            Err(error) => warn!(error = %error, "Exact timeline seek не принят player worker-ом"),
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
        self.drive_pending_playlist_media_reset(playlist_runtime);
        self.poll_exact_playlist_transport_receipts(playlist_runtime);
        let route_relative_beyond_end = self.poll_exact_timeline_seek_receipts(playlist_runtime);
        if route_relative_beyond_end {
            let snapshot = self.last_player_snapshot.clone();
            crate::transport_runtime::request_navigation(
                self,
                playlist_runtime,
                renderer,
                playlist_core::ManualNavigationDirection::Next,
                &snapshot,
                crate::playlist_runtime::TransportActionOrigin::Mpris,
            );
        }
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

    /// Неблокирующе отправляет latest Clear reset через общий frame transport driver.
    fn drive_pending_playlist_media_reset(&mut self, playlist_runtime: &mut PlaylistRuntime) {
        let Some(request) = playlist_runtime.pending_media_reset_request() else {
            return;
        };
        match self.player_worker.exact_media_transport(request) {
            Ok(receipt) => {
                playlist_runtime.mark_media_reset_dispatched(request);
                self.playlist_transport
                    .exact_receipts
                    .push(PendingExactTransportReceipt {
                        receipt,
                        purpose: ExactTransportReceiptPurpose::ClearMediaReset {
                            requested_media_instance_id: request.media_instance_id,
                        },
                    });
                self.mark_pending_worker_redraw();
            }
            Err(error) => {
                playlist_runtime.report_media_reset_send_error(request, error);
                if error == player_core::PlayerWorkerSendError::Full {
                    self.mark_pending_worker_redraw();
                }
            }
        }
    }

    pub(crate) fn has_pending_playlist_transport(&self) -> bool {
        self.playlist_transport.active_request_id.is_some()
            || !self.playlist_transport.exact_receipts.is_empty()
            || !self.playlist_transport.timeline_seek_receipts.is_empty()
            || !self.playlist_transport.intent_receipts.is_empty()
    }

    fn playlist_source_request(
        &self,
        playlist_runtime: &PlaylistRuntime,
        install: &PlannedPlaylistInstall,
    ) -> Result<MediaOpenSourceRequest, &'static str> {
        let open_intent = playlist_runtime
            .media_open_intent_for_planned_install(install)
            .map_err(|_| "stale playlist target")?;
        let locator = open_intent.locator();
        let config = self.committed_app_config();
        let physical_request = if let Some(local) = locator.as_local() {
            let path = local
                .expose_native_path_for_open()
                .ok_or("local path is unavailable on this platform")?;
            MediaOpenSourceRequest::Local {
                path: path.to_path_buf(),
                expected_fingerprint: None,
                demux_config: config.player.demux,
            }
        } else {
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
                .map_err(|_| "URL service rejected committed configuration")?
        };
        Ok(match open_intent.playback_window() {
            Some(semantic_identity) => MediaOpenSourceRequest::PlaybackWindow {
                source: Box::new(physical_request),
                semantic_identity,
            },
            None => physical_request,
        })
    }

    fn poll_exact_playlist_transport_receipts(&mut self, playlist_runtime: &mut PlaylistRuntime) {
        let had_pending_receipts = !self.playlist_transport.exact_receipts.is_empty();
        let mut retained_receipts =
            Vec::with_capacity(self.playlist_transport.exact_receipts.len());
        for pending in std::mem::take(&mut self.playlist_transport.exact_receipts) {
            let terminal = match pending.receipt.try_take_outcome() {
                Ok(None) => {
                    retained_receipts.push(pending);
                    continue;
                }
                Ok(Some(outcome)) => Ok(outcome),
                Err(error) => Err(error),
            };
            match pending.purpose {
                ExactTransportReceiptPurpose::OrdinaryTransport => match terminal {
                    Ok(ExactMediaTransportOutcome::Applied { .. }) => {}
                    Ok(outcome) => warn!(
                        ?outcome,
                        "Exact playlist transport завершился typed failure/stale outcome"
                    ),
                    Err(error) => {
                        warn!(error = %error, "Exact playlist transport потерял owner outcome");
                    }
                },
                ExactTransportReceiptPurpose::NeutralStop => match terminal {
                    Ok(outcome @ ExactMediaTransportOutcome::Applied { .. }) => {
                        playlist_runtime.apply_neutral_stop_outcome(&outcome);
                    }
                    Ok(outcome) => warn!(
                        ?outcome,
                        "Exact playlist Stop завершился typed failure/stale outcome"
                    ),
                    Err(error) => {
                        warn!(error = %error, "Exact playlist Stop потерял owner outcome");
                    }
                },
                ExactTransportReceiptPurpose::ClearMediaReset {
                    requested_media_instance_id,
                } => {
                    let disposition = playlist_runtime
                        .apply_media_reset_receipt(requested_media_instance_id, terminal);
                    let app_cleanup = (disposition
                        == crate::playlist_runtime::PlaylistMediaResetReceiptDisposition::ClearAppMediaState)
                        .then(|| {
                            self.record_cleared_media_after_exact_reset(
                                requested_media_instance_id,
                            )
                        });
                    if app_cleanup
                        == Some(super::media_jobs::ExactMediaResetCleanup::SupersededByNewSnapshot)
                    {
                        debug!(
                            "App media reset cleanup пропущен: новый player snapshot уже победил"
                        );
                    }
                }
            }
        }
        self.playlist_transport.exact_receipts = retained_receipts;
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

    fn poll_exact_timeline_seek_receipts(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> bool {
        let mut route_relative_beyond_end = false;
        let playlist_binding = self.playlist_runtime_binding();
        self.playlist_transport
            .timeline_seek_receipts
            .retain(|receipt| match receipt.try_take_outcome() {
                Ok(None) => true,
                Ok(Some(ExactTimelineSeekOutcome::Applied {
                    request_id,
                    media_instance_id,
                    position,
                })) => {
                    let request_id = desktop_seek_request_id(request_id);
                    playlist_runtime.publish_desktop_seeked(request_id, position);
                    if let Some(binding) = playlist_binding {
                        playlist_runtime.record_confirmed_resume_seek(
                            binding,
                            media_instance_id,
                            position.as_duration(),
                        );
                    }
                    false
                }
                Ok(Some(ExactTimelineSeekOutcome::BeyondEnd { request_id })) => {
                    playlist_runtime.record_desktop_seek_outcome(
                        desktop_integration::DesktopTimelineSeekOutcome::BeyondEnd {
                            request_id: desktop_seek_request_id(request_id),
                        },
                    );
                    route_relative_beyond_end = true;
                    false
                }
                Ok(Some(outcome)) => {
                    let desktop_outcome = match outcome {
                        ExactTimelineSeekOutcome::InvalidRange { request_id } => {
                            desktop_integration::DesktopTimelineSeekOutcome::InvalidRange {
                                request_id: desktop_seek_request_id(request_id),
                            }
                        }
                        ExactTimelineSeekOutcome::StaleInstance { request_id } => {
                            desktop_integration::DesktopTimelineSeekOutcome::StaleInstance {
                                request_id: desktop_seek_request_id(request_id),
                            }
                        }
                        ExactTimelineSeekOutcome::NotSeekable { request_id } => {
                            desktop_integration::DesktopTimelineSeekOutcome::NotSeekable {
                                request_id: desktop_seek_request_id(request_id),
                            }
                        }
                        ExactTimelineSeekOutcome::Failed { request_id, .. } => {
                            desktop_integration::DesktopTimelineSeekOutcome::Failed {
                                request_id: desktop_seek_request_id(request_id),
                            }
                        }
                        ExactTimelineSeekOutcome::Applied { .. }
                        | ExactTimelineSeekOutcome::BeyondEnd { .. } => {
                            unreachable!("Applied and BeyondEnd are handled above")
                        }
                    };
                    playlist_runtime.record_desktop_seek_outcome(desktop_outcome);
                    debug!(
                        ?desktop_outcome,
                        "Exact timeline seek завершился без Seeked signal"
                    );
                    false
                }
                Err(error) => {
                    warn!(error = %error, "Exact timeline seek потерял owner outcome");
                    false
                }
            });
        route_relative_beyond_end
    }
}

fn desktop_seek_request_id(
    request_id: player_core::TimelineSeekRequestId,
) -> desktop_integration::TimelineSeekRequestId {
    std::num::NonZeroU64::new(request_id.get())
        .map(desktop_integration::TimelineSeekRequestId::new)
        .expect("player timeline request IDs are non-zero")
}
