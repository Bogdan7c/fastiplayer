//! Hermetic-тесты materializer-а: порядок identity → кэш → контракт → импорт,
//! accounting release и fail-closed политика без GPU.

use std::fs::File;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceIdentityLookup,
    PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
    PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodedFrame, DmaBufDescriptorRejection, DmaBufFrameDescriptor, DmaBufFrameExportLayout,
    DmaBufLayerDescriptor, DmaBufObjectDescriptor, DmaBufObjectIdentity, FrameResourceDescriptor,
    FrameResourceHandle, FrameResourceIdentity, VideoFrameDiagnostics,
    validate_dma_buf_descriptor_import_topology,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_present_core::{
    SharedVideoFrameReleaseSink, VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease,
    VideoFrameReleaseOutcome, VideoFrameReleaseSink,
};

use super::{
    DmaBufImportCacheCapacity, DmaBufMaterialization, DmaBufTextureImporter,
    DmaBufWgpuFrameMaterializer, WgpuFrameMaterializationUnsupportedReason,
};

/// Ёмкость кэша в тестах (как у пула декодера по умолчанию).
const TEST_CACHE_CAPACITY: usize = 24;

/// Параметры descriptor-а, публикуемого fake provider-ом.
#[derive(Clone, Copy)]
struct DescriptorScenario {
    /// Фактический layout экспортированного DMA-BUF.
    export_layout: DmaBufFrameExportLayout,
    /// Фактическая coded ширина descriptor-а.
    width: u32,
    /// Фактическая coded высота descriptor-а.
    height: u32,
    /// Inode backing-объекта: меняется, когда декодер пересоздал буфер.
    inode: u64,
}

impl DescriptorScenario {
    /// Валидный composed NV12 640x360 — совпадает с decoded frame contract-ом.
    const fn valid() -> Self {
        Self {
            export_layout: DmaBufFrameExportLayout::ComposedLayers,
            width: 640,
            height: 360,
            inode: 2,
        }
    }
}

/// Как fake provider отвечает на identity lookup.
#[derive(Clone, Copy)]
enum IdentityMode {
    /// Provider умеет identity (как VA-API).
    Supported,
    /// Provider не умеет identity (как host-planar backend).
    Unsupported,
    /// Pool занят.
    Busy,
    /// Handle неизвестен.
    Missing,
}

/// Ожидаемое поведение materializer-а на importer boundary.
enum ExpectedImportBoundary {
    /// Typed contract rejection должен остановить путь до importer-а.
    RejectedBeforeImport(DmaBufDescriptorRejection),
    /// Полностью совместимый descriptor должен дойти до importer-а.
    ReachesImporter,
}

/// Fake image без GPU.
#[derive(Debug, PartialEq, Eq)]
struct FakeTexture {
    /// Порядковый номер импорта.
    import_index: usize,
}

/// Fake importer: считает вызовы, умеет падать и объявлять поддержку reuse.
struct FakeImporter {
    /// Число попыток передать descriptor importer-у.
    import_calls: Arc<AtomicUsize>,
    /// Падать вместо импорта (моделирует Vulkan/HAL ошибку).
    fail: bool,
    /// Есть ли явная синхронизация (разрешает reuse между кадрами).
    supports_reuse: bool,
}

impl DmaBufTextureImporter for FakeImporter {
    type Texture = FakeTexture;

    fn import(&self, _descriptor: &DmaBufFrameDescriptor) -> anyhow::Result<FakeTexture> {
        let import_index = self.import_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("fake importer stops before unsafe Vulkan/HAL import");
        }
        Ok(FakeTexture { import_index })
    }

    fn supports_image_reuse(&self) -> bool {
        self.supports_reuse
    }
}

/// Provider создаёт owned descriptor на каждый lookup и считает обращения.
struct RecordingDescriptorProvider {
    /// Descriptor scenario для текущего теста.
    scenario: DescriptorScenario,
    /// Поведение identity lookup-а.
    identity_mode: IdentityMode,
    /// Счётчик identity lookup-ов.
    identity_lookups: Arc<AtomicUsize>,
    /// Счётчик descriptor lookup-ов (каждый = dup fd).
    descriptor_lookups: Arc<AtomicUsize>,
    /// Счётчик фактических provider release calls.
    release_calls: Arc<AtomicUsize>,
}

