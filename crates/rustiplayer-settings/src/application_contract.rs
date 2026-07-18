//! Проверяемый контракт live-apply для каждого редактируемого setting-а.
//!
//! Модуль описывает только намерение и ownership. Конкретные worker, media,
//! decoder и renderer операции остаются у соответствующих runtime owners.

use settings_core::{RouteGeneration, SettingId};

use crate::AppRuntimeRoute;

/// Владелец активного runtime state для одного setting-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingStateOwner {
    /// UI shell: chrome, telemetry, язык, skin и animation policy.
    UiShell,
    /// Settings runtime: pacing и lifecycle самой settings transaction.
    SettingsRuntime,
    /// Renderer live parameters, применяемые без пересоздания GPU resources.
    RendererLiveParameters,
    /// Renderer/surface lifecycle, включая backend-specific resources.
    RendererLifecycle,
    /// Policy открытия media и выбора stream-а.
    MediaOpenPolicy,
    /// Player session policy и seek/scheduler state.
    PlayerSession,
    /// Lifecycle video decoder/pipeline на app/player boundary.
    VideoPipelineLifecycle,
    /// Lifecycle активного audio output.
    AudioOutputLifecycle,
    /// Lifecycle активного media source/demuxer/prefetch path.
    MediaSourceLifecycle,
    /// Player-owned Frame Server policy.
    FrameServerPolicy,

    /// Process-lifetime playlist policy owner.
    PlaylistPolicy,
}

/// Intent-механизм, которым setting должен попасть в активный runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingApplyMechanism {
    /// Простое owner state обновляется на месте.
    StateUpdateInPlace,
    /// Активная policy обновляется сейчас, а используется следующим естественным событием.
    PolicyUpdateInPlace,
    /// Owner worker атомарно принимает новую runtime configuration.
    WorkerReconfigure,
    /// Renderer live parameters обновляются без GPU lifecycle operation.
    RendererLiveUpdate,
    /// Активный audio output контролируемо пересоздаётся.
    AudioOutputRecreate,
    /// Активный media source/demuxer контролируемо перестраивается.
    MediaSourceRebuild,
    /// Video decoder/pipeline перестраивается с сохранением playback intent.
    VideoPipelineRebuild,
    /// Renderer/surface resources контролируемо пересоздаются.
    RendererRecreate,
    /// Уже активный preview становится committed runtime state.
    PreviewPromotion,
}

/// Focused scenario, который должен закреплять конкретный application contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingApplyTestScenario {
    /// Apply без активного runtime resource.
    NoActiveResource,
    /// Apply при активном runtime resource.
    ActiveResource,
    /// Apply при paused active media.
    ActiveMediaPaused,
    /// Apply при playing active media.
    ActiveMediaPlaying,
    /// Backpressure сохраняет различимую typed семантику.
    Backpressure,
    /// Повторный apply того же значения возвращает `Noop`.
    RepeatedApplyNoop,
    /// Busy/conflict обнаруживается до мутации owner-а.
    RuntimeBusyWithoutMutation,
    /// Ошибка apply сохраняет последнюю рабочую runtime configuration.
    ApplyFailurePreservesRuntime,
    /// Rollback восстанавливает предыдущую runtime configuration.
    RollbackRestoresRuntime,
    /// Ошибка rollback остаётся отдельной от исходной apply error.
    RollbackFailureIsDistinct,
    /// Route не меняет state соседнего owner-а.
    NoUnrelatedOwnerMutation,
    /// Event-scoped policy проявляется только на следующем естественном событии.
    EffectOnNextNaturalEvent,
    /// Renderer recreation учитывает активную present-frame lease.
    ActivePresentFrameLease,
    /// Device-lost остаётся отдельным fatal lifecycle исходом.
    DeviceLost,
}

/// Одна строка checked application matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingApplicationContract {
    /// Stable id, совпадающий с generated config descriptor.
    pub setting_id: SettingId,
    /// Project-level route, который доставляет intent владельцу.
    pub route: AppRuntimeRoute,
    /// Единственный владелец активного state.
    pub state_owner: SettingStateOwner,
    /// Требуемый live-apply механизм.
    pub mechanism: SettingApplyMechanism,
    /// Владелец snapshot-а и compensating rollback.
    pub rollback_owner: SettingStateOwner,
    /// Обязательные focused scenarios для реализации owner boundary.
    pub focused_tests: &'static [SettingApplyTestScenario],
}

