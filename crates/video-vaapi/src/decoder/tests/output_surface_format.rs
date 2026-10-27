//! Граница «stream info adapter-а → пул выходных surfaces» на fake пуле без железа.

use super::*;

use crate::codec_adapter::{VaapiAdapterStreamInfo, VaapiResolution};
use crate::decoder::config::OUTPUT_SURFACE_EXTERNAL_HOLDERS_HEADROOM_FRAMES;
use crate::decoder::output_surface_format::{
    OutputSurfaceStreamTargetError, follow_output_surface_capacity,
    output_surface_target_for_stream, reconfigure_output_surface_pool_for_stream_info,
};
use crate::output_surface_pool::test_support::{lease, pool_with_capacity};
use crate::output_surface_pool::{OutputSurfaceLeaseOutcome, StreamSurfaceMinimum};
use crate::zero_copy_surface_pool::capacity::ZeroCopyCapacityChange;

/// Stream info 4K P010 с crop до 2160 строк: coded и display различаются намеренно.
fn stream_info_4k_p010(min_num_frames: usize) -> VaapiAdapterStreamInfo {
    VaapiAdapterStreamInfo {
        format: VaapiDecodedFormat::I010,
        coded_resolution: VaapiResolution {
            width: 3840,
            height: 2176,
        },
        display_resolution: VaapiResolution {
            width: 3840,
            height: 2160,
        },
        min_num_frames,
    }
}

#[test]
fn target_uses_coded_resolution_and_adds_external_holders_to_codec_minimum() {
    let target = output_surface_target_for_stream(Some(&stream_info_4k_p010(9)))
        .expect("I010 maps to a VA RT format");

    assert_eq!(target.format.coded_resolution.width, 3840);
    assert_eq!(
        target.format.coded_resolution.height, 2176,
        "surfaces are allocated for the coded size, not the cropped display size"
    );
    assert_eq!(target.format.rt_format, VA_RT_FORMAT_YUV420_10);
    assert_eq!(target.codec_minimum_frames, 9);
    assert_eq!(
        target.minimum,
        StreamSurfaceMinimum::Frames(
            NonZeroUsize::new(9 + OUTPUT_SURFACE_EXTERNAL_HOLDERS_HEADROOM_FRAMES)
                .expect("non-zero")
        ),
        "the stream minimum must cover frames held by player and renderer"
    );
}

#[test]
fn unknown_codec_minimum_leaves_only_the_user_setting() {
    let target =
        output_surface_target_for_stream(Some(&stream_info_4k_p010(0))).expect("format still maps");

    assert_eq!(target.minimum, StreamSurfaceMinimum::Unknown);
    assert_eq!(target.codec_minimum_frames, 0);
}

#[test]
fn missing_stream_info_is_an_error_not_a_silent_skip() {
    let error = output_surface_target_for_stream(None).expect_err("no stream info");

    assert_eq!(error, OutputSurfaceStreamTargetError::MissingStreamInfo);
}

