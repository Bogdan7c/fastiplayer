//! Тесты приёмной стороны полосы: когда начинается новый эпизод, а когда нет.

use std::sync::Arc;

use super::{LaneId, LaneIntake, LaneMailbox, MailboxTake};
use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::intake::IntakeLedger;
use crate::policy::PushedSourcePolicy;
use crate::stream::pushed_demux_registry;
use crate::time::TimestampOffset;

const VIDEO_MP4: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/video.mp4");
const INIT_END: usize = 733;
const FIRST_FRAGMENT_END: usize = 5714;
const LANE: LaneId = LaneId::new(0);

struct Fixture {
    intake: LaneIntake,
    mailbox: Arc<LaneMailbox>,
    episodes: EpisodeFactory,
}

fn fixture() -> Fixture {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let mailbox = Arc::new(LaneMailbox::default());
    let episodes = EpisodeFactory::new(
        Arc::new(pushed_demux_registry().expect("registry")),
        Arc::new(IntakeLedger::new(1 << 20)),
        policy,
    );
    let intake = LaneIntake::new(
        LANE,
        PushedContainer::FragmentedMp4,
        policy.framing_limits(),
        policy.max_pending_episodes_per_lane(),
        Arc::clone(&mailbox),
    );
    Fixture {
        intake,
        mailbox,
        episodes,
    }
}

/// Сколько эпизодов лежит в ящике.
fn posted_episodes(mailbox: &LaneMailbox) -> usize {
    let mut count = 0;
    while let MailboxTake::Episode(_) = mailbox.take_next() {
        count += 1;
    }
    count
}

#[test]
fn identical_repeated_init_keeps_the_running_episode() {
    let mut lane = fixture();
    lane.intake
        .append(&VIDEO_MP4[..FIRST_FRAGMENT_END], &lane.episodes)
        .expect("first");
    // Плеер страницы повторяет тот же init перед следующим фрагментом.
    lane.intake
        .append(&VIDEO_MP4[..INIT_END], &lane.episodes)
        .expect("same init");
    lane.intake
        .append(&VIDEO_MP4[FIRST_FRAGMENT_END..], &lane.episodes)
        .expect("second");
    assert_eq!(posted_episodes(&lane.mailbox), 1);
}

#[test]
fn offset_change_and_change_type_start_new_episodes_at_the_next_media_segment() {
    let mut lane = fixture();
    lane.intake
        .append(&VIDEO_MP4[..FIRST_FRAGMENT_END], &lane.episodes)
        .expect("first");
    lane.intake
        .set_timestamp_offset(TimestampOffset::from_micros(1_000_000))
        .expect("offset");
    // Тот же offset ещё раз — не повод для нового эпизода.
    lane.intake
        .set_timestamp_offset(TimestampOffset::from_micros(1_000_000))
        .expect("same offset");
    lane.intake
        .append(&VIDEO_MP4[FIRST_FRAGMENT_END..], &lane.episodes)
        .expect("second");
    assert_eq!(posted_episodes(&lane.mailbox), 2);

    // После changeType медиа без нового init — ошибка, с init — новый эпизод.
    lane.intake.change_container(PushedContainer::FragmentedMp4);
    assert!(
        lane.intake
            .append(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END], &lane.episodes)
            .is_err()
    );
}

#[test]
fn abort_keeps_the_episode_and_counts_only_partial_bytes() {
    let mut lane = fixture();
    lane.intake
        .append(&VIDEO_MP4[..FIRST_FRAGMENT_END + 50], &lane.episodes)
        .expect("first");
    assert_eq!(lane.intake.partial_segment_bytes(), 50);
    lane.intake.abort();
    assert_eq!(lane.intake.partial_segment_bytes(), 0);
    lane.intake
        .append(&VIDEO_MP4[FIRST_FRAGMENT_END..], &lane.episodes)
        .expect("second");
    assert_eq!(posted_episodes(&lane.mailbox), 1);
}

#[test]
fn closed_consumer_discards_new_episodes_and_waiting_ones() {
    let mut lane = fixture();
    lane.intake
        .append(&VIDEO_MP4[..FIRST_FRAGMENT_END], &lane.episodes)
        .expect("first");
    lane.mailbox.close_consumer();
    assert!(matches!(lane.mailbox.take_next(), MailboxTake::Waiting));
    assert_eq!(
        lane.intake
            .append(&VIDEO_MP4[FIRST_FRAGMENT_END..], &lane.episodes),
        Ok(super::AppendOutcome::DiscardedConsumerClosed)
    );
    lane.intake.end_input(&lane.episodes).expect("end");
    assert!(matches!(lane.mailbox.take_next(), MailboxTake::InputEnded));
}