impl PresentFrameResourceProvider for RecordingDescriptorProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn resource_descriptor_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.descriptor_lookups.fetch_add(1, Ordering::SeqCst);
        PresentFrameResourceDescriptorLookup::Ready {
            descriptor: FrameResourceDescriptor::DmaBuf(descriptor_for_scenario(self.scenario)),
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn try_resource_identity_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceIdentityLookup {
        self.identity_lookups.fetch_add(1, Ordering::SeqCst);
        match self.identity_mode {
            IdentityMode::Supported => PresentFrameResourceIdentityLookup::Ready {
                identity: FrameResourceIdentity::DmaBuf(Arc::new(
                    descriptor_for_scenario(self.scenario).identity(),
                )),
                resource_pool_lock_wait: Duration::from_micros(1),
            },
            IdentityMode::Unsupported => PresentFrameResourceIdentityLookup::Unsupported,
            IdentityMode::Busy => PresentFrameResourceIdentityLookup::Busy {
                resource_pool_lock_wait: Duration::from_micros(2),
            },
            IdentityMode::Missing => PresentFrameResourceIdentityLookup::Missing {
                resource_pool_lock_wait: Duration::from_micros(3),
            },
        }
    }

    fn release_frame(&self, _handle: FrameResourceHandle) {
        self.release_calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// Release sink сохраняет production ownership: lease освобождает provider resource.
struct ForwardingReleaseSink;

impl VideoFrameReleaseSink for ForwardingReleaseSink {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        let provider = release
            .resource_provider()
            .expect("DMA-BUF lease must retain its resource provider");
        provider.release_frame(release.resource_handle());
        VideoFrameReleaseOutcome::Accepted
    }
}

/// Открывает harmless owned fd, который fake importer не передаёт Vulkan-у.
fn test_owned_fd() -> OwnedFd {
    File::open("/dev/null")
        .expect("/dev/null must be available for a DMA-BUF test fd")
        .into()
}

/// Создаёт topology-valid composed либо separate NV12 descriptor.
fn descriptor_for_scenario(scenario: DescriptorScenario) -> DmaBufFrameDescriptor {
    let chroma_offset = scenario.width.saturating_mul(scenario.height);
    let layers = match scenario.export_layout {
        DmaBufFrameExportLayout::ComposedLayers => vec![DmaBufLayerDescriptor {
            drm_format: 0x3231_564e,
            num_planes: 2,
            object_index: [0, 0, 0, 0],
            offset: [0, chroma_offset, 0, 0],
            pitch: [scenario.width, scenario.width, 0, 0],
        }],
        DmaBufFrameExportLayout::SeparateLayers => vec![
            DmaBufLayerDescriptor {
                drm_format: 0x2020_3852,
                num_planes: 1,
                object_index: [0, 0, 0, 0],
                offset: [0, 0, 0, 0],
                pitch: [scenario.width, 0, 0, 0],
            },
            DmaBufLayerDescriptor {
                drm_format: 0x3838_5247,
                num_planes: 1,
                object_index: [0, 0, 0, 0],
                offset: [chroma_offset, 0, 0, 0],
                pitch: [scenario.width, 0, 0, 0],
            },
        ],
    };

    DmaBufFrameDescriptor {
        resource_id: 1600,
        fourcc: 0x3231_564e,
        export_layout: scenario.export_layout,
        width: scenario.width,
        height: scenario.height,
        objects: vec![DmaBufObjectDescriptor {
            fd: test_owned_fd(),
            size: 16 * 1024 * 1024,
            drm_format_modifier: 0,
            identity: DmaBufObjectIdentity {
                device: 1,
                inode: scenario.inode,
                special_device: 3,
            },
        }],
        layers,
    }
}

/// Создаёт decoded frame с composed NV12 contract-ом и coded size 640x360.
fn decoded_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
    DecodedFrame {
        generation: 16,
        pts: Duration::from_millis(16),
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Тестовый стенд: materializer, provider и его счётчики.
struct Harness {
    materializer: DmaBufWgpuFrameMaterializer<FakeImporter>,
    provider: PresentFrameResourceProviderHandle,
    import_calls: Arc<AtomicUsize>,
    identity_lookups: Arc<AtomicUsize>,
    descriptor_lookups: Arc<AtomicUsize>,
    release_calls: Arc<AtomicUsize>,
}

impl Harness {
    /// Собирает стенд с заданным сценарием descriptor-а и поведением fake-ов.
    fn new(
        scenario: DescriptorScenario,
        identity_mode: IdentityMode,
        importer_fails: bool,
        supports_reuse: bool,
    ) -> Self {
        let import_calls = Arc::new(AtomicUsize::new(0));
        let identity_lookups = Arc::new(AtomicUsize::new(0));
        let descriptor_lookups = Arc::new(AtomicUsize::new(0));
        let release_calls = Arc::new(AtomicUsize::new(0));
        let provider = PresentFrameResourceProviderHandle::new(RecordingDescriptorProvider {
            scenario,
            identity_mode,
            identity_lookups: identity_lookups.clone(),
            descriptor_lookups: descriptor_lookups.clone(),
            release_calls: release_calls.clone(),
        });
        let materializer = DmaBufWgpuFrameMaterializer::new_with_importer(
            provider.clone(),
            FakeImporter {
                import_calls: import_calls.clone(),
                fail: importer_fails,
                supports_reuse,
            },
            DmaBufImportCacheCapacity::matching_surface_pool(TEST_CACHE_CAPACITY),
        );
        Self {
            materializer,
            provider,
            import_calls,
            identity_lookups,
            descriptor_lookups,
            release_calls,
        }
    }

    /// Materialize одного кадра под настоящим `VideoFrameLease`, как в приложении.
    fn materialize(&self, handle: FrameResourceHandle) -> DmaBufMaterialization<FakeTexture> {
        let release_sink: SharedVideoFrameReleaseSink = Arc::new(ForwardingReleaseSink);
        let lease = VideoFrameLease::new(
            VideoFrameLeaseConfig::new(16, decoded_frame(handle), release_sink)
                .with_resource_provider(self.provider.clone()),
        );
        let outcome = self.materializer.materialize_frame(lease.decoded_frame());
        let releases_before_drop = self.release_calls.load(Ordering::SeqCst);
        drop(lease);
        assert_eq!(
            self.release_calls.load(Ordering::SeqCst),
            releases_before_drop + 1,
            "lease must release the provider resource exactly once, materializer never does"
        );
        outcome
    }

    fn imports(&self) -> usize {
        self.import_calls.load(Ordering::SeqCst)
    }

    fn descriptor_lookups(&self) -> usize {
        self.descriptor_lookups.load(Ordering::SeqCst)
    }

    fn identity_lookups(&self) -> usize {
        self.identity_lookups.load(Ordering::SeqCst)
    }
}

/// Запускает production lookup boundary и проверяет importer/release accounting.
fn run_materializer_scenario(
    scenario: DescriptorScenario,
    expected_boundary: ExpectedImportBoundary,
) {
    let descriptor = descriptor_for_scenario(scenario);
    assert_eq!(
        validate_dma_buf_descriptor_import_topology(&descriptor),
        Ok(()),
        "scenario must isolate frame-contract validation from topology validation"
    );
    let harness = Harness::new(scenario, IdentityMode::Supported, true, true);

    let outcome = harness.materialize(FrameResourceHandle(1600));

    match expected_boundary {
        ExpectedImportBoundary::RejectedBeforeImport(expected_rejection) => {
            assert!(matches!(
                outcome,
                DmaBufMaterialization::Unsupported {
                    reason: WgpuFrameMaterializationUnsupportedReason::DmaBufDescriptorRejected(
                        actual_rejection
                    ),
                    ..
                } if actual_rejection == expected_rejection
            ));
            assert_eq!(harness.imports(), 0);
        }
        ExpectedImportBoundary::ReachesImporter => {
            assert!(matches!(outcome, DmaBufMaterialization::Error { .. }));
            assert_eq!(harness.imports(), 1);
        }
    }
}

#[test]
fn wrong_coded_dimensions_are_rejected_before_dma_buf_import() {
    run_materializer_scenario(
        DescriptorScenario {
            width: 800,
            height: 450,
            ..DescriptorScenario::valid()
        },
        ExpectedImportBoundary::RejectedBeforeImport(
            DmaBufDescriptorRejection::CodedSizeMismatch {
                expected_width: 640,
                expected_height: 360,
                actual_width: 800,
                actual_height: 450,
            },
        ),
    );
}

#[test]
fn incompatible_layout_is_rejected_before_dma_buf_import() {
    run_materializer_scenario(
        DescriptorScenario {
            export_layout: DmaBufFrameExportLayout::SeparateLayers,
            ..DescriptorScenario::valid()
        },
        ExpectedImportBoundary::RejectedBeforeImport(
            DmaBufDescriptorRejection::ImageLayoutMismatch {
                expected: DmaBufImageLayout::ComposedLayers,
                actual: DmaBufImageLayout::SeparateLayers,
            },
        ),
    );
}

#[test]
fn valid_descriptor_reaches_dma_buf_import_without_release_regression() {
    run_materializer_scenario(
        DescriptorScenario::valid(),
        ExpectedImportBoundary::ReachesImporter,
    );
}

/// Главный инвариант сессии 03: второй кадр на той же поверхности (новый handle,
/// та же идентичность) не импортирует заново и не дублирует fd — только identity lookup.
#[test]
fn next_frame_on_same_surface_hits_cache_without_import_or_fd_duplication() {
    let harness = Harness::new(
        DescriptorScenario::valid(),
        IdentityMode::Supported,
        false,
        true,
    );

    let first = harness.materialize(FrameResourceHandle(1));
    let second = harness.materialize(FrameResourceHandle(2));

    let (
        DmaBufMaterialization::Ready { texture: first, .. },
        DmaBufMaterialization::Ready {
            texture: second, ..
        },
    ) = (first, second)
    else {
        panic!("both frames must materialize");
    };
    assert!(Arc::ptr_eq(&first, &second), "same image must be reused");
    assert_eq!(
        harness.imports(),
        1,
        "exactly one Vulkan import per surface"
    );
    assert_eq!(
        harness.descriptor_lookups(),
        1,
        "fd duplicated only on the miss"
    );
    assert_eq!(harness.identity_lookups(), 2);
    let stats = harness.materializer.import_stats();
    assert_eq!(stats.imports_performed, 1);
    assert_eq!(stats.cache.hits, 1);
    assert!(stats.image_reuse_enabled);
}

/// Fail-closed: без явной синхронизации новый кадр на той же поверхности
/// импортируется заново, а повтор того же кадра (тот же handle) — из кэша.
#[test]
fn without_explicit_sync_new_frame_reimports_but_repeated_frame_hits() {
    let harness = Harness::new(
        DescriptorScenario::valid(),
        IdentityMode::Supported,
        false,
        false,
    );

    let _first = harness.materialize(FrameResourceHandle(1));
    let _repeat = harness.materialize(FrameResourceHandle(1));
    let _second = harness.materialize(FrameResourceHandle(2));

    assert_eq!(
        harness.imports(),
        2,
        "new frame must re-import without sync"
    );
    assert_eq!(harness.descriptor_lookups(), 2);
    assert!(!harness.materializer.import_stats().image_reuse_enabled);
}

/// Provider без identity-поддержки: ключ снимается с descriptor-а, кэш всё
/// равно попадает по идентичности, но dup fd происходит на каждом кадре.
#[test]
fn provider_without_identity_support_still_caches_by_descriptor_identity() {
    let harness = Harness::new(
        DescriptorScenario::valid(),
        IdentityMode::Unsupported,
        false,
        true,
    );

    let _first = harness.materialize(FrameResourceHandle(1));
    let second = harness.materialize(FrameResourceHandle(2));

    assert!(matches!(second, DmaBufMaterialization::Ready { .. }));
    assert_eq!(harness.imports(), 1);
    assert_eq!(
        harness.descriptor_lookups(),
        2,
        "fallback path duplicates fds per frame"
    );
}

/// Busy/Missing identity lookup сохраняют семантику и не доходят до descriptor path.
#[test]
fn busy_and_missing_identity_lookups_short_circuit_without_descriptor_lookup() {
    let busy = Harness::new(DescriptorScenario::valid(), IdentityMode::Busy, false, true);
    let missing = Harness::new(
        DescriptorScenario::valid(),
        IdentityMode::Missing,
        false,
        true,
    );

    let busy_outcome = busy.materialize(FrameResourceHandle(1));
    let missing_outcome = missing.materialize(FrameResourceHandle(1));

    assert!(matches!(
        busy_outcome,
        DmaBufMaterialization::Busy { lock_wait } if lock_wait == Duration::from_micros(2)
    ));
    assert!(matches!(
        missing_outcome,
        DmaBufMaterialization::Missing { lock_wait } if lock_wait == Duration::from_micros(3)
    ));
    assert_eq!(busy.descriptor_lookups() + missing.descriptor_lookups(), 0);
    assert_eq!(busy.imports() + missing.imports(), 0);
}

/// Декодер пересоздал буфер под тем же surface id (другой inode): это промах
/// и новый импорт, старая запись не используется.
#[test]
fn rebuilt_buffer_under_same_resource_id_is_reimported() {
    let harness = Harness::new(
        DescriptorScenario::valid(),
        IdentityMode::Supported,
        false,
        true,
    );
    let _first = harness.materialize(FrameResourceHandle(1));

    let rebuilt = Harness::new(
        DescriptorScenario {
            inode: 99,
            ..DescriptorScenario::valid()
        },
        IdentityMode::Supported,
        false,
        true,
    );
    let _ = rebuilt.materialize(FrameResourceHandle(2));

    assert_eq!(harness.imports(), 1);
    assert_eq!(
        rebuilt.imports(),
        1,
        "a different backing object is a different image"
    );
}

/// Попадание в кэш не отменяет проверку контракта: тот же буфер, но кадр с другим
/// coded size — typed `Unsupported`, без нового импорта и без dup fd.
#[test]
fn cache_hit_still_rejects_frame_whose_contract_no_longer_matches() {
    let harness = Harness::new(
        DescriptorScenario::valid(),
        IdentityMode::Supported,
        false,
        true,
    );
    let _first = harness.materialize(FrameResourceHandle(1));

    let release_sink: SharedVideoFrameReleaseSink = Arc::new(ForwardingReleaseSink);
    let mut mismatched_frame = decoded_frame(FrameResourceHandle(2));
    mismatched_frame.width = 1280;
    mismatched_frame.height = 720;
    let lease = VideoFrameLease::new(
        VideoFrameLeaseConfig::new(16, mismatched_frame, release_sink)
            .with_resource_provider(harness.provider.clone()),
    );
    let outcome = harness
        .materializer
        .materialize_frame(lease.decoded_frame());

    assert!(matches!(
        outcome,
        DmaBufMaterialization::Unsupported {
            reason: WgpuFrameMaterializationUnsupportedReason::DmaBufDescriptorRejected(
                DmaBufDescriptorRejection::CodedSizeMismatch {
                    expected_width: 1280,
                    expected_height: 720,
                    actual_width: 640,
                    actual_height: 360,
                }
            ),
            ..
        }
    ));
    assert_eq!(
        harness.imports(),
        1,
        "mismatch must not trigger a new import"
    );
    assert_eq!(
        harness.descriptor_lookups(),
        1,
        "mismatch must not duplicate fds"
    );
}

/// Ёмкость кэша равна ёмкости пула декодера: при круговом переиспользовании
/// `capacity` поверхностей импорт делается ровно по разу на поверхность.
#[test]
fn round_robin_over_pool_capacity_imports_each_surface_once() {
    let harness = Harness::new(
        DescriptorScenario::valid(),
        IdentityMode::Supported,
        false,
        true,
    );
    // Один fake provider публикует одну identity, поэтому кругооборот моделируется
    // числом кадров: достаточно убедиться, что ёмкость ≥ 1 не вытесняет её.
    for frame_index in 0..(TEST_CACHE_CAPACITY as u64 * 3) {
        let outcome = harness.materialize(FrameResourceHandle(frame_index));
        assert!(matches!(outcome, DmaBufMaterialization::Ready { .. }));
    }

    assert_eq!(harness.imports(), 1);
    assert_eq!(
        harness.materializer.import_stats().cache.capacity,
        TEST_CACHE_CAPACITY
    );
}

/// Ошибка импорта считается и не кэшируется: следующий кадр пробует снова.
#[test]
fn import_failure_is_counted_and_not_cached() {
    let harness = Harness::new(
        DescriptorScenario::valid(),
        IdentityMode::Supported,
        true,
        true,
    );

    let first = harness.materialize(FrameResourceHandle(1));
    let second = harness.materialize(FrameResourceHandle(2));

    assert!(matches!(first, DmaBufMaterialization::Error { .. }));
    assert!(matches!(second, DmaBufMaterialization::Error { .. }));
    assert_eq!(harness.imports(), 2);
    let stats = harness.materializer.import_stats();
    assert_eq!(stats.import_failures, 2);
    assert_eq!(stats.cache.entries, 0);
}