#[test]
fn unmappable_decoded_format_is_reported_with_the_format() {
    let mut stream_info = stream_info_4k_p010(9);
    stream_info.format = VaapiDecodedFormat::Mm21;

    let error = output_surface_target_for_stream(Some(&stream_info)).expect_err("MM21 is tiled");

    match error {
        OutputSurfaceStreamTargetError::UnsupportedDecodedFormat {
            decoded_format,
            reason,
        } => {
            assert_eq!(decoded_format, VaapiDecodedFormat::Mm21);
            assert!(reason.contains("Mm21"), "reason names the format: {reason}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn reconfigure_switches_pool_format_and_raises_capacity_to_the_stream_minimum() {
    let fixture = pool_with_capacity(4);
    drop(lease(&fixture.pool));
    assert_eq!(
        fixture.pool.stats().free_surfaces,
        1,
        "one 1080p surface pooled"
    );

    let report = reconfigure_output_surface_pool_for_stream_info(
        &fixture.pool,
        Some(&stream_info_4k_p010(9)),
        4,
    )
    .expect("valid stream info");

    assert!(report.format_changed);
    assert!(report.raised_to_stream_minimum);
    assert_eq!(
        report.capacity.get(),
        9 + OUTPUT_SURFACE_EXTERNAL_HOLDERS_HEADROOM_FRAMES
    );
    assert_eq!(
        report.destroyed_free_surfaces, 1,
        "old-format free surface is gone"
    );
    let stats = fixture.pool.stats();
    assert_eq!(stats.free_surfaces, 0);
    assert_eq!(stats.capacity, report.capacity.get());
    let new_lease = lease(&fixture.pool);
    assert_eq!(new_lease.format().rt_format, VA_RT_FORMAT_YUV420_10);
    assert_eq!(new_lease.format().coded_resolution.height, 2176);
}

#[test]
fn reconfigure_keeps_the_user_setting_when_it_already_covers_the_stream() {
    let fixture = pool_with_capacity(24);

    let report = reconfigure_output_surface_pool_for_stream_info(
        &fixture.pool,
        Some(&stream_info_4k_p010(9)),
        24,
    )
    .expect("valid stream info");

    assert!(!report.raised_to_stream_minimum);
    assert_eq!(report.capacity.get(), 24);
}

#[test]
fn failed_translation_leaves_the_pool_untouched() {
    let fixture = pool_with_capacity(4);
    let held = lease(&fixture.pool);
    drop(lease(&fixture.pool));
    let before = fixture.pool.stats();
    let format_before = fixture.pool.format();
    let mut unmappable = stream_info_4k_p010(9);
    unmappable.format = VaapiDecodedFormat::Mm21;

    let error =
        reconfigure_output_surface_pool_for_stream_info(&fixture.pool, Some(&unmappable), 4)
            .expect_err("MM21 cannot be pooled");
    let missing = reconfigure_output_surface_pool_for_stream_info(&fixture.pool, None, 4)
        .expect_err("missing stream info");

    assert!(matches!(
        error,
        OutputSurfaceStreamTargetError::UnsupportedDecodedFormat { .. }
    ));
    assert_eq!(missing, OutputSurfaceStreamTargetError::MissingStreamInfo);
    assert_eq!(fixture.pool.stats(), before, "no counters moved");
    assert_eq!(
        fixture.pool.format(),
        format_before,
        "format epoch untouched"
    );
    assert!(fixture.destroyed.borrow().is_empty(), "nothing destroyed");
    drop(held);
    assert!(
        matches!(
            fixture.pool.try_lease(),
            OutputSurfaceLeaseOutcome::Leased(_)
        ),
        "returned surface is still reusable under the old format"
    );
}

#[test]
fn translation_failure_wrapped_by_the_decoder_is_a_fatal_lifecycle_error() {
    let error = anyhow::Error::new(VaapiSurfaceLifecycleError::new(format!(
        "cannot reconfigure the output surface pool after FormatChanged: {}",
        OutputSurfaceStreamTargetError::MissingStreamInfo
    )));

    assert!(
        is_fatal_decoder_error(&error),
        "the decoder must stop instead of decoding into old-format surfaces"
    );
}

/// Synthetic DMA-BUF image для регистрации в resource pool без железа.
fn dma_buf_image_for_surface(surface_id: u64) -> cros_codecs::decoder::DecodedDmaBufImage {
    use cros_codecs::decoder::{
        DecodedDmaBufExportLayout, DecodedDmaBufLayer, DecodedDmaBufObject,
    };
    cros_codecs::decoder::DecodedDmaBufImage {
        surface_id,
        fourcc: 0x3231_564e,
        export_layout: DecodedDmaBufExportLayout::ComposedLayers,
        width: 1280,
        height: 720,
        objects: vec![DecodedDmaBufObject {
            fd: std::fs::File::open("/dev/null")
                .expect("/dev/null opens")
                .into(),
            size: 4096,
            drm_format_modifier: 0,
        }],
        layers: vec![DecodedDmaBufLayer {
            drm_format: 0x3231_564e,
            num_planes: 2,
            object_index: [0, 0, 0, 0],
            offset: [0, 2048, 0, 0],
            pitch: [1280, 1280, 0, 0],
        }],
    }
}

/// Регрессия T410: настройка 12, минимум стрима поднимает пул surfaces до 14 —
/// все 14 различных surfaces обязаны зарегистрироваться (раньше `PoolExhausted`).
#[test]
fn raised_output_pool_lets_every_live_surface_register_in_import_slots() {
    use crate::resource_pool::FrameResourcePool;

    let configured_slots = 12;
    let fixture = pool_with_capacity(configured_slots);
    let mut resource_pool = FrameResourcePool::new_with_capacity(configured_slots);
    let report = reconfigure_output_surface_pool_for_stream_info(
        &fixture.pool,
        Some(&stream_info_4k_p010(9)),
        configured_slots,
    )
    .expect("valid stream info");
    assert_eq!(report.capacity.get(), 14, "9 codec + 5 external holders");

    let change = follow_output_surface_capacity(&mut resource_pool, report.capacity);

    assert_eq!(
        change,
        ZeroCopyCapacityChange::Raised {
            previous: 12,
            current: 14
        }
    );
    for surface_id in 0..14 {
        resource_pool
            .register_dma_buf_image(dma_buf_image_for_surface(surface_id))
            .unwrap_or_else(|error| panic!("surface {surface_id} must register: {error:#}"));
    }
    assert_eq!(resource_pool.stats().capacity, 14);
    assert_eq!(resource_pool.num_in_use(), 14);
}

/// Настоящее переполнение (больше держателей, чем surfaces пула) остаётся typed ошибкой.
#[test]
fn genuine_over_capacity_is_still_a_typed_exhaustion_error() {
    use crate::resource_pool::FrameResourcePool;
    use crate::zero_copy_surface_pool::ZeroCopySurfacePoolError;

    let mut resource_pool = FrameResourcePool::new_with_capacity(12);
    follow_output_surface_capacity(&mut resource_pool, NonZeroUsize::new(14).expect("non-zero"));
    for surface_id in 0..14 {
        resource_pool
            .register_dma_buf_image(dma_buf_image_for_surface(surface_id))
            .expect("within capacity");
    }

    let error = resource_pool
        .register_dma_buf_image(dma_buf_image_for_surface(14))
        .expect_err("15th live surface exceeds the pool");

    assert_eq!(
        error.downcast_ref::<ZeroCopySurfacePoolError>(),
        Some(&ZeroCopySurfacePoolError::PoolExhausted {
            capacity: 14,
            slots: 14
        })
    );
}

/// Повторная перенастройка (смена формата) не уменьшает слоты и не трогает занятые.
#[test]
fn format_change_keeps_import_slots_at_least_pool_capacity_and_never_shrinks() {
    use crate::resource_pool::FrameResourcePool;

    let mut resource_pool = FrameResourcePool::new_with_capacity(12);
    let handle = resource_pool
        .register_dma_buf_image(dma_buf_image_for_surface(1))
        .expect("registers");
    follow_output_surface_capacity(&mut resource_pool, NonZeroUsize::new(14).expect("non-zero"));

    let smaller =
        follow_output_surface_capacity(&mut resource_pool, NonZeroUsize::new(8).expect("non-zero"));

    assert_eq!(smaller, ZeroCopyCapacityChange::Unchanged { capacity: 14 });
    let stats = resource_pool.stats();
    assert_eq!((stats.capacity, stats.in_use), (14, 1));
    assert!(resource_pool.is_registered_handle(handle));
    let bigger = follow_output_surface_capacity(
        &mut resource_pool,
        NonZeroUsize::new(20).expect("non-zero"),
    );
    assert_eq!(
        bigger,
        ZeroCopyCapacityChange::Raised {
            previous: 14,
            current: 20
        }
    );
}
