//! Контракт трубы субтитров на уровне её публичных границ (без session).

use std::time::Duration;

use bytes::Bytes;
use media_core::{MediaAttachment, MediaAttachments, TrackId};

use super::*;
use crate::MediaInstanceId;

fn capacity(messages: usize) -> SubtitleFeedCapacity {
    SubtitleFeedCapacity::from_messages(messages).expect("test capacity is non-zero")
}

fn packet(instance: MediaInstanceId, pts_ms: u64) -> SubtitleFeedPacket {
    SubtitleFeedPacket {
        media_instance_id: instance,
        track_id: TrackId::new(3),
        presentation_time: Duration::from_millis(pts_ms),
        duration: Some(Duration::from_millis(500)),
        payload: Bytes::from_static(b"cue"),
    }
}

#[test]
fn zero_capacity_is_rejected_and_nonzero_roundtrips() {
    assert_eq!(SubtitleFeedCapacity::from_messages(0), None);
    assert_eq!(capacity(7).messages(), 7);
    assert_eq!(SubtitleFeedCapacity::FALLBACK.messages(), 4_096);
}

#[test]
fn messages_arrive_in_order_and_drain_respects_the_limit() {
    let (sender, receiver) = subtitle_feed(capacity(8));
    let instance = MediaInstanceId::new_unique();
    for pts_ms in [10, 20, 30] {
        assert_eq!(
            sender.try_send_packet(packet(instance, pts_ms)),
            SubtitleFeedSendOutcome::Delivered
        );
    }

    let mut drained = Vec::new();
    assert_eq!(receiver.drain_into(&mut drained, 2), 2);
    assert_eq!(receiver.drain_into(&mut drained, 10), 1);
    assert_eq!(receiver.drain_into(&mut drained, 10), 0);

    let times: Vec<_> = drained
        .iter()
        .map(|message| match message {
            SubtitleFeedMessage::Packet(packet) => packet.presentation_time,
            SubtitleFeedMessage::MediaStarted(_) => panic!("only packets were sent"),
        })
        .collect();
    assert_eq!(
        times,
        [10, 20, 30].map(Duration::from_millis),
        "порядок отправки сохраняется"
    );
}

#[test]
fn full_queue_drops_packets_counts_them_and_never_blocks() {
    let (sender, receiver) = subtitle_feed(capacity(2));
    let instance = MediaInstanceId::new_unique();

    let outcomes: Vec<_> = (0..5)
        .map(|index| sender.try_send_packet(packet(instance, index)))
        .collect();

    assert_eq!(
        outcomes,
        [
            SubtitleFeedSendOutcome::Delivered,
            SubtitleFeedSendOutcome::Delivered,
            SubtitleFeedSendOutcome::DroppedFull,
            SubtitleFeedSendOutcome::DroppedFull,
            SubtitleFeedSendOutcome::DroppedFull,
        ]
    );
    assert_eq!(receiver.dropped_packets(), 3);
    assert_eq!(
        receiver.dropped_media_starts(),
        0,
        "потеря пакетов не засчитывается как потеря media start"
    );
}

#[test]
fn dropped_media_start_is_counted_separately_from_packets() {
    let (sender, receiver) = subtitle_feed(capacity(1));
    let instance = MediaInstanceId::new_unique();
    assert_eq!(
        sender.try_send_packet(packet(instance, 0)),
        SubtitleFeedSendOutcome::Delivered
    );

    let outcome = sender.try_send_media_start(SubtitleFeedMediaStart {
        media_instance_id: instance,
        attachments: MediaAttachments::default(),
        track_headers: Vec::new(),
    });

    assert_eq!(outcome, SubtitleFeedSendOutcome::DroppedFull);
    assert_eq!(receiver.dropped_media_starts(), 1);
    assert_eq!(receiver.dropped_packets(), 0);
}

#[test]
fn media_start_carries_attachments_intact() {
    let (sender, receiver) = subtitle_feed(capacity(4));
    let instance = MediaInstanceId::new_unique();
    let attachments = MediaAttachments {
        attachments: vec![MediaAttachment {
            file_name: "font.ttf".to_owned(),
            media_type: Some("font/ttf".to_owned()),
            bytes: Bytes::from_static(b"font-bytes"),
        }],
        skipped: Vec::new(),
    };

    let track_headers = vec![SubtitleTrackHeader {
        track_id: TrackId::new(3),
        codec_private: Bytes::from_static(b"[Script Info]"),
    }];

    sender.try_send_media_start(SubtitleFeedMediaStart {
        media_instance_id: instance,
        attachments: attachments.clone(),
        track_headers: track_headers.clone(),
    });

    let mut drained = Vec::new();
    receiver.drain_into(&mut drained, 4);
    assert_eq!(
        drained,
        [SubtitleFeedMessage::MediaStarted(SubtitleFeedMediaStart {
            media_instance_id: instance,
            attachments,
            track_headers
        })]
    );
}

#[test]
fn dropped_receiver_makes_sends_a_silent_noop() {
    let (sender, receiver) = subtitle_feed(capacity(2));
    drop(receiver);
    let instance = MediaInstanceId::new_unique();

    assert_eq!(
        sender.try_send_packet(packet(instance, 0)),
        SubtitleFeedSendOutcome::ReceiverGone
    );
    assert_eq!(
        sender.try_send_media_start(SubtitleFeedMediaStart {
            media_instance_id: instance,
            attachments: MediaAttachments::default(),
            track_headers: Vec::new(),
        }),
        SubtitleFeedSendOutcome::ReceiverGone
    );
}

#[test]
fn disabled_sender_reports_receiver_gone() {
    let sender = SubtitleFeedSender::disabled();
    assert_eq!(
        sender.try_send_packet(packet(MediaInstanceId::new_unique(), 0)),
        SubtitleFeedSendOutcome::ReceiverGone
    );
}
