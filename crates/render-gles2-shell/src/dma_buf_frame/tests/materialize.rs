//! Порядок и исходы показа аппаратного кадра на подставных провайдере и видеослое.

use std::collections::HashSet;

use gles2_egui::{
    DmaBufMissingRequirement, DmaBufTextureAttached, DmaBufTextureError, ImageRelease,
    ModifierAttribute,
};
use gles2_video::{
    CachedDmaBufFrame, DmaBufBufferIdentity, DmaBufImportOutcome, DmaBufImportReport,
    DmaBufNv12Frame, DmaBufPlaneRole, VideoFrameKey, VideoImportError,
};
use video_core::{DecodedFrame, DmaBufDescriptorRejection, FrameResourceDescriptor};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::support::{
    DESCRIPTOR_LOCK_WAIT, FakeProvider, IDENTITY_LOCK_WAIT, Reply, composed_descriptor, contract,
    frame, p010_descriptor, separate_descriptor,
};
use crate::dma_buf_frame::materialize::{DmaBufShowTarget, materialize_with_target};
use crate::{DmaBufMaterializeOutcome, DmaBufShownFrom, DmaBufUnsupportedReason, Nv12DmaBufError};

/// Подставной видеослой: помнит паспорта импортированных буферов и ключ текущего кадра.
#[derive(Default)]
struct FakeTarget {
    cached: HashSet<DmaBufBufferIdentity>,
    current_key: Option<VideoFrameKey>,
    show_cached_calls: usize,
    imports: Vec<(VideoFrameKey, u64)>,
    import_failure: Option<VideoImportError>,
}

impl DmaBufShowTarget for FakeTarget {
    fn show_cached(
        &mut self,
        passport: &DmaBufBufferIdentity,
        key: VideoFrameKey,
    ) -> CachedDmaBufFrame {
        self.show_cached_calls += 1;
        if self.current_key == Some(key) {
            CachedDmaBufFrame::AlreadyCurrent
        } else if self.cached.contains(passport) {
            self.current_key = Some(key);
            CachedDmaBufFrame::Shown
        } else {
            CachedDmaBufFrame::NotCached
        }
    }

    fn import(
        &mut self,
        frame: &DmaBufNv12Frame<'_>,
    ) -> Result<DmaBufImportOutcome, VideoImportError> {
        if let Some(error) = self.import_failure.clone() {
            return Err(error);
        }
        self.imports.push((frame.key(), frame.buffer().resource_id));
        self.cached.insert(frame.buffer().clone());
        self.current_key = Some(frame.key());
        let attached = DmaBufTextureAttached {
            modifier: ModifierAttribute::DriverChosen,
            image_release: ImageRelease::Released,
        };
        Ok(DmaBufImportOutcome::Imported(DmaBufImportReport {
            luma: attached,
            chroma: attached,
        }))
    }
}

fn composed_provider(resource_id: u64, inode: u64) -> FakeProvider {
    FakeProvider::new(move || {
        FrameResourceDescriptor::DmaBuf(composed_descriptor(resource_id, inode))
    })
}

fn composed_frame(handle: u64) -> DecodedFrame {
    frame(contract(DmaBufImageLayout::ComposedLayers), handle)
}

fn is_fresh_import(outcome: &DmaBufMaterializeOutcome) -> bool {
    matches!(
        outcome,
        DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::FreshImport(_))
    )
}

/// Первый кадр поверхности импортируется (дескриптор взят один раз), второй кадр той же
/// поверхности показывается из кэша БЕЗ запроса дескриптора и без импорта.
#[test]
fn second_frame_of_the_same_surface_hits_the_cache_without_a_descriptor() {
    let (provider, calls) = composed_provider(1, 100).into_handle();
    let mut target = FakeTarget::default();

    let first = materialize_with_target(&mut target, &provider, &composed_frame(1));
    assert!(is_fresh_import(&first.outcome), "{:?}", first.outcome);
    assert_eq!((calls.identity(), calls.descriptor()), (1, 1));

    let second = materialize_with_target(&mut target, &provider, &composed_frame(2));
    assert_eq!(
        second.outcome,
        DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::CacheHit)
    );
    assert_eq!(calls.descriptor(), 1, "cache hit must not duplicate fds");
    assert_eq!(target.imports.len(), 1);
    assert_eq!(target.current_key, Some(VideoFrameKey(2)));
    // Ожидание lock-ов считается по тем lookup-ам, что были.
    assert_eq!(
        first.resource_pool_lock_wait,
        IDENTITY_LOCK_WAIT + DESCRIPTOR_LOCK_WAIT
    );
    assert_eq!(second.resource_pool_lock_wait, IDENTITY_LOCK_WAIT);
}

