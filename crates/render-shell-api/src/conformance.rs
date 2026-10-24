//! Conformance-набор: одинаковый экзамен контракта для каждого рендера.
//!
//! Подключается feature `conformance` из dev-dependencies конкретного рендера. Рендер
//! предоставляет [`RenderShellConformanceHarness`] (как создать рендер, какой кадр он не умеет,
//! какой декодер с готовым кадром дать), а проверки здесь одинаковы для wgpu, GLES2 и DX.
//!
//! Каждая проверка создаёт свежий рендер и возвращает `Err(ConformanceViolation)` с
//! объяснением, а не паникует: тест рендера утверждает `Ok(())` и печатает причину.
//!
//! Что закреплено:
//! 1. чужой билет буфера не рисуется ([`check_foreign_surface_frame_is_rejected`]);
//! 2. `ReusePrevious` без удержанного интерфейса — `RetainedUiUnavailable`, а после свежего
//!    кадра того же размера — разрешён ([`check_reuse_previous_requires_retained_ui`]);
//! 3. неподдерживаемый контракт — `Unsupported`, а не `Error`
//!    ([`check_unsupported_contract_is_not_error`]);
//! 4. «декодер не подключён» отличим от остальных исходов
//!    ([`check_missing_video_backend_is_distinct`]);
//! 5. кадр декодера освобождается ровно один раз и не раньше, чем приложение отпустило
//!    аренду и ключ ([`check_frame_released_exactly_once`]);
//! 6. потеря устройства освобождает ждущие кадры ровно один раз
//!    ([`check_device_lost_releases_exactly_once`]);
//! 7. подготовщик импорта работает в другом потоке, не трогает активный импорт, а его импорт
//!    ставится на выдавший рендер и даёт готовый кадр
//!    ([`check_preparer_prepares_on_another_thread`]);
//! 8. (02a) отпущенное окно не даёт буферов, повторы detach/reattach — явные no-op, после
//!    возврата окна кадр показывается ([`check_detached_presentation_gives_no_frames`]);
//! 9. (02a) перенастройка показа на месте различает «применено», «уже так» и отказ без
//!    изменений ([`check_presentation_reconfigure_distinguishes_no_op`]);
//! 10. (02a) частичная правка неизвестной текстуры интерфейса — `UiTexturesOutOfSync`, а не
//!     паника; после целой загрузки правки проходят
//!     ([`check_partial_edit_of_unknown_ui_texture_is_reported`]);
//! 11. (02a) изменения текстур интерфейса (`set` и `free`) применяются и в отклонённом кадре
//!     ([`check_ui_textures_apply_on_rejected_frames`]).

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use video_backend_api::StartedVideoBackend;
use video_core::{DecodedFrame, FrameResourceHandle};
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};

mod presentation;
mod ui_textures;

pub use presentation::{
    check_detached_presentation_gives_no_frames, check_presentation_reconfigure_distinguishes_no_op,
};
pub use ui_textures::{
    check_partial_edit_of_unknown_ui_texture_is_reported,
    check_ui_textures_apply_on_rejected_frames,
};

use crate::{
    AcquiredSurfaceFrame, EguiFrameContent, FrameImportUnsupportedReason,
    FrameMaterializationOutcome, RenderFrameDropReason, RenderFrameFailureKind, RenderFrameInput,
    RenderFrameOutcome, RenderScreenDescriptor, RenderShell, RendererCreateError,
    RendererGpuDrainError, SurfaceFrameAcquire, SurfacePresentSettings, VideoImportPlan,
    WindowCornerMask,
};

/// Сколько проверка ждёт завершения работы GPU (как bounded drain пересоздания рендера).
pub const CONFORMANCE_GPU_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Одна проверка контракта над стендом рендера.
type ConformanceCheckFn =
    fn(&mut dyn RenderShellConformanceHarness) -> Result<(), ConformanceViolation>;

/// Поколение рендера в арендах проверок; значение не важно, важно одно на всю проверку.
const CONFORMANCE_RENDER_GENERATION: u64 = 1;

/// Что рендер предоставляет проверкам.
pub trait RenderShellConformanceHarness {
    /// Создаёт свежий рендер: без удержанного интерфейса и без подключённого декодера.
    fn create_renderer(&mut self) -> Result<Box<dyn RenderShell>, RendererCreateError>;

