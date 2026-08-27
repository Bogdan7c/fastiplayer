// Cancellation AES media/key проверяется на реальном loopback body, а не на timer-only fake.

use super::*;

const KEY_A: [u8; 16] = *b"0123456789abcdef";
const KEY_B: [u8; 16] = *b"fedcba9876543210";

fn encrypted_request(server: &TestServer, rotate_target_key: bool) -> HlsVodOpenRequest {
    let mut request = discontinuous_post_target_request(server);
    let selected_url = server.target("/transient-manifest-retry.m3u8");
    let segment_lines = (0..SEGMENT_COUNT)
        .map(|segment_index| {
            let discontinuity = if segment_index == TARGET_SEGMENT_INDEX {
                "#EXT-X-DISCONTINUITY\n"
            } else {
                ""
            };
            let rotation = if rotate_target_key && segment_index == TARGET_SEGMENT_INDEX {
                "#EXT-X-KEY:METHOD=AES-128,URI=\"key-b.bin\"\n"
            } else {
                ""
            };
            format!(
                "{discontinuity}{rotation}#EXTINF:{SEGMENT_SECONDS},\nsegment-{segment_index}.ts\n"
            )
        })
        .collect::<String>();
    let playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:{SEGMENT_SECONDS}\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"key-a.bin\"\n\
         {segment_lines}#EXT-X-ENDLIST\n"
    );
    request.manifest = HlsManifestInput::InlineMedia {
        selected_url: selected_url.clone(),
        playlist: SecretInlineMediaPlaylist::new(&playlist),
    };
    request.http = adaptive_context_without_completed_cache(
        &selected_url,
        CancellationToken::new(),
        SourceGeneration::new(1),
        TestQueries::default(),
    );
    request
}

fn encrypted_segments() -> Arc<Vec<Vec<u8>>> {
    Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                let key = if segment_index == TARGET_SEGMENT_INDEX {
                    KEY_B
                } else {
                    KEY_A
                };
                encrypt_pkcs7(
                    &long_muxed_ts_segment(
                        segment_index * SEGMENT_SECONDS * 90_000,
                        SEGMENT_SECONDS,
                    ),
                    key,
                    sequence_iv(segment_index),
                )
            })
            .collect(),
    )
}

fn prove_target_and_finish(demuxer: &mut dyn Demuxer) {
    let mut saw_target_video = false;
    loop {
        match next_ready_event(demuxer).expect("encrypted initial playback") {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video
                    && packet.pts >= Duration::from_secs(TARGET_SEGMENT_INDEX * SEGMENT_SECONDS) =>
            {
                saw_target_video = true;
            }
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::Packet(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    }
    assert!(saw_target_video, "initial playback обязан доказать target RAP");
}

#[test]
fn final_receipt_cancels_partial_ciphertext_before_decrypt_and_stale_publication() {
    let segments = encrypted_segments();
    let partial_plaintext = long_muxed_ts_segment_without_rap(90_000_000, 2);
    let partial_ciphertext = Arc::new(encrypt_pkcs7(
        &partial_plaintext,
        KEY_B,
        sequence_iv(TARGET_SEGMENT_INDEX),
    ));
    let target_attempts = Arc::new(AtomicUsize::new(0));
    let (prefix_sender, prefix_receiver) = mpsc::sync_channel(1);
    let (disconnected_sender, disconnected_receiver) = mpsc::sync_channel(1);
    let (final_sender, final_receiver) = mpsc::sync_channel(1);
    let server_segments = Arc::clone(&segments);
    let server_partial = Arc::clone(&partial_ciphertext);
    let server_attempts = Arc::clone(&target_attempts);
    let server = TestServer::start_streaming(move |_, request, stream| {
        if request.request_line.contains("/key-a.bin") {
            stream.write_all(&response("200 OK", &[], &KEY_A)).expect("key A");
            return;
        }
        if request.request_line.contains("/key-b.bin") {
            stream.write_all(&response("200 OK", &[], &KEY_B)).expect("key B");
            return;
        }
        if request.request_line.contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts")) {
            let attempt = server_attempts.fetch_add(1, Ordering::AcqRel);
            if attempt == 1 {
                let declared = server_partial.len() + 16;
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n").expect("ciphertext headers");
                stream.write_all(&server_partial).expect("ciphertext prefix");
                stream.flush().expect("flush ciphertext prefix");
                prefix_sender.send(()).expect("publish ciphertext prefix");
                observe_peer_disconnect(stream, &disconnected_sender);
                return;
            }
        }
        if request.request_line.contains(&format!("/segment-{SUPERSEDING_SEGMENT_INDEX}.ts")) {
            final_sender.send(()).expect("publish final segment start");
        }
        let body = (0..SEGMENT_COUNT)
            .find(|index| request.request_line.contains(&format!("/segment-{index}.ts")))
            .map(|index| server_segments[index as usize].as_slice())
            .unwrap_or_default();
        stream.write_all(&response("200 OK", &[], body)).expect("encrypted segment");
    });
    let (seek_handle, mut demuxer) = prepared_worker_from_request(encrypted_request(&server, true));
    prove_target_and_finish(&mut *demuxer);
    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(75)))
        .expect("encrypted preview anchor");
    wait_for_partial_body(&mut *demuxer, &prefix_receiver);
    let fence = enqueue_superseding_seek(&seek_handle);
    disconnected_receiver.recv_timeout(TEST_TIMEOUT).expect("ciphertext TCP close");
    final_receiver.recv_timeout(TEST_TIMEOUT).expect("final starts before release");
    assert_successful_final(&seek_handle, fence, &mut *demuxer);
    assert_eq!(target_attempts.load(Ordering::Acquire), 2);
}

