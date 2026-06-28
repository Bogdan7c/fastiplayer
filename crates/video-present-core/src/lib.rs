//! Нейтральный контракт present-frame lease-а.
//!
//! Crate не знает о `player-core`, WGPU, VA-API или конкретном frame server-е.
//! Он описывает только RAII lease, lookup descriptor vocabulary и release sink,
//! чтобы playback и frame server могли использовать один lease contract.

#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use video_backend_api::{PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup};
use video_core::{DecodedFrame, FrameMemoryPath, FrameResourceHandle};

/// Shared release sink, который получает release ровно при drop-е последнего clone lease-а.
pub type SharedVideoFrameReleaseSink = Arc<dyn VideoFrameReleaseSink + Send + Sync>;

/// Shared diagnostics sink для renderer/resource lookup samples.
pub type SharedVideoFrameLeaseDiagnosticsSink =
    Arc<dyn VideoFrameLeaseDiagnosticsSink + Send + Sync>;

/// Renderer-neutral тип ресурса, который стоит за present frame lease-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPresentFrameResourceKind {
    /// Decoder-owned DMA-BUF доступен renderer-у как zero-copy GPU resource.
    DmaBufZeroCopy,

    /// Provider-owned HostPlanar frame доступен renderer-у через host-upload materializer.
    HostPlanar,

    /// Opaque backend texture скрыта за `FrameResourceHandle` без public GPU handles.
    OpaqueBackendTexture,

    /// Future external GPU handle, который backend materializer обработает самостоятельно.
    ExternalGpuHandle,
}

impl VideoPresentFrameResourceKind {
    /// Мапит decoded memory path в neutral resource kind без знания renderer-а.
    #[must_use]
    pub const fn from_memory_path(memory_path: FrameMemoryPath) -> Self {
        match memory_path {
            FrameMemoryPath::DmaBufZeroCopy => Self::DmaBufZeroCopy,
            FrameMemoryPath::CpuUpload => Self::HostPlanar,
        }
    }
}

/// Renderer-neutral descriptor present frame resource-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPresentFrameResourceDescriptor {
    /// Тип backend resource-а без привязки к API конкретного renderer-а.
    kind: VideoPresentFrameResourceKind,

    /// Поколение render resources, где был выдан opaque handle.
    render_generation: u64,

    /// Opaque handle decoded frame-а внутри backend-owned resource table.
    resource_handle: FrameResourceHandle,

    /// Memory path сохраняет distinction zero-copy/upload для diagnostics и policy.
    memory_path: FrameMemoryPath,
}

impl VideoPresentFrameResourceDescriptor {
    /// Собирает neutral descriptor из decoded frame metadata без renderer materialization.
    #[must_use]
    pub fn from_decoded_frame(render_generation: u64, frame: &DecodedFrame) -> Self {
        Self {
            kind: VideoPresentFrameResourceKind::from_memory_path(frame.memory_path()),
            render_generation,
            resource_handle: frame.resource_handle,
            memory_path: frame.memory_path(),
        }
    }

    /// Возвращает renderer-neutral kind ресурса для выбора materialization path-а.
    #[must_use]
    pub const fn kind(&self) -> VideoPresentFrameResourceKind {
        self.kind
    }

    /// Возвращает render generation, которому принадлежит opaque resource handle.
    #[must_use]
    pub const fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Возвращает opaque frame resource handle без раскрытия backend storage-а.
    #[must_use]
    pub const fn resource_handle(&self) -> FrameResourceHandle {
        self.resource_handle
    }

    /// Возвращает decoded memory path для diagnostics и backend policy decisions.
    #[must_use]
    pub const fn memory_path(&self) -> FrameMemoryPath {
        self.memory_path
    }
}