    /// Экран, под который рендер получает буфер (размер его surface и масштаб интерфейса).
    fn screen(&self) -> RenderScreenDescriptor;

    /// Настройки показа, с которыми стенд создаёт рендер (02a: от них отталкивается проверка
    /// перенастройки на месте).
    fn initial_presentation(&self) -> SurfacePresentSettings;

    /// Кадр, контракт которого рендер заведомо не объявляет в `RenderCapabilities`.
    fn unsupported_frame(&mut self) -> DecodedFrame;

    /// Запущенный декодер с одним готовым кадром поддерживаемого контракта.
    ///
    /// Внутренний provider декодера обязан записывать каждое освобождение в `ledger`.
    fn supported_video_source(&mut self) -> ConformanceVideoSource;
}

/// Декодер для проверок освобождения кадра.
pub struct ConformanceVideoSource {
    /// Запущенный декодер; рендер обернёт его в `prepare_video_import`.
    pub backend: StartedVideoBackend,

    /// Как импортировать его кадры.
    pub plan: VideoImportPlan,

    /// Кадр, ресурс которого уже готов в provider-е декодера.
    pub frame: DecodedFrame,

    /// Журнал освобождений внутреннего provider-а декодера.
    pub ledger: ConformanceReleaseLedger,
}

/// Журнал освобождений кадров, которые дошли до внутреннего provider-а декодера.
#[derive(Debug, Clone, Default)]
pub struct ConformanceReleaseLedger {
    /// Ресурсы в порядке освобождения (повтор одного ресурса = двойное освобождение).
    released: Arc<Mutex<Vec<FrameResourceHandle>>>,
}

impl ConformanceReleaseLedger {
    /// Записывает освобождение ресурса (вызывает fake/тестовый provider декодера).
    pub fn record_release(&self, handle: FrameResourceHandle) {
        // Журнал — только список; отравление после паники другого потока не портит его смысл.
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle);
    }

    /// Сколько раз освобождён ресурс.
    #[must_use]
    pub fn release_count(&self, handle: FrameResourceHandle) -> usize {
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|released| **released == handle)
            .count()
    }
}

/// Какая проверка не прошла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceCheck {
    /// Создание рендера или стенда (не нарушение контракта, но проверка не выполнена).
    HarnessSetup,
    /// Чужой билет буфера не рисуется.
    ForeignSurfaceFrameRejected,
    /// `ReusePrevious` требует удержанного интерфейса.
    ReusePreviousRequiresRetainedUi,
    /// Неподдерживаемый контракт — `Unsupported`, а не `Error`.
    UnsupportedContractIsNotError,
    /// «Декодер не подключён» отличим от остальных исходов.
    MissingVideoBackendIsDistinct,
    /// Кадр освобождается ровно один раз.
    FrameReleasedExactlyOnce,
    /// Потеря устройства освобождает ждущие кадры ровно один раз.
    DeviceLostReleasesExactlyOnce,
    /// Подготовщик импорта работает в другом потоке и готовит импорт для своего рендера.
    PreparerPreparesOnAnotherThread,
    /// Отпущенное окно не даёт буферов; после возврата показ продолжается.
    DetachedPresentationGivesNoFrames,
    /// Перенастройка показа на месте отличает «применено» от «уже так» и от отказа.
    PresentationReconfigureDistinguishesNoOp,
    /// Частичная правка неизвестной текстуры интерфейса — типизированный отказ, а не паника.
    PartialEditOfUnknownUiTextureIsReported,
    /// Изменения текстур интерфейса применяются и в отклонённом кадре.
    UiTexturesApplyOnRejectedFrames,
}

/// Нарушение контракта с объяснением.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceViolation {
    /// Проверка.
    pub check: ConformanceCheck,

    /// Что ожидалось и что получилось.
    pub detail: String,
}

impl ConformanceViolation {
    fn new(check: ConformanceCheck, detail: impl Into<String>) -> Self {
        Self {
            check,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ConformanceViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.check, self.detail)
    }
}

impl std::error::Error for ConformanceViolation {}