impl SettingApplicationContract {
    /// Собирает строку матрицы без неочевидных positional значений в callsite.
    fn new(
        setting_id: &str,
        route: AppRuntimeRoute,
        state_owner: SettingStateOwner,
        mechanism: SettingApplyMechanism,
        focused_tests: &'static [SettingApplyTestScenario],
    ) -> Self {
        Self {
            setting_id: SettingId::from(setting_id),
            route,
            state_owner,
            mechanism,
            rollback_owner: state_owner,
            focused_tests,
        }
    }
}

/// Non-interruptible operation, которая может временно закрыть apply boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsBoundaryActivity {
    /// Уже выполняется другая settings transaction.
    SettingsTransaction,
    /// Активная seek transaction владеет player/pipeline boundary.
    Seek,
    /// Активный scrub владеет player/pipeline boundary.
    Scrub,
    /// Идёт non-interruptible pipeline lifecycle operation.
    PipelineLifecycle,
    /// Идёт non-interruptible renderer/surface lifecycle operation.
    RendererLifecycle,
}

/// Typed stage исходной renderer recreation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererRecreationApplyErrorKind {
    /// Выбранный renderer profile не реализован текущим runtime-ом.
    UnsupportedProfile,
    /// Candidate device/surface/renderer создать не удалось.
    CandidateCreation,
    /// Candidate не смог восстановить live render state или materializer compatibility.
    CandidatePreparation,
    /// Старую GPU work не удалось безопасно завершить.
    GpuDrain,
    /// WGPU callback доказал device lost.
    DeviceLost,
    /// Финальный owner commit candidate-а не завершился.
    Commit,
}

/// Typed apply error controlled renderer recreation-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererRecreationApplyError {
    /// Lifecycle stage, на котором остановилась транзакция.
    pub kind: RendererRecreationApplyErrorKind,

    /// Диагностический контекст без потери typed kind-а.
    pub message: String,
}

/// Typed cause доказанного restore failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererRecreationRollbackErrorKind {
    /// Старый device потерян, а fresh renderer старой конфигурации создать не удалось.
    DeviceLost,
    /// Surface старой конфигурации доказанно невозможно восстановить.
    SurfaceInvalidated,
    /// Renderer/materializer старой конфигурации не удалось восстановить.
    ResourceRestore,
}

/// Отдельная rollback error controlled renderer recreation-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererRecreationRollbackError {
    /// Конкретный класс restore failure.
    pub kind: RendererRecreationRollbackErrorKind,

    /// Диагностический контекст restore attempt-а.
    pub message: String,
}

/// Typed renderer failure сохраняет исходную apply error рядом с rollback error.
pub type RendererRecreationFailure =
    SettingsApplyFailure<RendererRecreationApplyError, RendererRecreationRollbackError>;

/// Typed failure, не теряющий исходную apply error при rollback failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsApplyFailure<ApplyError, RollbackError> {
    /// Owner отклонил apply до успешного runtime commit-а.
    ApplyFailed {
        /// Owner, вернувший typed error.
        owner: SettingStateOwner,
        /// Конкретная ошибка owner boundary.
        error: ApplyError,
    },
    /// Apply не удался, а compensating rollback тоже завершился ошибкой.
    ApplyAndRollbackFailed {
        /// Owner исходной apply operation.
        apply_owner: SettingStateOwner,
        /// Исходная typed apply error.
        apply_error: ApplyError,
        /// Owner compensating rollback.
        rollback_owner: SettingStateOwner,
        /// Отдельная typed rollback error.
        rollback_error: RollbackError,
    },
}

/// Итог одной попытки apply без bool/string-схлопывания семантики.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsApplyOutcome<ApplyError, RollbackError> {
    /// Runtime полностью принял setting указанным механизмом.
    Applied {
        /// Реально выполненный live-apply механизм.
        mechanism: SettingApplyMechanism,
    },
    /// Runtime уже находился в требуемом состоянии.
    Noop,
    /// Boundary временно занят; запрос не поставлен в скрытую очередь.
    RuntimeBusy {
        /// Owner занятого boundary.
        owner: SettingStateOwner,
        /// Активная non-interruptible operation.
        activity: SettingsBoundaryActivity,
    },
    /// Generation conflict обнаружен до первой owner mutation.
    Conflict {
        /// Owner конфликтующего route.
        owner: SettingStateOwner,
        /// Generation, захваченная draft transaction.
        baseline: RouteGeneration,
        /// Текущая generation owner-а.
        current: RouteGeneration,
    },
    /// Неретрайная apply/rollback failure.
    Failed(SettingsApplyFailure<ApplyError, RollbackError>),
}