/// Stable identity decoded кадра на present/render boundary.
///
/// Этот тип намеренно не хранит texture views или owner pointers: он нужен только
/// для сравнения "тот же ли кадр" между scrub commit/match events и app-owned
/// visual override state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPresentFrameIdentity {
    /// Поколение render resources, которому принадлежит opaque resource handle.
    render_generation: u64,

    /// Opaque handle decoded frame-а внутри backend-owned resource table.
    resource_handle: FrameResourceHandle,

    /// Поколение decoded frame-а внутри текущего seek/decode lifecycle.
    decoded_generation: u64,

    /// Presentation timestamp decoded frame-а.
    pts: Duration,
}

impl VideoPresentFrameIdentity {
    /// Собирает stable identity из decoded frame metadata без materialization-а.
    #[must_use]
    pub fn from_decoded_frame(render_generation: u64, frame: &DecodedFrame) -> Self {
        Self {
            render_generation,
            resource_handle: frame.resource_handle,
            decoded_generation: frame.generation,
            pts: frame.pts,
        }
    }

    /// Возвращает render generation, которому принадлежит opaque resource handle.
    #[must_use]
    pub const fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Возвращает opaque resource handle decoded frame-а.
    #[must_use]
    pub const fn resource_handle(&self) -> FrameResourceHandle {
        self.resource_handle
    }

    /// Возвращает decoded generation, чтобы reuse той же texture не склеивал разные кадры.
    #[must_use]
    pub const fn decoded_generation(&self) -> u64 {
        self.decoded_generation
    }

    /// Возвращает presentation timestamp decoded frame-а.
    #[must_use]
    pub const fn pts(&self) -> Duration {
        self.pts
    }
}

/// Renderer-neutral результат lookup-а present frame resource-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPresentFrameResourceLookup {
    /// Resource доступен, backend может materialize его через renderer-specific путь.
    Ready(VideoPresentFrameResourceDescriptor),

    /// Backend resource table занят, caller должен выбрать safe fallback без stall-а.
    Busy,

    /// Resource отсутствует при доступном backend table-е.
    Missing,

    /// Backend сообщил poisoned/fatal lookup state.
    Fatal,
}

/// Diagnostics sample renderer/resource lookup-а без зависимости на playback diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameResourceLookupSample {
    /// Сколько render/frame-server side ждал backend resource pool lock-а.
    wait: Duration,

    /// Был ли lookup остановлен из-за занятого resource pool lock-а.
    lookup_was_busy: bool,

    /// PTS кадра, для которого запрашивали resource materialization.
    pts: Option<Duration>,

    /// Memory path кадра для сопоставления zero-copy/upload path-ов.
    memory_path: Option<FrameMemoryPath>,
}

impl VideoFrameResourceLookupSample {
    /// Создаёт sample с уже вычисленной семантикой lookup-а.
    #[must_use]
    const fn new(
        wait: Duration,
        lookup_was_busy: bool,
        pts: Option<Duration>,
        memory_path: Option<FrameMemoryPath>,
    ) -> Self {
        Self {
            wait,
            lookup_was_busy,
            pts,
            memory_path,
        }
    }

    /// Возвращает длительность ожидания backend resource lock-а.
    #[must_use]
    pub const fn wait(&self) -> Duration {
        self.wait
    }

    /// Возвращает `true`, если lookup завершился typed Busy.
    #[must_use]
    pub const fn lookup_was_busy(&self) -> bool {
        self.lookup_was_busy
    }

    /// Возвращает PTS кадра, если sample относится к конкретному lease-у.
    #[must_use]
    pub const fn pts(&self) -> Option<Duration> {
        self.pts
    }

    /// Возвращает memory path кадра, если sample относится к конкретному lease-у.
    #[must_use]
    pub const fn memory_path(&self) -> Option<FrameMemoryPath> {
        self.memory_path
    }
}

/// Object-safe sink для diagnostics lookup-а.
pub trait VideoFrameLeaseDiagnosticsSink: Send + Sync {
    /// Принимает sample без ожидания owner thread-а.
    fn report_resource_lookup_sample(&self, sample: VideoFrameResourceLookupSample);
}

