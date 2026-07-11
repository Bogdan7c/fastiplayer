use super::*;

/// Проверяет fail-closed policy на локально poisoned pool без hardware/global state.
#[test]
fn poisoned_resource_pool_returns_typed_fatal_error_on_every_lock_attempt() {
    let resource_pool = Mutex::new(FrameResourcePool::default());

    std::thread::scope(|scope| {
        let poison_thread = scope.spawn(|| {
            let _resource_pool_guard = resource_pool
                .lock()
                .expect("fresh test mutex must be lockable before deliberate poison");
            panic!("deliberately poison local resource-pool mutex");
        });
        assert!(poison_thread.join().is_err());
    });

    for operation in ["first test access", "cleanup test access"] {
        let error = match lock_resource_pool(&resource_pool, operation) {
            Ok(_) => panic!("poisoned resource pool must never be recovered"),
            Err(error) => error,
        };
        let poison_error = error
            .downcast_ref::<VaapiResourcePoolPoisonError>()
            .expect("resource-pool poison must preserve its typed root cause");

        assert_eq!(poison_error.operation, operation);
        assert!(is_fatal_decoder_error(&error));
    }
}

/// Проверяет discard helper: flush tail frame синхронизируется и не попадает в reclaim queue.
#[test]
fn sync_discard_ready_frame_syncs_and_returns_handle_to_pool() {
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let initial_free_frames = frame_pool.num_free();
    let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));

    sync_discard_ready_frame(&mut frame_pool, handle, "flush")
        .expect("flush discard должен sync-нуть ready tail frame");

    assert!(
        sync_called.get(),
        "discard path обязан вызвать blocking sync перед release"
    );
    assert_eq!(
        frame_pool.num_free(),
        initial_free_frames + 1,
        "discard path должен вернуть backing frame в pool"
    );
}

/// Проверяет overflow fallback: sync вызывается только для oldest handle.
#[test]
fn overflow_forces_sync_of_oldest_only_as_fallback() {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let (oldest_handle, oldest_sync_called) =
        fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
    let (incoming_handle, incoming_sync_called) =
        fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));

    enqueue_suppressed_frame_for_reclaim_in_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(1),
        oldest_handle,
        "overflow_test_first",
        PrerollFallbackCandidateMetadata {
            pts: Duration::from_millis(1),
            generation: 1,
        },
    )
    .expect("первый enqueue должен пройти без overflow");
    enqueue_suppressed_frame_for_reclaim_in_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(1),
        incoming_handle,
        "overflow_test_second",
        PrerollFallbackCandidateMetadata {
            pts: Duration::from_millis(2),
            generation: 2,
        },
    )
    .expect("overflow enqueue должен освободить oldest и принять incoming");

    assert_eq!(suppressed_reclaim_queue.len(), 1);
    assert!(
        oldest_sync_called.get(),
        "overflow должен forced-sync только oldest handle"
    );
    assert!(
        !incoming_sync_called.get(),
        "incoming handle нельзя sync-ать как обычную per-frame policy"
    );
    assert_eq!(suppressed_reclaim_counters.ring_full_count, 1);
    assert_eq!(suppressed_reclaim_counters.forced_sync_count, 1);
    assert_eq!(suppressed_reclaim_counters.total_enqueued, 2);
    assert_eq!(suppressed_reclaim_counters.total_reclaimed, 1);
}

/// Проверяет, что VA 10-bit 4:2:0 surface становится P010 boundary contract.
#[test]
fn va_yuv420_10_rt_format_maps_to_p010_decoded_contract() {
    let contract = decoded_contract_for_rt_format(VA_RT_FORMAT_YUV420_10).unwrap();

    assert_eq!(contract.format, DecodedPixelFormat::P010);
    assert_eq!(contract.bit_depth, BitDepth::Ten);
    assert_eq!(contract.chroma, ChromaSubsampling::Yuv420);
}

/// Проверяет, что выбранный frame contract прямо задаёт VA export layout.
#[test]
fn frame_contract_maps_to_preferred_dma_buf_export_layout() {
    assert_eq!(
        dma_buf_export_layout_from_frame_contract(VideoFrameContract::dma_buf_nv12(
            DmaBufImageLayout::ComposedLayers
        ))
        .unwrap(),
        DecodedDmaBufExportLayout::ComposedLayers
    );
    assert_eq!(
        dma_buf_export_layout_from_frame_contract(VideoFrameContract::dma_buf_p010(
            DmaBufImageLayout::SeparateLayers
        ))
        .unwrap(),
        DecodedDmaBufExportLayout::SeparateLayers
    );
}

/// Проверяет, что VAAPI decoder не принимает non-DMA-BUF contract для export-а.
#[test]
fn non_dma_buf_frame_contract_is_rejected_for_vaapi_export_layout() {
    let error =
        dma_buf_export_layout_from_frame_contract(VideoFrameContract::host_yuv420_planar8())
            .unwrap_err();

    assert!(
        error.to_string().contains("DMA-BUF"),
        "unexpected error: {error}"
    );
}

/// Проверяет `cros-codecs` I010 alias, который VA-API отдаёт для P010 FourCC.
#[test]
fn i010_stream_format_maps_to_p010_decoded_contract() {
    let contract = decoded_contract_for_stream_format(VaapiDecodedFormat::I010).unwrap();

    assert_eq!(contract.format, DecodedPixelFormat::P010);
    assert_eq!(contract.bit_depth, BitDepth::Ten);
    assert_eq!(contract.chroma, ChromaSubsampling::Yuv420);
}

/// Проверяет, что 12-bit VA format не маскируется под P010.
#[test]
fn va_yuv420_12_rt_format_is_not_p010_contract() {
    let error = decoded_contract_for_rt_format(VA_RT_FORMAT_YUV420_12).unwrap_err();

    assert!(
        error.to_string().contains("Unsupported VA RT format"),
        "unexpected error: {error}"
    );
}