const IN_PLACE_TESTS: &[SettingApplyTestScenario] = &[
    SettingApplyTestScenario::ActiveResource,
    SettingApplyTestScenario::RepeatedApplyNoop,
    SettingApplyTestScenario::ApplyFailurePreservesRuntime,
    SettingApplyTestScenario::RollbackRestoresRuntime,
    SettingApplyTestScenario::NoUnrelatedOwnerMutation,
];

const POLICY_TESTS: &[SettingApplyTestScenario] = &[
    SettingApplyTestScenario::ActiveResource,
    SettingApplyTestScenario::RepeatedApplyNoop,
    SettingApplyTestScenario::RuntimeBusyWithoutMutation,
    SettingApplyTestScenario::RollbackRestoresRuntime,
    SettingApplyTestScenario::NoUnrelatedOwnerMutation,
    SettingApplyTestScenario::EffectOnNextNaturalEvent,
];

const WORKER_TESTS: &[SettingApplyTestScenario] = &[
    SettingApplyTestScenario::NoActiveResource,
    SettingApplyTestScenario::ActiveMediaPaused,
    SettingApplyTestScenario::ActiveMediaPlaying,
    SettingApplyTestScenario::RuntimeBusyWithoutMutation,
    SettingApplyTestScenario::ApplyFailurePreservesRuntime,
    SettingApplyTestScenario::RollbackRestoresRuntime,
    SettingApplyTestScenario::NoUnrelatedOwnerMutation,
];

const PIPELINE_TESTS: &[SettingApplyTestScenario] = &[
    SettingApplyTestScenario::NoActiveResource,
    SettingApplyTestScenario::ActiveMediaPaused,
    SettingApplyTestScenario::ActiveMediaPlaying,
    SettingApplyTestScenario::Backpressure,
    SettingApplyTestScenario::RuntimeBusyWithoutMutation,
    SettingApplyTestScenario::ApplyFailurePreservesRuntime,
    SettingApplyTestScenario::RollbackRestoresRuntime,
    SettingApplyTestScenario::RollbackFailureIsDistinct,
    SettingApplyTestScenario::NoUnrelatedOwnerMutation,
];

const RENDER_LIVE_TESTS: &[SettingApplyTestScenario] = &[
    SettingApplyTestScenario::NoActiveResource,
    SettingApplyTestScenario::ActiveResource,
    SettingApplyTestScenario::RepeatedApplyNoop,
    SettingApplyTestScenario::ApplyFailurePreservesRuntime,
    SettingApplyTestScenario::RollbackRestoresRuntime,
    SettingApplyTestScenario::NoUnrelatedOwnerMutation,
];

const RENDER_RECREATE_TESTS: &[SettingApplyTestScenario] = &[
    SettingApplyTestScenario::NoActiveResource,
    SettingApplyTestScenario::ActiveMediaPaused,
    SettingApplyTestScenario::ActiveMediaPlaying,
    SettingApplyTestScenario::RuntimeBusyWithoutMutation,
    SettingApplyTestScenario::ApplyFailurePreservesRuntime,
    SettingApplyTestScenario::RollbackRestoresRuntime,
    SettingApplyTestScenario::RollbackFailureIsDistinct,
    SettingApplyTestScenario::ActivePresentFrameLease,
    SettingApplyTestScenario::DeviceLost,
];