/// Причина fallback release-а вне owner accounting path-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFrameReleaseFallbackReason {
    /// Owner release queue была заполнена дольше допустимого бюджета.
    Backpressure,

    /// Owner release sink уже недоступен.
    Disconnected,
}

/// Typed результат передачи release-а владельцу accounting-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFrameReleaseOutcome {
    /// Release принят владельцем accounting-а.
    Accepted,

    /// Sink освободил provider resource через fallback path.
    FallbackReleased {
        /// Почему release не прошёл через основной owner accounting path.
        reason: VideoFrameReleaseFallbackReason,
    },

    /// Fallback был нужен, но provider отсутствовал.
    FallbackUnavailable {
        /// Почему release не прошёл через основной owner accounting path.
        reason: VideoFrameReleaseFallbackReason,
    },

    /// Sink намеренно ничего не сделал.
    NoOp,

    /// Sink не смог безопасно обработать release.
    Fatal,
}

/// Object-safe sink, который сохраняет owner-specific accounting за пределами lease crate-а.
pub trait VideoFrameReleaseSink: Send + Sync {
    /// Принимает release последнего clone lease-а.
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome;
}

/// Явно описывает, дошёл ли lease до renderer submit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFrameRendererSubmission {
    /// Lease был только получен/lookup-нут, но не отправлен в renderer submit.
    NotSubmitted,

    /// Хотя бы один clone lease-а был включён в renderer submit.
    Submitted,
}

impl VideoFrameRendererSubmission {
    /// Возвращает compact bool только для старых accounting boundaries.
    #[must_use]
    pub const fn submitted_to_renderer(self) -> bool {
        matches!(self, Self::Submitted)
    }
}

/// Drop-time release request, не привязанный к `player-core`.
#[derive(Clone)]
pub struct VideoFrameRelease {
    /// Поколение render resources, где lease был создан.
    render_generation: u64,

    /// Resource handle, который больше не удерживает render/frame-server side.
    resource_handle: FrameResourceHandle,

    /// Provider исходного decoder resource pool для fallback release после смены owner-а.
    resource_provider: Option<PresentFrameResourceProviderHandle>,

    /// Был ли lease реально включён в renderer submit.
    submitted_to_renderer: bool,

    /// Монотонный момент drop-а render lease-а.
    released_at: Instant,
}

impl VideoFrameRelease {
    /// Создаёт release request; production path обычно делает это через `VideoFrameLease`.
    #[must_use]
    pub fn new(
        render_generation: u64,
        resource_handle: FrameResourceHandle,
        resource_provider: Option<PresentFrameResourceProviderHandle>,
        renderer_submission: VideoFrameRendererSubmission,
        released_at: Instant,
    ) -> Self {
        Self {
            render_generation,
            resource_handle,
            resource_provider,
            submitted_to_renderer: renderer_submission.submitted_to_renderer(),
            released_at,
        }
    }

    /// Возвращает render generation исходного lease-а.
    #[must_use]
    pub const fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Возвращает released resource handle.
    #[must_use]
    pub const fn resource_handle(&self) -> FrameResourceHandle {
        self.resource_handle
    }

    /// Возвращает provider для fallback/renderer-owned release path-а.
    #[must_use]
    pub fn resource_provider(&self) -> Option<&PresentFrameResourceProviderHandle> {
        self.resource_provider.as_ref()
    }

    /// Возвращает `true`, если lease дошёл до renderer submit-а.
    #[must_use]
    pub const fn submitted_to_renderer(&self) -> bool {
        self.submitted_to_renderer
    }

    /// Возвращает момент drop-а lease-а.
    #[must_use]
    pub const fn released_at(&self) -> Instant {
        self.released_at
    }
}

/// Конфигурация нового lease-а; owner layer собирает её на своей boundary.
pub struct VideoFrameLeaseConfig {
    /// Render generation, которому принадлежит resource handle.
    render_generation: u64,

    /// Metadata decoded frame без доступа к owner pipeline/session.
    frame: DecodedFrame,

    /// Признак stale visual fallback-а для текущего timeline состояния.
    stale: bool,