#[test]
fn final_receipt_cancels_partial_rotated_key_without_caching_stale_key() {
    let segments = encrypted_segments();
    let key_b_attempts = Arc::new(AtomicUsize::new(0));
    let target_attempts = Arc::new(AtomicUsize::new(0));
    let (prefix_sender, prefix_receiver) = mpsc::sync_channel(1);
    let (disconnected_sender, disconnected_receiver) = mpsc::sync_channel(1);
    let (final_sender, final_receiver) = mpsc::sync_channel(1);
    let server_segments = Arc::clone(&segments);
    let server_key_attempts = Arc::clone(&key_b_attempts);
    let server_target_attempts = Arc::clone(&target_attempts);
    let server = TestServer::start_streaming(move |_, request, stream| {
        if request.request_line.contains("/key-a.bin") {
            stream.write_all(&response("200 OK", &[], &KEY_A)).expect("key A");
            return;
        }
        if request.request_line.contains("/key-b.bin") {
            let attempt = server_key_attempts.fetch_add(1, Ordering::AcqRel);
            if attempt == 1 {
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n").expect("key headers");
                stream.write_all(&KEY_B[..8]).expect("partial key");
                stream.flush().expect("flush partial key");
                prefix_sender.send(()).expect("publish key prefix");
                observe_peer_disconnect(stream, &disconnected_sender);
                return;
            }
            stream.write_all(&response("200 OK", &[], &KEY_B)).expect("key B");
            return;
        }
        if request.request_line.contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts")) {
            server_target_attempts.fetch_add(1, Ordering::AcqRel);
        }
        if request.request_line.contains(&format!("/segment-{SUPERSEDING_SEGMENT_INDEX}.ts")) {
            final_sender.send(()).expect("publish final segment start");
        }
        let body = (0..SEGMENT_COUNT)
            .find(|index| request.request_line.contains(&format!("/segment-{index}.ts")))
            .map(|index| server_segments[index as usize].as_slice())
            .unwrap_or_default();
        stream.write_all(&response("200 OK", &[], body)).expect("encrypted segment");
    });
    let (seek_handle, mut demuxer) = prepared_worker_from_request(encrypted_request(&server, true));
    prove_target_and_finish(&mut *demuxer);
    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(75)))
        .expect("rotated-key preview anchor");
    wait_for_partial_body(&mut *demuxer, &prefix_receiver);
    let fence = enqueue_superseding_seek(&seek_handle);
    disconnected_receiver.recv_timeout(TEST_TIMEOUT).expect("key TCP close");
    final_receiver.recv_timeout(TEST_TIMEOUT).expect("final starts before key release");
    assert_successful_final(&seek_handle, fence, &mut *demuxer);
    assert_eq!(key_b_attempts.load(Ordering::Acquire), 2, "partial key нельзя cache-ить");
    assert_eq!(
        target_attempts.load(Ordering::Acquire),
        2,
        "cancelled key phase не должна повторно рестартовать уже дочитанный ciphertext"
    );
}

fn observe_peer_disconnect(stream: &mut std::net::TcpStream, sender: &mpsc::SyncSender<()>) {
    stream.set_read_timeout(Some(Duration::from_millis(100))).expect("disconnect timeout");
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut probe = [0_u8; 1];
    loop {
        match stream.peek(&mut probe) {
            Ok(0) => {
                sender.send(()).expect("publish disconnect");
                return;
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                sender.send(()).expect("publish disconnect error");
                return;
            }
        }
        assert!(Instant::now() < deadline, "cancelled AES response remained connected");
    }
}

fn wait_for_partial_body(demuxer: &mut dyn Demuxer, receiver: &mpsc::Receiver<()>) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if receiver.try_recv().is_ok() {
            return;
        }
        assert!(matches!(
            demuxer.next_event(),
            Ok(DemuxReadEvent::TemporarilyUnavailable(_))
                | Ok(DemuxReadEvent::TracksChanged(_))
                | Ok(DemuxReadEvent::MediaMetadataChanged(_))
        ));
        assert!(Instant::now() < deadline, "AES partial body did not start");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn enqueue_superseding_seek(handle: &demux_api::ProgressiveAsyncSeekHandle) -> ProgressiveSeekFence {
    let fence = ProgressiveSeekFence {
        runtime_generation: handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    handle
        .enqueue(
            fence,
            DemuxSeekRequest::decode_point_before(Duration::from_secs(35)),
        )
        .expect("enqueue final AES seek");
    fence
}

fn assert_successful_final(
    handle: &demux_api::ProgressiveAsyncSeekHandle,
    fence: ProgressiveSeekFence,
    demuxer: &mut dyn Demuxer,
) {
    let receipt = wait_for_receipt(handle);
    assert_eq!(receipt.fence, fence);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("final AES seek failed: {receipt:?}");
    };
    assert_eq!(result.actual_position.as_duration(), Duration::from_secs(40));
    assert_eq!(first_post_seek_video_packet(demuxer).pts, Duration::from_secs(40));
}