#[test]
fn the_same_frame_again_is_already_current() {
    let (provider, calls) = composed_provider(1, 100).into_handle();
    let mut target = FakeTarget::default();
    let _ = materialize_with_target(&mut target, &provider, &composed_frame(7));
    let again = materialize_with_target(&mut target, &provider, &composed_frame(7));
    assert_eq!(
        again.outcome,
        DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::AlreadyCurrent)
    );
    assert_eq!(calls.descriptor(), 1);
    assert_eq!(target.imports.len(), 1);
}

/// Провайдер без идентичности: дескриптор нужен каждый раз, но импорт — один (кэш по
/// паспорту, снятому с дескриптора).
#[test]
fn provider_without_identity_still_caches_by_the_descriptor_passport() {
    let mut provider = composed_provider(2, 200);
    provider.identity = None;
    let (provider, calls) = provider.into_handle();
    let mut target = FakeTarget::default();

    let first = materialize_with_target(&mut target, &provider, &composed_frame(1));
    assert!(is_fresh_import(&first.outcome));
    let second = materialize_with_target(&mut target, &provider, &composed_frame(2));
    assert_eq!(
        second.outcome,
        DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::CacheHit)
    );
    assert_eq!(
        calls.descriptor(),
        2,
        "no identity: descriptor on every frame"
    );
    assert_eq!(target.imports.len(), 1, "but only one import");
    // Первый кадр спросил кэш один раз (после дескриптора), второй — тоже один.
    assert_eq!(target.show_cached_calls, 2);
}

/// Пересозданный под тем же номером буфер — промах кэша и новый импорт.
#[test]
fn recreated_buffer_is_imported_again() {
    let (first_provider, _) = composed_provider(3, 300).into_handle();
    let (second_provider, _) = composed_provider(3, 301).into_handle();
    let mut target = FakeTarget::default();
    let _ = materialize_with_target(&mut target, &first_provider, &composed_frame(1));
    let second = materialize_with_target(&mut target, &second_provider, &composed_frame(2));
    assert!(is_fresh_import(&second.outcome));
    assert_eq!(target.imports.len(), 2);
}

#[test]
fn separate_layout_is_imported_with_a_separate_contract() {
    let provider =
        FakeProvider::new(|| FrameResourceDescriptor::DmaBuf(separate_descriptor(4, 400)));
    let (provider, _) = provider.into_handle();
    let mut target = FakeTarget::default();
    let outcome = materialize_with_target(
        &mut target,
        &provider,
        &frame(contract(DmaBufImageLayout::SeparateLayers), 1),
    );
    assert!(is_fresh_import(&outcome.outcome), "{:?}", outcome.outcome);
}

/// Busy/Missing/Fatal провайдера завершают показ без дескриптора и без обращения к видеослою;
/// те же ответы на lookup-е дескриптора дают те же исходы.
#[test]
fn provider_states_short_circuit_with_their_own_outcomes() {
    for (reply, expected) in [
        (Reply::Busy, DmaBufMaterializeOutcome::Busy),
        (Reply::Missing, DmaBufMaterializeOutcome::Missing),
        (Reply::Fatal, DmaBufMaterializeOutcome::ProviderFatal),
    ] {
        let mut provider = composed_provider(5, 500);
        provider.identity = Some(reply);
        let (provider, calls) = provider.into_handle();
        let mut target = FakeTarget::default();
        let result = materialize_with_target(&mut target, &provider, &composed_frame(1));
        assert_eq!(result.outcome, expected);
        assert_eq!(calls.descriptor(), 0, "{expected:?}");
        assert_eq!(target.show_cached_calls, 0);
        assert_eq!(result.resource_pool_lock_wait, IDENTITY_LOCK_WAIT);

        // Идентичности нет, но дескриптор не отдаётся: тот же исход.
        let mut provider = composed_provider(5, 500);
        provider.identity = None;
        provider.descriptor = reply;
        let (provider, calls) = provider.into_handle();
        let result = materialize_with_target(&mut target, &provider, &composed_frame(1));
        assert_eq!(result.outcome, expected);
        assert_eq!(calls.descriptor(), 1);
        assert!(target.imports.is_empty());
    }
}

#[test]
fn host_planar_descriptor_is_unsupported_not_an_error() {
    let provider = FakeProvider::new(|| {
        let host = crate::test_support::host_frame(
            video_frame_contract::VideoFramePixelLayout::Yuv420Planar8,
            [2, 2],
            0,
            [1, 2, 3],
        );
        host.descriptor
    });
    let mut provider = provider;
    provider.identity = None;
    let (provider, _) = provider.into_handle();
    let mut target = FakeTarget::default();
    let result = materialize_with_target(&mut target, &provider, &composed_frame(1));
    assert_eq!(
        result.outcome,
        DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::HostPlanarDescriptor)
    );
    assert!(target.imports.is_empty());
}