    /// Renderer-neutral provider для status lookup-а и fallback release-а.
    resource_provider: Option<PresentFrameResourceProviderHandle>,

    /// Sink, который владеет accounting/release policy.
    release_sink: SharedVideoFrameReleaseSink,

    /// Optional diagnostics sink для resource lookup samples.
    diagnostics_sink: Option<SharedVideoFrameLeaseDiagnosticsSink>,
}

impl VideoFrameLeaseConfig {
    /// Создаёт минимальную конфигурацию lease-а с обязательным release sink.
    #[must_use]
    pub fn new(
        render_generation: u64,
        frame: DecodedFrame,
        release_sink: SharedVideoFrameReleaseSink,
    ) -> Self {
        Self {
            render_generation,
            frame,
            stale: false,
            resource_provider: None,
            release_sink,
            diagnostics_sink: None,
        }
    }

    /// Добавляет provider, который владеет resource lookup/release boundary.
    #[must_use]
    pub fn with_resource_provider(
        mut self,
        resource_provider: PresentFrameResourceProviderHandle,
    ) -> Self {
        self.resource_provider = Some(resource_provider);
        self
    }

    /// Помечает lease как stale visual fallback без позиционного boolean-а.
    #[must_use]
    pub const fn with_timeline_stale(mut self) -> Self {
        self.stale = true;
        self
    }

    /// Добавляет owner-specific diagnostics sink для lookup samples.
    #[must_use]
    pub fn with_diagnostics_sink(
        mut self,
        diagnostics_sink: SharedVideoFrameLeaseDiagnosticsSink,
    ) -> Self {
        self.diagnostics_sink = Some(diagnostics_sink);
        self
    }
}

/// Lease текущего decoded frame-а, который render/frame-server side может удерживать.
#[derive(Clone)]
pub struct VideoFrameLease {
    /// Поколение render resources, которому принадлежит resource handle.
    render_generation: u64,

    /// Metadata decoded frame без прямого доступа к owner pipeline/session.
    frame: DecodedFrame,

    /// Признак, что кадр является stale fallback для текущего timeline состояния.
    stale: bool,

    /// Renderer-neutral provider для status lookup-а.
    resource_provider: Option<PresentFrameResourceProviderHandle>,

    /// Optional sink для resource lookup diagnostics.
    diagnostics_sink: Option<SharedVideoFrameLeaseDiagnosticsSink>,

    /// Shared guard отправляет release, когда последний clone кадра освобождён.
    drop_ack: Arc<VideoFrameLeaseDropAck>,
}

impl VideoFrameLease {
    /// Собирает RAII lease из owner-provided конфигурации.
    #[must_use]
    pub fn new(config: VideoFrameLeaseConfig) -> Self {
        let resource_handle = config.frame.resource_handle;

        Self {
            render_generation: config.render_generation,
            frame: config.frame,
            stale: config.stale,
            resource_provider: config.resource_provider.clone(),
            diagnostics_sink: config.diagnostics_sink,
            drop_ack: Arc::new(VideoFrameLeaseDropAck {
                render_generation: config.render_generation,
                resource_handle,
                resource_provider: config.resource_provider,
                submitted_to_renderer: AtomicBool::new(false),
                release_sink: config.release_sink,
            }),
        }
    }

    /// Возвращает render generation, которому принадлежит lease.
    #[must_use]
    pub const fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Возвращает decoded frame metadata для renderer/materializer boundary.
    #[must_use]
    pub const fn decoded_frame(&self) -> &DecodedFrame {
        &self.frame
    }

    /// Возвращает opaque resource handle кадра без доступа к owner pipeline.
    #[must_use]
    pub const fn resource_handle(&self) -> FrameResourceHandle {
        self.frame.resource_handle
    }