/// Прогоняет все проверки и возвращает все нарушения (пустой список — контракт соблюдён).
pub fn run_render_shell_conformance(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Vec<ConformanceViolation> {
    let checks: [ConformanceCheckFn; 11] = [
        check_foreign_surface_frame_is_rejected,
        check_reuse_previous_requires_retained_ui,
        check_unsupported_contract_is_not_error,
        check_missing_video_backend_is_distinct,
        check_frame_released_exactly_once,
        check_device_lost_releases_exactly_once,
        check_preparer_prepares_on_another_thread,
        check_detached_presentation_gives_no_frames,
        check_presentation_reconfigure_distinguishes_no_op,
        check_partial_edit_of_unknown_ui_texture_is_reported,
        check_ui_textures_apply_on_rejected_frames,
    ];
    checks
        .into_iter()
        .filter_map(|check| check(harness).err())
        .collect()
}

/// Проверка 1: билет, выданный не этим рендером, даёт `Failed(ForeignSurfaceFrame)`.
pub fn check_foreign_surface_frame_is_rejected(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::ForeignSurfaceFrameRejected;
    let mut renderer = create_renderer(harness)?;
    let outcome = renderer.render_frame(frame_input(
        AcquiredSurfaceFrame::unowned_for_conformance(),
        harness.screen(),
        None,
        EguiFrameContent::empty_fresh(),
    ));
    match outcome {
        RenderFrameOutcome::Failed(failure)
            if failure.kind == RenderFrameFailureKind::ForeignSurfaceFrame =>
        {
            Ok(())
        }
        other => Err(ConformanceViolation::new(
            check,
            format!("foreign surface ticket must fail as ForeignSurfaceFrame, got {other:?}"),
        )),
    }
}

/// Проверка 2: `ReusePrevious` на свежем рендере отклоняется, после свежего кадра — показан.
pub fn check_reuse_previous_requires_retained_ui(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::ReusePreviousRequiresRetainedUi;
    let screen = harness.screen();
    let mut renderer = create_renderer(harness)?;

    if renderer.can_reuse_retained_ui(screen) {
        return Err(ConformanceViolation::new(
            check,
            "fresh renderer must not report retained UI",
        ));
    }
    let refused = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        EguiFrameContent::ReusePrevious,
    )?;
    if refused != RenderFrameOutcome::Dropped(RenderFrameDropReason::RetainedUiUnavailable) {
        return Err(ConformanceViolation::new(
            check,
            format!(
                "ReusePrevious without retained UI must drop as RetainedUiUnavailable, got {refused:?}"
            ),
        ));
    }

    let fresh = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        EguiFrameContent::empty_fresh(),
    )?;
    expect_presented(check, "fresh frame", &fresh)?;
    if !renderer.can_reuse_retained_ui(screen) {
        return Err(ConformanceViolation::new(
            check,
            "renderer must retain UI after a presented fresh frame of the same screen",
        ));
    }
    let reused = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        EguiFrameContent::ReusePrevious,
    )?;
    expect_presented(check, "ReusePrevious after a fresh frame", &reused)
}

/// Проверка 3: контракт, которого рендер не объявляет, — `Unsupported(ContractNotDeclared)`.
///
/// Декодер не подключён намеренно: контракт фиксирует порядок «сначала умеет ли рендер
/// контракт вообще, потом подключён ли декодер».
pub fn check_unsupported_contract_is_not_error(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::UnsupportedContractIsNotError;
    let mut renderer = create_renderer(harness)?;
    let frame = harness.unsupported_frame();
    let frame_contract = frame.frame_contract;
    if renderer
        .capabilities()
        .supports_frame_contract(frame_contract)
    {
        return Err(ConformanceViolation::new(
            ConformanceCheck::HarnessSetup,
            format!("harness 'unsupported' frame {frame_contract:?} is declared by the renderer"),
        ));
    }
    let lease = lease_without_provider(frame);
    let materialization = renderer.video_frames().materialize(&lease);
    match materialization.outcome {
        FrameMaterializationOutcome::Unsupported(
            FrameImportUnsupportedReason::ContractNotDeclared {
                frame_contract: reported,
            },
        ) if reported == frame_contract => Ok(()),
        other => Err(ConformanceViolation::new(
            check,
            format!("undeclared contract must be Unsupported(ContractNotDeclared), got {other:?}"),
        )),
    }
}

