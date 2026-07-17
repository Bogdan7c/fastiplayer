use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use playlist_core::{
    CachedPlaylistMetadata, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind, SecretUrlLocator,
};

use super::*;
use crate::app_wake::AppWakeOwner;

struct FakeYoutubeMetadataResolver {
    outcomes: Mutex<VecDeque<YoutubeMetadataTaskOutcome>>,
    calls: AtomicUsize,
}

impl FakeYoutubeMetadataResolver {
    fn new(outcomes: Vec<YoutubeMetadataTaskOutcome>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into()),
            calls: AtomicUsize::new(0),
        }
    }
}

impl YoutubeMetadataResolver for FakeYoutubeMetadataResolver {
    fn resolve(
        &self,
        _locator: &service_youtube::YoutubeMediaLocator,
        _youtube_config: &YoutubeConfig,
        cancellation: &CancellationToken,
    ) -> YoutubeMetadataTaskOutcome {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if cancellation.is_cancelled() {
            return YoutubeMetadataTaskOutcome::Cancelled;
        }
        self.outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .unwrap_or(YoutubeMetadataTaskOutcome::Failed)
    }
}

fn youtube_config() -> YoutubeConfig {
    YoutubeConfig {
        enabled: true,
        ..YoutubeConfig::default()
    }
}

fn append_youtube_item(
    controller: &mut PlaylistController,
    url: &str,
    cached_metadata: CachedPlaylistMetadata,
) -> PlaylistItemId {
    let locator = SecretUrlLocator::from_reopenable_url(url).expect("valid test URL");
    controller
        .append_capped_tail(vec![PlaylistItemDraft::url(locator, cached_metadata)])
        .expect("test append")
        .item_ids[0]
}

fn demand(
    controller: &PlaylistController,
    item_id: PlaylistItemId,
    url: &str,
) -> YoutubeMetadataDemand {
    let expected_locator = controller
        .queue()
        .item(item_id)
        .expect("committed test item")
        .locator()
        .clone();
    let youtube_locator =
        service_youtube::parse_youtube_media_locator(url).expect("valid YouTube URL");
    YoutubeMetadataDemand::new(item_id, expected_locator, youtube_locator, youtube_config())
}