/// Возвращает единственную строку application matrix для stable setting id.
///
/// Match намеренно перечисляет ids явно: новый descriptor не должен молча
/// унаследовать общий prefix-route и обязан сломать coverage test.
#[must_use]
pub fn setting_application_contract(setting_id: &SettingId) -> Option<SettingApplicationContract> {
    let setting_name = setting_id.as_str();
    let contract = match setting_name {
        "ui.show_telemetry"
        | "ui.language"
        | "ui.skin"
        | "ui.window.titlebar_height_px"
        | "ui.sidebar.width_points"
        | "ui.animations.reduced_motion"
        | "ui.animations.sidebar_slide_duration_ms" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Ui,
            SettingStateOwner::UiShell,
            SettingApplyMechanism::StateUpdateInPlace,
            IN_PLACE_TESTS,
        ),
        "ui.settings.live_preview_max_hz" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Ui,
            SettingStateOwner::SettingsRuntime,
            SettingApplyMechanism::StateUpdateInPlace,
            IN_PLACE_TESTS,
        ),
        "render.hdr_to_sdr.enabled"
        | "render.hdr_to_sdr.operator"
        | "render.hdr_to_sdr.sdr_reference_white_nits"
        | "render.hdr_to_sdr.hdr_reference_peak_nits"
        | "render.color_adjustment.brightness"
        | "render.color_adjustment.contrast"
        | "render.color_adjustment.saturation"
        | "render.color_adjustment.exposure"
        | "render.color_adjustment.rgb_gain"
        | "render.color_adjustment.rgb_offset" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::RenderPreview,
            SettingStateOwner::RendererLiveParameters,
            SettingApplyMechanism::RendererLiveUpdate,
            RENDER_LIVE_TESTS,
        ),
        "render.profile"
        | "render.tone_mapping"
        | "render.vulkan.present_mode"
        | "render.vulkan.max_frame_latency"
        | "render.opengles.enabled"
        | "render.opengles.simple_ui" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::RenderCommitted,
            SettingStateOwner::RendererLifecycle,
            SettingApplyMechanism::RendererRecreate,
            RENDER_RECREATE_TESTS,
        ),
        "player.start_paused" | "player.resume_last_position" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Player,
            SettingStateOwner::MediaOpenPolicy,
            SettingApplyMechanism::PolicyUpdateInPlace,
            POLICY_TESTS,
        ),
        "player.seek.paused_commit_behavior"
        | "player.seek.hotkey_small_step_secs"
        | "player.seek.hotkey_large_step_secs"
        | "audio.volume" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Player,
            SettingStateOwner::PlayerSession,
            SettingApplyMechanism::PolicyUpdateInPlace,
            POLICY_TESTS,
        ),
        "player.seek.commit_timeout_ms"
        | "player.seek.resume_audio_min_buffer_ms"
        | "player.seek.resume_audio_gate_timeout_ms"
        | "player.seek.resume_video_min_ready_frames"
        | "player.seek.fast_preroll_budget_ms"
        | "player.seek.fast_preroll_video_packet_burst"
        | "audio.buffer_target_ms"
        | "video.max_decode_ahead_ms"
        | "video.present_queue_frames"
        | "video.scheduler.demux_packets_per_tick"
        | "video.scheduler.video_packets_per_tick"
        | "video.scheduler.decoded_frames_per_tick"
        | "video.scheduler.catch_up_budget_ms"
        | "video.scheduler.present_queue_min_frames"
        | "video.scheduler.present_queue_target_frames"
        | "video.scheduler.decode_ahead_target_ms"
        | "video.scheduler.surface_free_slots_min"
        | "video.scheduler.surface_free_slots_target" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Player,
            SettingStateOwner::PlayerSession,
            SettingApplyMechanism::WorkerReconfigure,
            WORKER_TESTS,
        ),
        "player.preferred_video_codec_order"
        | "video.preferred_backend"
        | "video.decoder_packet_channel_frames"
        | "video.decoder_frame_channel_frames"
        | "video.decoder_ready_queue_frames"
        | "video.decoder_surface_pool_frames"
        | "video.sw_decoder_surface_pool_frames"
        | "video.sw_decode_threads"
        | "video.zero_copy_surface_pool_slots" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Player,
            SettingStateOwner::VideoPipelineLifecycle,
            SettingApplyMechanism::VideoPipelineRebuild,
            PIPELINE_TESTS,
        ),
        "player.demux.max_consecutive_corrupted_packets" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Player,
            SettingStateOwner::MediaSourceLifecycle,
            SettingApplyMechanism::MediaSourceRebuild,
            PIPELINE_TESTS,
        ),
        "audio.output_device" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Player,
            SettingStateOwner::AudioOutputLifecycle,
            SettingApplyMechanism::AudioOutputRecreate,
            PIPELINE_TESTS,
        ),
        "network.memory_cache_mb"
        | "network.read_ahead_mb"
        | "network.prefetch_initial_chunk_kb"
        | "network.prefetch_chunk_mb"
        | "network.connect_timeout_ms"
        | "network.read_timeout_ms" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::MediaService,
            SettingStateOwner::MediaSourceLifecycle,
            SettingApplyMechanism::MediaSourceRebuild,
            PIPELINE_TESTS,
        ),
        "yt_dlp.enabled" | "yt_dlp.hdr_selection" | "yt_dlp.resolve_timeout_ms" => {
            SettingApplicationContract::new(
                setting_name,
                AppRuntimeRoute::MediaService,
                SettingStateOwner::MediaOpenPolicy,
                SettingApplyMechanism::PolicyUpdateInPlace,
                POLICY_TESTS,
            )
        }
        "frame_server.live_scrub_enabled"
        | "frame_server.live_scrub_decode_mode"
        | "frame_server.live_scrub_max_hz" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::FrameServer,
            SettingStateOwner::FrameServerPolicy,
            SettingApplyMechanism::WorkerReconfigure,
            WORKER_TESTS,
        ),
        "playlist.load_siblings"
        | "playlist.sibling_media_filter"
        | "playlist.playback_behavior"
        | "playlist.error_behavior"
        | "playlist.previous_restart_threshold_ms" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Playlist,
            SettingStateOwner::PlaylistPolicy,
            SettingApplyMechanism::PolicyUpdateInPlace,
            POLICY_TESTS,
        ),
        "playlist.state_save_debounce_ms" => SettingApplicationContract::new(
            setting_name,
            AppRuntimeRoute::Playlist,
            SettingStateOwner::PlaylistPolicy,
            SettingApplyMechanism::WorkerReconfigure,
            WORKER_TESTS,
        ),
        _ => return None,
    };

    Some(contract)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use settings_core::SettingAccess;

    use super::*;
    use crate::{app_config_registry, runtime_route_from_descriptor};

    #[test]
    fn every_editable_setting_has_one_checked_live_application_contract() {
        let registry = app_config_registry().expect("AppConfig registry должен собираться");
        let editable_descriptors = registry
            .descriptors()
            .filter(|descriptor| descriptor.access == SettingAccess::ReadWrite)
            .collect::<Vec<_>>();
        let mut covered_setting_ids = BTreeSet::new();

        for descriptor in &editable_descriptors {
            let contract = setting_application_contract(&descriptor.id).unwrap_or_else(|| {
                panic!(
                    "editable setting `{}` не имеет live application contract",
                    descriptor.id.as_str()
                )
            });
            let metadata_route = runtime_route_from_descriptor(descriptor)
                .expect("editable descriptor должен иметь project runtime route");

            assert_eq!(contract.setting_id, descriptor.id);
            assert_eq!(contract.route, metadata_route);
            assert!(!contract.focused_tests.is_empty());
            assert!(covered_setting_ids.insert(contract.setting_id.clone()));
        }

        assert_eq!(covered_setting_ids.len(), editable_descriptors.len());
        assert!(setting_application_contract(&SettingId::from("schema_version")).is_none());
    }

    #[test]
    fn typed_outcome_keeps_retryable_and_rollback_failures_distinct() {
        let busy = SettingsApplyOutcome::<&str, &str>::RuntimeBusy {
            owner: SettingStateOwner::VideoPipelineLifecycle,
            activity: SettingsBoundaryActivity::Scrub,
        };
        let conflict = SettingsApplyOutcome::<&str, &str>::Conflict {
            owner: SettingStateOwner::PlayerSession,
            baseline: RouteGeneration::new(3),
            current: RouteGeneration::new(4),
        };
        let rollback_failure =
            SettingsApplyOutcome::Failed(SettingsApplyFailure::ApplyAndRollbackFailed {
                apply_owner: SettingStateOwner::RendererLifecycle,
                apply_error: "renderer create failed",
                rollback_owner: SettingStateOwner::RendererLifecycle,
                rollback_error: "old renderer restore failed",
            });

        assert!(matches!(busy, SettingsApplyOutcome::RuntimeBusy { .. }));
        assert!(matches!(conflict, SettingsApplyOutcome::Conflict { .. }));
        assert!(matches!(
            rollback_failure,
            SettingsApplyOutcome::Failed(SettingsApplyFailure::ApplyAndRollbackFailed { .. })
        ));
    }

    #[test]
    fn playlist_descriptors_use_dedicated_owner_and_explicit_mechanisms() {
        for setting_id in [
            "playlist.load_siblings",
            "playlist.sibling_media_filter",
            "playlist.playback_behavior",
            "playlist.error_behavior",
            "playlist.previous_restart_threshold_ms",
        ] {
            let contract = setting_application_contract(&SettingId::from(setting_id))
                .expect("playlist policy contract exists");
            assert_eq!(contract.route, AppRuntimeRoute::Playlist);
            assert_eq!(contract.state_owner, SettingStateOwner::PlaylistPolicy);
            assert_eq!(
                contract.mechanism,
                SettingApplyMechanism::PolicyUpdateInPlace
            );
        }

        let debounce =
            setting_application_contract(&SettingId::from("playlist.state_save_debounce_ms"))
                .expect("playlist debounce contract exists");
        assert_eq!(debounce.route, AppRuntimeRoute::Playlist);
        assert_eq!(debounce.state_owner, SettingStateOwner::PlaylistPolicy);
        assert_eq!(debounce.mechanism, SettingApplyMechanism::WorkerReconfigure);
    }
}