/// Проверка 4: поддерживаемый кадр без подключённого декодера — `NoVideoBackendBound`.
pub fn check_missing_video_backend_is_distinct(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::MissingVideoBackendIsDistinct;
    let mut renderer = create_renderer(harness)?;
    let source = harness.supported_video_source();
    if renderer.video_frames().active_video_import_path().is_some() {
        return Err(ConformanceViolation::new(
            check,
            "fresh renderer must not have an active video import",
        ));
    }
    let lease = lease_without_provider(source.frame);
    let materialization = renderer.video_frames().materialize(&lease);
    match materialization.outcome {
        FrameMaterializationOutcome::NoVideoBackendBound => Ok(()),
        other => Err(ConformanceViolation::new(
            check,
            format!(
                "supported frame without a bound decoder must be NoVideoBackendBound, got {other:?}"
            ),
        )),
    }
}

/// Проверка 5: кадр, показанный рендером, освобождается ровно один раз и только после того,
/// как приложение отпустило аренду и ключ и GPU доработал.
pub fn check_frame_released_exactly_once(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::FrameReleasedExactlyOnce;
    let screen = harness.screen();
    let mut renderer = create_renderer(harness)?;
    let presented = present_supported_frame(renderer.as_mut(), harness, check, screen)?;
    let handle = presented.handle;
    let ledger = presented.ledger.clone();

    // Ключ и аренда ещё у приложения: кадр нельзя вернуть декодеру даже после работы GPU.
    wait_for_gpu_idle(renderer.as_mut(), check)?;
    expect_release_count(
        check,
        "while the app still holds the lease",
        &ledger,
        handle,
        0,
    )?;

    drop(presented);
    wait_for_gpu_idle(renderer.as_mut(), check)?;
    expect_release_count(check, "after the app dropped the lease", &ledger, handle, 1)?;

    // Ещё один кадр и ожидание не должны освобождать тот же ресурс повторно.
    let empty = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        EguiFrameContent::empty_fresh(),
    )?;
    expect_presented(check, "frame after release", &empty)?;
    wait_for_gpu_idle(renderer.as_mut(), check)?;
    expect_release_count(check, "after another frame", &ledger, handle, 1)
}

/// Проверка 6: потеря устройства освобождает ждущий кадр один раз; поздние сигналы GPU
/// не освобождают его повторно.
pub fn check_device_lost_releases_exactly_once(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::DeviceLostReleasesExactlyOnce;
    let screen = harness.screen();
    let mut renderer = create_renderer(harness)?;
    let presented = present_supported_frame(renderer.as_mut(), harness, check, screen)?;
    let handle = presented.handle;
    let ledger = presented.ledger.clone();
    drop(presented);

    renderer
        .video_frames()
        .release_after_device_lost()
        .map_err(|error| {
            ConformanceViolation::new(
                check,
                format!("release after device lost failed: {error:?}"),
            )
        })?;
    expect_release_count(check, "after release_after_device_lost", &ledger, handle, 1)?;

    // Поздние сигналы GPU (настоящее устройство на стенде не потеряно) — без второго release.
    match renderer.wait_for_gpu_idle(CONFORMANCE_GPU_IDLE_TIMEOUT) {
        Ok(()) | Err(RendererGpuDrainError::DeviceLost(_)) => {}
        Err(other) => {
            return Err(ConformanceViolation::new(
                check,
                format!("GPU idle wait after device-lost release failed: {other}"),
            ));
        }
    }
    expect_release_count(check, "after late GPU completion", &ledger, handle, 1)
}