fn drain_until_idle(
    owner: &mut YoutubeMetadataOwner,
    controller: &mut PlaylistController,
    now: Instant,
) {
    for _ in 0..200 {
        let _visible_change = owner.drain(controller, now);
        if owner.active.is_empty() && owner.pending.is_empty() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("YouTube metadata test job did not become idle");
}

#[test]
fn resolved_metadata_updates_only_exact_duplicate_item_and_preserves_other_cache() {
    let mut controller = PlaylistController::new();
    let url = "https://youtu.be/same-video";
    let rich_fallback =
        CachedPlaylistMetadata::new("YouTube media (youtu.be)", PlaylistMediaKind::Unknown)
            .with_artists(vec!["Сохранённый автор".to_string()])
            .expect("bounded artists");
    let first_id = append_youtube_item(&mut controller, url, rich_fallback);
    let second_id = append_youtube_item(
        &mut controller,
        url,
        CachedPlaylistMetadata::new("YouTube media (youtu.be)", PlaylistMediaKind::Unknown),
    );
    let first_demand = demand(&controller, first_id, url);

    assert_eq!(
        apply_resolved_metadata(
            &mut controller,
            &first_demand,
            Some("Настоящее название".to_string()),
            Some(Duration::from_secs(125)),
        ),
        YoutubeMetadataApplyOutcome::Applied
    );

    let first = controller.queue().item(first_id).expect("first item");
    assert_eq!(first.cached_metadata().title(), Some("Настоящее название"));
    assert_eq!(
        first.cached_metadata().artists(),
        &["Сохранённый автор".to_string()]
    );
    assert_eq!(
        first.cached_metadata().duration(),
        Some(media_core::MediaDuration::from_duration(
            Duration::from_secs(125)
        ))
    );
    assert_eq!(
        controller
            .queue()
            .item(second_id)
            .expect("duplicate item")
            .cached_metadata()
            .title(),
        None
    );
}

#[test]
fn stale_locator_and_missing_item_never_mutate_queue() {
    let mut controller = PlaylistController::new();
    let url = "https://youtu.be/current";
    let item_id = append_youtube_item(
        &mut controller,
        url,
        CachedPlaylistMetadata::new("YouTube media (youtu.be)", PlaylistMediaKind::Unknown),
    );
    let mut stale_demand = demand(&controller, item_id, url);
    stale_demand.expected_locator = playlist_core::PlaylistLocator::Url(
        SecretUrlLocator::from_reopenable_url("https://youtu.be/stale").expect("valid stale URL"),
    );
    let dirty_before = controller.dirty_revision();

    assert_eq!(
        apply_resolved_metadata(
            &mut controller,
            &stale_demand,
            Some("Не должно примениться".to_string()),
            None,
        ),
        YoutubeMetadataApplyOutcome::Stale
    );
    assert_eq!(controller.dirty_revision(), dirty_before);
    assert_eq!(
        controller
            .queue()
            .item(item_id)
            .expect("item remains")
            .cached_metadata()
            .title(),
        None
    );
}

#[test]
fn owner_coalesces_in_flight_and_retries_failed_job_after_delay() {
    let mut controller = PlaylistController::new();
    let url = "https://youtu.be/retry";
    let item_id = append_youtube_item(
        &mut controller,
        url,
        CachedPlaylistMetadata::new("YouTube media (youtu.be)", PlaylistMediaKind::Unknown),
    );
    let fake = Arc::new(FakeYoutubeMetadataResolver::new(vec![
        YoutubeMetadataTaskOutcome::Failed,
        YoutubeMetadataTaskOutcome::Resolved {
            title: Some("Название после retry".to_string()),
            duration: Some(Duration::from_secs(42)),
        },
    ]));
    let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut owner = YoutubeMetadataOwner::new(wake_port);
    owner.replace_resolver_for_test(fake.clone());
    let initial_now = Instant::now();

    let accepted = owner.request(vec![demand(&controller, item_id, url)], initial_now);
    assert_eq!(accepted.accepted, 1);
    let coalesced = owner.request(vec![demand(&controller, item_id, url)], initial_now);
    assert_eq!(coalesced.coalesced, 1);
    drain_until_idle(&mut owner, &mut controller, initial_now);
    assert_eq!(fake.calls.load(Ordering::Relaxed), 1);

    let early_retry = owner.request(
        vec![demand(&controller, item_id, url)],
        initial_now + Duration::from_secs(29),
    );
    assert_eq!(early_retry.coalesced, 1);
    let accepted_retry = owner.request(
        vec![demand(&controller, item_id, url)],
        initial_now + YOUTUBE_METADATA_RETRY_DELAY,
    );
    assert_eq!(accepted_retry.accepted, 1);
    drain_until_idle(
        &mut owner,
        &mut controller,
        initial_now + YOUTUBE_METADATA_RETRY_DELAY,
    );

    assert_eq!(fake.calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        controller
            .queue()
            .item(item_id)
            .expect("item remains")
            .cached_metadata()
            .title(),
        Some("Название после retry")
    );
}

#[test]
fn unavailable_executor_reports_typed_outcome_without_running_resolver() {
    let mut controller = PlaylistController::new();
    let url = "https://youtu.be/no-executor";
    let item_id = append_youtube_item(
        &mut controller,
        url,
        CachedPlaylistMetadata::new("YouTube media (youtu.be)", PlaylistMediaKind::Unknown),
    );
    let fake = Arc::new(FakeYoutubeMetadataResolver::new(Vec::new()));
    let mut owner = YoutubeMetadataOwner::with_dependencies(
        AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime),
        None,
        fake.clone(),
    );

    let outcome = owner.request(vec![demand(&controller, item_id, url)], Instant::now());

    assert_eq!(outcome.executor_unavailable, 1);
    assert_eq!(fake.calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        controller
            .queue()
            .item(item_id)
            .expect("item remains")
            .cached_metadata()
            .title(),
        None
    );
}