    /// Возвращает `true`, если lease является stale visual fallback-ом.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.stale
    }

    /// Помечает конкретный clone lease-а как stale visual fallback.
    pub const fn mark_timeline_stale(&mut self) {
        self.stale = true;
    }

    /// Отмечает, что renderer действительно включил этот lease в GPU submit.
    pub fn mark_submitted_to_renderer(&self) {
        self.drop_ack
            .submitted_to_renderer
            .store(true, Ordering::Release);
    }

    /// Возвращает `true`, если lease устарел относительно актуального render generation.
    #[must_use]
    pub const fn stale_for_generation(&self, current_render_generation: u64) -> bool {
        self.stale || self.render_generation != current_render_generation
    }

    /// Возвращает neutral descriptor без попытки materialize backend resource.
    #[must_use]
    pub fn resource_descriptor(&self) -> VideoPresentFrameResourceDescriptor {
        VideoPresentFrameResourceDescriptor::from_decoded_frame(self.render_generation, &self.frame)
    }

    /// Пытается проверить доступность present resource-а без GPU handles.
    #[must_use]
    pub fn try_resource_lookup(&self) -> VideoPresentFrameResourceLookup {
        let Some(provider) = self.resource_provider.as_ref() else {
            return VideoPresentFrameResourceLookup::Missing;
        };
        let lookup = provider.try_resource_lookup(self.resource_handle());

        self.present_resource_lookup_from_boundary(lookup)
    }

    /// Записывает diagnostics lookup-а, выполненного renderer-specific materializer-ом.
    pub fn report_resource_lookup_sample(&self, wait: Duration, lookup_was_busy: bool) {
        self.report_resource_lock_sample(wait, lookup_was_busy);
    }

    /// Конвертирует backend lookup в neutral public outcome и сохраняет diagnostics.
    fn present_resource_lookup_from_boundary(
        &self,
        lookup: PresentFrameResourceProviderLookup,
    ) -> VideoPresentFrameResourceLookup {
        self.report_resource_provider_lookup_sample(&lookup);

        match lookup {
            PresentFrameResourceProviderLookup::Ready { .. } => {
                VideoPresentFrameResourceLookup::Ready(self.resource_descriptor())
            }
            PresentFrameResourceProviderLookup::Busy { .. } => {
                VideoPresentFrameResourceLookup::Busy
            }
            PresentFrameResourceProviderLookup::Missing { .. } => {
                VideoPresentFrameResourceLookup::Missing
            }
            PresentFrameResourceProviderLookup::Fatal { .. } => {
                VideoPresentFrameResourceLookup::Fatal
            }
        }
    }

    /// Записывает diagnostics sample для renderer-neutral provider lookup-а.
    fn report_resource_provider_lookup_sample(&self, lookup: &PresentFrameResourceProviderLookup) {
        let resource_pool_lock_wait = lookup.resource_pool_lock_wait();
        let lookup_was_busy = matches!(lookup, PresentFrameResourceProviderLookup::Busy { .. });
        self.report_resource_lock_sample(resource_pool_lock_wait, lookup_was_busy);
    }

    /// Отправляет lock-wait sample в diagnostics sink без ожидания owner thread-а.
    fn report_resource_lock_sample(&self, wait: Duration, lookup_was_busy: bool) {
        let Some(diagnostics_sink) = &self.diagnostics_sink else {
            return;
        };

        diagnostics_sink.report_resource_lookup_sample(VideoFrameResourceLookupSample::new(
            wait,
            lookup_was_busy,
            Some(self.frame.pts),
            Some(self.frame.memory_path()),
        ));
    }
}

/// Shared guard, который отправляет release ровно один раз на группу clone-ов.
pub struct VideoFrameLeaseDropAck {
    /// Поколение render resources, где lease был создан.
    render_generation: u64,

    /// Resource handle, защищённый от premature release.
    resource_handle: FrameResourceHandle,

    /// Provider исходного frame-а; нужен, если owner accounting path недоступен.
    resource_provider: Option<PresentFrameResourceProviderHandle>,

    /// Shared флаг: хотя бы один clone lease-а был отправлен в renderer submit.
    submitted_to_renderer: AtomicBool,