/// Проверка 7: подготовщик импорта работает в другом потоке (как кандидат нового файла в
/// потоке плеера), не трогает активный импорт рендера, а подготовленный им импорт ставится
/// на выдавший рендер и превращает кадр в ключ.
pub fn check_preparer_prepares_on_another_thread(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::PreparerPreparesOnAnotherThread;
    let mut renderer = create_renderer(harness)?;
    let ConformanceVideoSource {
        backend,
        plan,
        frame,
        ledger: _,
    } = harness.supported_video_source();
    let mut preparer = renderer.video_import_preparer();

    let prepared = std::thread::spawn(move || preparer.prepare_video_import(backend, plan))
        .join()
        .map_err(|_panic| ConformanceViolation::new(check, "preparer panicked on another thread"))?
        .map_err(|error| {
            ConformanceViolation::new(check, format!("preparer failed to prepare: {error:?}"))
        })?;
    if renderer.video_frames().active_video_import_path().is_some() {
        return Err(ConformanceViolation::new(
            check,
            "preparing on another thread must not change the active import",
        ));
    }
    renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .map_err(|error| {
            ConformanceViolation::new(
                check,
                format!("import prepared by the renderer's own preparer must install: {error:?}"),
            )
        })?;
    if renderer.video_frames().active_video_import_path() != Some(plan.path) {
        return Err(ConformanceViolation::new(
            check,
            "installed import must become the active import of its path",
        ));
    }

    let decoder_thread = prepared.backend.into_decoder_thread();
    let lease = VideoFrameLease::new(
        VideoFrameLeaseConfig::new(
            CONFORMANCE_RENDER_GENERATION,
            frame,
            Arc::new(ProviderReleaseSink),
        )
        .with_resource_provider(decoder_thread.resource_provider()),
    );
    match renderer.video_frames().materialize(&lease).outcome {
        FrameMaterializationOutcome::Ready(_) => Ok(()),
        other => Err(ConformanceViolation::new(
            check,
            format!("frame of the prepared import must be Ready, got {other:?}"),
        )),
    }
}

/// Показанный поддерживаемый кадр, который приложение ещё держит.
///
/// Поля роняются в порядке объявления, как у приложения: сначала ключ (ресурсы рендера),
/// потом аренда (освобождение кадра), декодер — последним.
struct PresentedSupportedFrame {
    /// Ключ кадра (держит аренду и ресурсы рендера); только удерживается до drop-а.
    _key: crate::RenderableFrameKey,
    /// Аренда кадра у «приложения»; только удерживается до drop-а.
    _lease: VideoFrameLease,
    /// Обёрнутый декодер: живёт, пока проверка держит кадр.
    _decoder_thread: Box<video_backend_api::VideoBackendDecoderThreadHandle>,
    /// Ресурс кадра.
    handle: FrameResourceHandle,
    /// Журнал освобождений внутреннего provider-а.
    ledger: ConformanceReleaseLedger,
}

/// Подключает декодер стенда, готовит его кадр и показывает его одним кадром.
fn present_supported_frame(
    renderer: &mut dyn RenderShell,
    harness: &mut dyn RenderShellConformanceHarness,
    check: ConformanceCheck,
    screen: RenderScreenDescriptor,
) -> Result<PresentedSupportedFrame, ConformanceViolation> {
    let source = harness.supported_video_source();
    let handle = source.frame.resource_handle;
    let prepared = renderer
        .video_frames()
        .prepare_video_import(source.backend, source.plan)
        .map_err(|error| {
            ConformanceViolation::new(check, format!("prepare_video_import failed: {error:?}"))
        })?;
    renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .map_err(|error| {
            ConformanceViolation::new(check, format!("install_video_import failed: {error:?}"))
        })?;

    // Плеер получил бы обёрнутый декодер; его provider и кладётся в аренду кадра.
    let decoder_thread = prepared.backend.into_decoder_thread();
    let lease = VideoFrameLease::new(
        VideoFrameLeaseConfig::new(
            CONFORMANCE_RENDER_GENERATION,
            source.frame,
            Arc::new(ProviderReleaseSink),
        )
        .with_resource_provider(decoder_thread.resource_provider()),
    );

    let materialization = renderer.video_frames().materialize(&lease);
    let key = match materialization.outcome {
        FrameMaterializationOutcome::Ready(key) => key,
        other => {
            return Err(ConformanceViolation::new(
                check,
                format!("supported frame with a bound decoder must be Ready, got {other:?}"),
            ));
        }
    };
    let outcome = render_with_acquired_surface(
        renderer,
        check,
        screen,
        Some(&key),
        EguiFrameContent::empty_fresh(),
    )?;
    expect_presented(check, "frame with video", &outcome)?;
    lease.mark_submitted_to_renderer();

    Ok(PresentedSupportedFrame {
        _key: key,
        _lease: lease,
        _decoder_thread: decoder_thread,
        handle,
        ledger: source.ledger,
    })
}