/// Контракт и дескриптор расходятся: отказ до видеослоя, импорта нет.
#[test]
fn descriptor_contradicting_the_contract_is_rejected_before_the_video_layer() {
    // Separate-дескриптор при composed-контракте — по дескриптору (идентичности нет).
    let mut provider =
        FakeProvider::new(|| FrameResourceDescriptor::DmaBuf(separate_descriptor(6, 600)));
    provider.identity = None;
    let (provider, _) = provider.into_handle();
    let mut target = FakeTarget::default();
    let result = materialize_with_target(&mut target, &provider, &composed_frame(1));
    assert!(matches!(
        result.outcome,
        DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::DescriptorRejected(
            DmaBufDescriptorRejection::ImageLayoutMismatch { .. }
        ))
    ));
    assert!(target.imports.is_empty());

    // Тот же отказ по идентичности: дескриптор даже не запрашивается, кэш не трогается.
    let (provider, calls) =
        FakeProvider::new(|| FrameResourceDescriptor::DmaBuf(separate_descriptor(6, 600)))
            .into_handle();
    let result = materialize_with_target(&mut target, &provider, &composed_frame(1));
    assert!(matches!(
        result.outcome,
        DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::DescriptorRejected(_))
    ));
    assert_eq!(calls.descriptor(), 0);
    assert_eq!(
        target.show_cached_calls, 0,
        "a rejected frame must never become current"
    );
}

/// Другой размер кадра при том же буфере: отказ, даже если буфер лежит в кэше.
#[test]
fn cached_buffer_is_not_shown_for_a_frame_with_another_size() {
    let (provider, _) = composed_provider(7, 700).into_handle();
    let mut target = FakeTarget::default();
    let _ = materialize_with_target(&mut target, &provider, &composed_frame(1));
    let mut resized = composed_frame(2);
    resized.width += 2;
    let result = materialize_with_target(&mut target, &provider, &resized);
    assert!(matches!(
        result.outcome,
        DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::DescriptorRejected(
            DmaBufDescriptorRejection::CodedSizeMismatch { .. }
        ))
    ));
    assert_eq!(target.current_key, Some(VideoFrameKey(1)));
}

#[test]
fn ten_bit_frames_are_unsupported_by_contract_and_by_descriptor() {
    let mut target = FakeTarget::default();
    // По контракту кадра: провайдер вообще не спрашивается.
    let (provider, calls) = composed_provider(8, 800).into_handle();
    let p010_frame = frame(
        VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
        1,
    );
    let result = materialize_with_target(&mut target, &provider, &p010_frame);
    assert!(matches!(
        result.outcome,
        DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::FrameFormat(
            Nv12DmaBufError::TenBitHardwareNotDeclared { .. }
        ))
    ));
    assert_eq!((calls.identity(), calls.descriptor()), (0, 0));
    // По дескриптору P010 при контракте NV12.
    let (provider, _) =
        FakeProvider::new(|| FrameResourceDescriptor::DmaBuf(p010_descriptor(8, 800)))
            .into_handle();
    let result = materialize_with_target(&mut target, &provider, &composed_frame(1));
    assert!(matches!(
        result.outcome,
        DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::FrameFormat(
            Nv12DmaBufError::TenBitHardwareNotDeclared { .. }
        ))
    ));
    assert!(target.imports.is_empty());
}

/// Драйвер сказал «не умею» — ограничение (Unsupported); любой другой отказ — ImportFailed.
#[test]
fn driver_limitations_are_unsupported_and_other_import_errors_are_failures() {
    let limitations = [
        DmaBufTextureError::ZeroCopyUnsupported {
            missing: DmaBufMissingRequirement::EglExtImageDmaBufImport,
        },
        DmaBufTextureError::ImageTargetFunctionMissing,
    ];
    for source in limitations {
        let error = VideoImportError::Attach {
            plane: DmaBufPlaneRole::Luma,
            source,
        };
        let (provider, _) = composed_provider(9, 900).into_handle();
        let mut target = FakeTarget {
            import_failure: Some(error.clone()),
            ..FakeTarget::default()
        };
        let result = materialize_with_target(&mut target, &provider, &composed_frame(1));
        assert_eq!(
            result.outcome,
            DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::DriverCannotImport(
                error
            ))
        );
    }
    let failures = [
        VideoImportError::Attach {
            plane: DmaBufPlaneRole::Chroma,
            source: DmaBufTextureError::TextureTarget { gl_error: 0x0502 },
        },
        VideoImportError::ForeignContext,
    ];
    for error in failures {
        let (provider, _) = composed_provider(9, 900).into_handle();
        let mut target = FakeTarget {
            import_failure: Some(error.clone()),
            ..FakeTarget::default()
        };
        let result = materialize_with_target(&mut target, &provider, &composed_frame(1));
        assert_eq!(
            result.outcome,
            DmaBufMaterializeOutcome::ImportFailed(error)
        );
        assert_eq!(target.current_key, None, "a failed import shows nothing");
    }
}