    /// Owner-specific sink для accounting и fallback release policy.
    release_sink: SharedVideoFrameReleaseSink,
}

impl Drop for VideoFrameLeaseDropAck {
    /// Освобождает render lease без участия UI/frame-server кода.
    fn drop(&mut self) {
        let renderer_submission = if self.submitted_to_renderer.load(Ordering::Acquire) {
            VideoFrameRendererSubmission::Submitted
        } else {
            VideoFrameRendererSubmission::NotSubmitted
        };
        let release = VideoFrameRelease::new(
            self.render_generation,
            self.resource_handle,
            self.resource_provider.clone(),
            renderer_submission,
            Instant::now(),
        );

        let _outcome = self.release_sink.release_frame(release);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use codec_core::VideoColorMetadata;
    use video_backend_api::PresentFrameResourceProvider;
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    use super::*;

    struct RecordingReleaseSink {
        releases: Mutex<Vec<VideoFrameRelease>>,
        outcome: VideoFrameReleaseOutcome,
    }

    impl RecordingReleaseSink {
        fn new(outcome: VideoFrameReleaseOutcome) -> Self {
            Self {
                releases: Mutex::new(Vec::new()),
                outcome,
            }
        }

        fn release_count(&self) -> usize {
            self.releases.lock().unwrap().len()
        }

        fn latest_release(&self) -> VideoFrameRelease {
            self.releases.lock().unwrap().last().unwrap().clone()
        }
    }

    impl VideoFrameReleaseSink for RecordingReleaseSink {
        fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
            self.releases.lock().unwrap().push(release);
            self.outcome
        }
    }

    struct RecordingDiagnosticsSink {
        samples: Mutex<Vec<VideoFrameResourceLookupSample>>,
    }

    impl RecordingDiagnosticsSink {
        fn new() -> Self {
            Self {
                samples: Mutex::new(Vec::new()),
            }
        }

        fn latest_sample(&self) -> VideoFrameResourceLookupSample {
            *self.samples.lock().unwrap().last().unwrap()
        }
    }

    impl VideoFrameLeaseDiagnosticsSink for RecordingDiagnosticsSink {
        fn report_resource_lookup_sample(&self, sample: VideoFrameResourceLookupSample) {
            self.samples.lock().unwrap().push(sample);
        }
    }

    struct LookupProvider {
        expected_handle: FrameResourceHandle,
        lookup: PresentFrameResourceProviderLookup,
        released_handles: Arc<Mutex<Vec<FrameResourceHandle>>>,
    }

    impl PresentFrameResourceProvider for LookupProvider {
        fn resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            assert_eq!(handle, self.expected_handle);
            self.lookup
        }