/// Sink аренды как fallback плеера: освобождение сразу уходит в provider аренды
/// (это обёртка рендера, которая сама дождётся GPU).
struct ProviderReleaseSink;

impl VideoFrameReleaseSink for ProviderReleaseSink {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        match release.resource_provider() {
            Some(provider) => {
                provider.release_frame(release.resource_handle());
                VideoFrameReleaseOutcome::Accepted
            }
            None => VideoFrameReleaseOutcome::NoOp,
        }
    }
}

/// Аренда без provider-а: для проверок, где до provider-а дело дойти не должно.
fn lease_without_provider(frame: DecodedFrame) -> VideoFrameLease {
    VideoFrameLease::new(VideoFrameLeaseConfig::new(
        CONFORMANCE_RENDER_GENERATION,
        frame,
        Arc::new(ProviderReleaseSink),
    ))
}

fn create_renderer(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<Box<dyn RenderShell>, ConformanceViolation> {
    harness.create_renderer().map_err(|error| {
        ConformanceViolation::new(
            ConformanceCheck::HarnessSetup,
            format!("renderer creation failed: {error}"),
        )
    })
}

/// Получает буфер и рисует кадр; отсутствие буфера на стенде — ошибка стенда.
fn render_with_acquired_surface(
    renderer: &mut dyn RenderShell,
    check: ConformanceCheck,
    screen: RenderScreenDescriptor,
    video_frame: Option<&crate::RenderableFrameKey>,
    egui: EguiFrameContent,
) -> Result<RenderFrameOutcome, ConformanceViolation> {
    match renderer.acquire_surface_frame(screen) {
        SurfaceFrameAcquire::Acquired(surface) => {
            Ok(renderer.render_frame(frame_input(surface, screen, video_frame, egui)))
        }
        SurfaceFrameAcquire::Dropped(reason) => Err(ConformanceViolation::new(
            ConformanceCheck::HarnessSetup,
            format!("{check:?}: harness surface was not acquired: {reason:?}"),
        )),
    }
}

fn frame_input<'frame>(
    surface: AcquiredSurfaceFrame,
    screen: RenderScreenDescriptor,
    video_frame: Option<&'frame crate::RenderableFrameKey>,
    egui: EguiFrameContent,
) -> RenderFrameInput<'frame> {
    let [width, height] = screen.size_in_pixels;
    RenderFrameInput {
        surface,
        video_frame,
        egui,
        screen,
        video_viewport: render_core::RenderViewport::full_surface(width, height),
        video_exclusion_rects: Vec::new(),
        window_corner_mask: WindowCornerMask::square(),
        subtitle_overlay: None,
    }
}

fn expect_presented(
    check: ConformanceCheck,
    what: &str,
    outcome: &RenderFrameOutcome,
) -> Result<(), ConformanceViolation> {
    match outcome {
        RenderFrameOutcome::Presented(_) => Ok(()),
        other => Err(ConformanceViolation::new(
            check,
            format!("{what} must be presented, got {other:?}"),
        )),
    }
}

fn wait_for_gpu_idle(
    renderer: &mut dyn RenderShell,
    check: ConformanceCheck,
) -> Result<(), ConformanceViolation> {
    renderer
        .wait_for_gpu_idle(CONFORMANCE_GPU_IDLE_TIMEOUT)
        .map_err(|error| ConformanceViolation::new(check, format!("GPU idle wait failed: {error}")))
}

fn expect_release_count(
    check: ConformanceCheck,
    moment: &str,
    ledger: &ConformanceReleaseLedger,
    handle: FrameResourceHandle,
    expected: usize,
) -> Result<(), ConformanceViolation> {
    let actual = ledger.release_count(handle);
    if actual == expected {
        Ok(())
    } else {
        Err(ConformanceViolation::new(
            check,
            format!(
                "decoder frame {handle:?} released {actual} times {moment}, expected {expected}"
            ),
        ))
    }
}