        fn try_resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            self.resource_lookup(handle)
        }

        fn release_frame(&self, handle: FrameResourceHandle) {
            self.released_handles.lock().unwrap().push(handle);
        }
    }

    fn decoded_frame_with_contract_for_tests(
        resource_handle: FrameResourceHandle,
        frame_contract: VideoFrameContract,
    ) -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::from_millis(42),
            frame_contract,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    fn decoded_frame_for_tests(resource_handle: FrameResourceHandle) -> DecodedFrame {
        decoded_frame_with_contract_for_tests(
            resource_handle,
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        )
    }

    fn lease_for_tests(
        render_generation: u64,
        resource_handle: FrameResourceHandle,
        release_sink: SharedVideoFrameReleaseSink,
    ) -> VideoFrameLease {
        VideoFrameLease::new(VideoFrameLeaseConfig::new(
            render_generation,
            decoded_frame_for_tests(resource_handle),
            release_sink,
        ))
    }

    fn lease_with_provider_for_tests(
        resource_handle: FrameResourceHandle,
        provider_lookup: PresentFrameResourceProviderLookup,
        diagnostics_sink: SharedVideoFrameLeaseDiagnosticsSink,
        release_sink: SharedVideoFrameReleaseSink,
    ) -> VideoFrameLease {
        let released_handles = Arc::new(Mutex::new(Vec::new()));
        let resource_provider = PresentFrameResourceProviderHandle::new(LookupProvider {
            expected_handle: resource_handle,
            lookup: provider_lookup,
            released_handles,
        });
        VideoFrameLease::new(
            VideoFrameLeaseConfig::new(9, decoded_frame_for_tests(resource_handle), release_sink)
                .with_resource_provider(resource_provider)
                .with_diagnostics_sink(diagnostics_sink),
        )
    }

    #[test]
    fn resource_descriptor_exposes_renderer_neutral_dma_buf_handle() {
        let resource_handle = FrameResourceHandle(76);
        let release_sink = Arc::new(RecordingReleaseSink::new(
            VideoFrameReleaseOutcome::Accepted,
        ));
        let lease = lease_for_tests(8, resource_handle, release_sink);

        let descriptor = lease.resource_descriptor();

        assert_eq!(
            descriptor.kind(),
            VideoPresentFrameResourceKind::DmaBufZeroCopy
        );
        assert_eq!(descriptor.render_generation(), 8);
        assert_eq!(descriptor.resource_handle(), resource_handle);
        assert_eq!(descriptor.memory_path(), FrameMemoryPath::DmaBufZeroCopy);
    }

    #[test]
    fn present_frame_identity_distinguishes_reused_resource_handle_frames() {
        let resource_handle = FrameResourceHandle(76);
        let previous_frame = decoded_frame_for_tests(resource_handle);
        let mut next_generation_frame = previous_frame.clone();
        let mut next_pts_frame = previous_frame.clone();
        next_generation_frame.generation = previous_frame.generation + 1;
        next_pts_frame.pts += Duration::from_millis(33);

        let previous_identity = VideoPresentFrameIdentity::from_decoded_frame(8, &previous_frame);

        assert_ne!(
            previous_identity,
            VideoPresentFrameIdentity::from_decoded_frame(8, &next_generation_frame)
        );
        assert_ne!(
            previous_identity,
            VideoPresentFrameIdentity::from_decoded_frame(8, &next_pts_frame)
        );
        assert_eq!(previous_identity.render_generation(), 8);
        assert_eq!(previous_identity.resource_handle(), resource_handle);
        assert_eq!(
            previous_identity.decoded_generation(),
            previous_frame.generation
        );
        assert_eq!(previous_identity.pts(), previous_frame.pts);
    }

    #[test]
    fn resource_descriptor_exposes_renderer_neutral_host_planar_handle() {
        let resource_handle = FrameResourceHandle(78);
        let decoded_frame = decoded_frame_with_contract_for_tests(
            resource_handle,
            VideoFrameContract::host_yuv420_planar8(),
        );

        let descriptor =
            VideoPresentFrameResourceDescriptor::from_decoded_frame(10, &decoded_frame);

        assert_eq!(descriptor.kind(), VideoPresentFrameResourceKind::HostPlanar);
        assert_eq!(descriptor.render_generation(), 10);
        assert_eq!(descriptor.resource_handle(), resource_handle);
        assert_eq!(descriptor.memory_path(), FrameMemoryPath::CpuUpload);
    }

    #[test]
    fn lookup_preserves_missing_busy_ready_and_fatal_outcomes() {
        let release_sink = Arc::new(RecordingReleaseSink::new(
            VideoFrameReleaseOutcome::Accepted,
        ));
        let diagnostics_sink = Arc::new(RecordingDiagnosticsSink::new());
        let resource_handle = FrameResourceHandle(77);
        let ready_lease = lease_with_provider_for_tests(
            resource_handle,
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::from_micros(10),
            },
            diagnostics_sink.clone(),
            release_sink.clone(),
        );
        assert!(matches!(
            ready_lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Ready(_)
        ));

        let busy_lease = lease_with_provider_for_tests(
            resource_handle,
            PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait: Duration::from_micros(20),
            },
            diagnostics_sink.clone(),
            release_sink.clone(),
        );
        assert!(matches!(
            busy_lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Busy
        ));

        let missing_lease = lease_with_provider_for_tests(
            resource_handle,
            PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait: Duration::from_micros(30),
            },
            diagnostics_sink.clone(),
            release_sink.clone(),
        );
        assert!(matches!(
            missing_lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Missing
        ));

        let fatal_lease = lease_with_provider_for_tests(
            resource_handle,
            PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait: Duration::from_micros(40),
            },
            diagnostics_sink,
            release_sink,
        );
        assert!(matches!(
            fatal_lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Fatal
        ));
    }

    #[test]
    fn lookup_records_wait_busy_pts_and_memory_path_sample() {
        let release_sink = Arc::new(RecordingReleaseSink::new(
            VideoFrameReleaseOutcome::Accepted,
        ));
        let diagnostics_sink = Arc::new(RecordingDiagnosticsSink::new());
        let resource_handle = FrameResourceHandle(80);
        let lease = lease_with_provider_for_tests(
            resource_handle,
            PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait: Duration::from_micros(250),
            },
            diagnostics_sink.clone(),
            release_sink,
        );

        assert!(matches!(
            lease.try_resource_lookup(),
            VideoPresentFrameResourceLookup::Busy
        ));

        let sample = diagnostics_sink.latest_sample();
        assert_eq!(sample.wait(), Duration::from_micros(250));
        assert!(sample.lookup_was_busy());
        assert_eq!(sample.pts(), Some(Duration::from_millis(42)));
        assert_eq!(sample.memory_path(), Some(FrameMemoryPath::DmaBufZeroCopy));
    }

    #[test]
    fn drop_releases_exactly_once_across_clones() {
        let release_sink = Arc::new(RecordingReleaseSink::new(
            VideoFrameReleaseOutcome::Accepted,
        ));
        let lease = lease_for_tests(2, FrameResourceHandle(12), release_sink.clone());
        let lease_clone = lease.clone();

        drop(lease);
        assert_eq!(release_sink.release_count(), 0);

        drop(lease_clone);

        assert_eq!(release_sink.release_count(), 1);
        let release = release_sink.latest_release();
        assert_eq!(release.render_generation(), 2);
        assert_eq!(release.resource_handle(), FrameResourceHandle(12));
        assert!(!release.submitted_to_renderer());
    }

    #[test]
    fn submitted_clone_marks_shared_release() {
        let release_sink = Arc::new(RecordingReleaseSink::new(
            VideoFrameReleaseOutcome::Accepted,
        ));
        let lease = lease_for_tests(3, FrameResourceHandle(13), release_sink.clone());
        let submitted_clone = lease.clone();

        submitted_clone.mark_submitted_to_renderer();
        drop(lease);
        drop(submitted_clone);

        let release = release_sink.latest_release();
        assert!(release.submitted_to_renderer());
    }

    #[test]
    fn stale_marker_is_clone_local_and_generation_aware() {
        let release_sink = Arc::new(RecordingReleaseSink::new(
            VideoFrameReleaseOutcome::Accepted,
        ));
        let mut lease = lease_for_tests(4, FrameResourceHandle(14), release_sink);
        let unchanged_clone = lease.clone();

        lease.mark_timeline_stale();

        assert!(lease.is_stale());
        assert!(!unchanged_clone.is_stale());
        assert!(lease.stale_for_generation(4));
        assert!(unchanged_clone.stale_for_generation(5));
    }

    #[test]
    fn sink_outcome_is_typed_without_affecting_drop_once_semantics() {
        let release_sink = Arc::new(RecordingReleaseSink::new(VideoFrameReleaseOutcome::NoOp));
        let lease = lease_for_tests(5, FrameResourceHandle(15), release_sink.clone());

        drop(lease);

        assert_eq!(release_sink.release_count(), 1);
        assert_eq!(
            release_sink.outcome,
            VideoFrameReleaseOutcome::NoOp,
            "test sink keeps typed outcome distinct from release delivery"
        );
    }
}
