//! Бросок ссылки-коллекции на видео (решение владельца 9, сессия 12) на настоящем
//! `PlaylistRuntime`: тот же topology job, что у «Добавить URL», но результат ждёт замены очереди.
//!
//! Fake заменяет только процесс yt-dlp: worker-поток, generation fence, drain, слот коллекции,
//! admission/подтверждение и commit очереди — production.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use playlist_core::{LocalLocator, PlaylistItemDraft};

use super::*;
use crate::playlist_runtime::controller::{ControllerAppendOutcome, ControllerPlayItemOutcome};
use crate::playlist_runtime::replacement_confirmation::{
    AdmittedQueueReplacementIntent, InAppQueueReplacementAdmission,
};
use crate::playlist_runtime::{
    DroppedWebUrlAppendOutcome, DroppedWebUrlReplacementStart, InAppQueueReplacementIntent,
    ResolvedUrlCollection, RuntimeRowPlayOutcome, ServiceUrlReplacementOutcome,
};
use crate::web_open_message::url_import_failure_message;

/// Что fake вернёт после разрешения теста.
#[derive(Clone, Copy)]
enum CollectionOutcome {
    /// Коллекция из N доступных роликов `clip-1 … clip-N`.
    Clips(usize),
    /// Типизированный отказ.
    Fail(PlaylistUrlImportFailure),
}

/// Resolver, ждущий сигнала теста; считает, сколько раз worker увидел отмену.
struct CollectionResolver {
    started: SyncSender<()>,
    release: Arc<AtomicBool>,
    observed_cancellations: Arc<AtomicUsize>,
    outcome: CollectionOutcome,
}

impl PlaylistUrlTopologyResolver for CollectionResolver {
    fn resolve(
        &self,
        _locator: &service_ytdlp::YtDlpMediaLocator,
        _yt_dlp_config: &YtDlpConfig,
        sensitive_durable_locator_count: usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaylistImportDraft, PlaylistUrlImportFailure> {
        self.started.send(()).expect("test receiver alive");
        while !self.release.load(Ordering::Acquire) && !is_cancelled() {
            thread::yield_now();
        }
        if is_cancelled() {
            self.observed_cancellations.fetch_add(1, Ordering::AcqRel);
            return Err(PlaylistUrlImportFailure::Cancelled);
        }
        match self.outcome {
            CollectionOutcome::Clips(count) => {
                let entries = (1..=count)
                    .map(|index| {
                        PlaylistImportEntryDraft::Single(part(
                            &format!("clip-{index}"),
                            PlaylistImportAvailability::Available,
                        ))
                    })
                    .collect();
                Ok(PlaylistImportDraft::new(
                    entries,
                    Vec::new(),
                    None,
                    sensitive_durable_locator_count,
                ))
            }
            CollectionOutcome::Fail(failure) => Err(failure),
        }
    }
}

/// Runtime с очередью из `old_items` локальных строк и управляемым resolver-ом.
struct DropRuntime {
    runtime: PlaylistRuntime,
    started: Receiver<()>,
    release: Arc<AtomicBool>,
    observed_cancellations: Arc<AtomicUsize>,
}

impl DropRuntime {
    fn new(old_items: usize, outcome: CollectionOutcome) -> Self {
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.resolve_missing_state_for_test();
        if old_items > 0 {
            let drafts = (0..old_items)
                .map(|index| {
                    let name = format!("old-{index}.mkv");
                    PlaylistItemDraft::local(
                        LocalLocator::Native(PathBuf::from(&name)),
                        None,
                        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
                    )
                })
                .collect();
            assert!(matches!(
                runtime.controller.append(drafts).expect("queue fixture"),
                ControllerAppendOutcome::Added { .. }
            ));
        }
        let (started_sender, started) = mpsc::sync_channel(4);
        let release = Arc::new(AtomicBool::new(false));
        let observed_cancellations = Arc::new(AtomicUsize::new(0));
        runtime
            .url_import
            .replace_resolver_for_test(Arc::new(CollectionResolver {
                started: started_sender,
                release: Arc::clone(&release),
                observed_cancellations: Arc::clone(&observed_cancellations),
                outcome,
            }));
        Self {
            runtime,
            started,
            release,
            observed_cancellations,
        }
    }

    /// Бросок ссылки на видео; ждёт, пока worker начнёт разбор.
    fn drop_on_video(&mut self, url: &str) {
        let start = self
            .runtime
            .start_dropped_web_url_replacement(url, &enabled_config());
        assert!(
            matches!(start, DroppedWebUrlReplacementStart::ResolvingTopology),
            "ожидался запуск topology job, got {start:?}"
        );
        self.started
            .recv_timeout(Duration::from_secs(1))
            .expect("resolver started");
    }

    /// Разрешает fake и ждёт коллекцию в слоте владельца.
    fn release_and_take_collection(&mut self) -> ResolvedUrlCollection {
        self.release.store(true, Ordering::Release);
        let mut taken = None;
        wait_until(|| {
            self.runtime.drain_owner_mailbox();
            taken = self.runtime.take_resolved_url_collection();
            taken.is_some()
        });
        taken.expect("коллекция получена")
    }

    /// Названия строк очереди в порядке воспроизведения.
    fn queue_names(&self) -> Vec<String> {
        self.runtime
            .controller
            .queue()
            .iter_playable_items()
            .map(|item| item.cached_metadata().fallback_display_name().to_owned())
            .collect()
    }

    fn indicator_host(&self) -> Option<String> {
        self.runtime
            .playlist_interaction_model()
            .url_import_progress
            .and_then(|progress| progress.display_host.map(|host| host.to_string()))
    }

    fn feedback_text(&self) -> Option<String> {
        self.runtime
            .playlist_interaction_model()
            .safe_feedback
            .map(|feedback| feedback.message.to_string())
    }

    /// Ждёт, пока worker увидит отмену, и убеждается, что результат не доставлен.
    fn assert_cancelled_result_never_arrives(&mut self) {
        wait_until(|| self.observed_cancellations.load(Ordering::Acquire) == 1);
        self.release.store(true, Ordering::Release);
        assert!(!self.runtime.drain_playlist_url_import_job());
        assert!(self.runtime.take_resolved_url_collection().is_none());
    }
}

fn enabled_config() -> YtDlpConfig {
    YtDlpConfig {
        enabled: true,
        ..YtDlpConfig::default()
    }
}

const COLLECTION_URL: &str = "https://www.collection.example.test/playlist/SECRETPATH";

fn confirm_decision(
    runtime: &mut PlaylistRuntime,
    decision: QueueReplacementConfirmationDecision,
) -> PlaylistConfirmationApplyOutcome {
    let model = runtime
        .pending_playlist_confirmation()
        .expect("подтверждение должно ждать ответа");
    runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision,
    })
}

#[test]
fn collection_on_nonempty_queue_shows_count_and_domain_then_confirm_replaces_and_plays_first() {
    let mut drop = DropRuntime::new(2, CollectionOutcome::Clips(3));
    drop.drop_on_video(COLLECTION_URL);
    // Пока разбираем, видна строка прогресса только с доменом; очередь не тронута.
    assert_eq!(
        drop.indicator_host().as_deref(),
        Some("collection.example.test")
    );
    assert_eq!(drop.queue_names(), ["old-0.mkv", "old-1.mkv"]);

    let collection = drop.release_and_take_collection();
    assert_eq!(drop.indicator_host(), None);
    assert_eq!(drop.queue_names(), ["old-0.mkv", "old-1.mkv"]);

    let admission = drop
        .runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::resolved_url_collection(
            collection,
        ))
        .expect("admission");
    assert!(matches!(
        admission,
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    // Вопрос: число и домен, ничего из пути/query ссылки; очередь ждёт ответа.
    let model = drop
        .runtime
        .pending_playlist_confirmation()
        .expect("pending");
    assert_eq!(model.safe_label(), "3 ролика с collection.example.test");
    assert!(model.reasons().queue_replacement());
    for secret in ["SECRETPATH", "playlist"] {
        assert!(!model.safe_label().contains(secret));
    }
    assert_eq!(drop.queue_names(), ["old-0.mkv", "old-1.mkv"]);

    let outcome = confirm_decision(
        &mut drop.runtime,
        QueueReplacementConfirmationDecision::Confirm,
    );
    let PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(
        AdmittedQueueReplacementIntent::ResolvedUrlCollection(admitted),
    ) = outcome
    else {
        panic!("Confirm должен вернуть допущенную коллекцию, got {outcome:?}");
    };
    assert_eq!(
        drop.queue_names(),
        ["old-0.mkv", "old-1.mkv"],
        "Confirm очередь не меняет"
    );

    let ServiceUrlReplacementOutcome::Replaced { item } = drop
        .runtime
        .replace_queue_with_resolved_url_collection(admitted)
    else {
        panic!("замена должна пройти");
    };
    assert_eq!(drop.queue_names(), ["clip-1", "clip-2", "clip-3"]);
    let first_id = drop
        .runtime
        .controller
        .queue()
        .iter_playable_items()
        .next()
        .map(|queued| queued.item_id());
    assert_eq!(Some(item), first_id, "играть надо первую строку");
    assert!(matches!(
        drop.runtime.play_playlist_row(item),
        RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall { .. })
    ));
}

#[test]
fn cancel_on_confirmation_keeps_old_queue() {
    let mut drop = DropRuntime::new(1, CollectionOutcome::Clips(3));
    drop.drop_on_video(COLLECTION_URL);
    let collection = drop.release_and_take_collection();
    drop.runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::resolved_url_collection(
            collection,
        ))
        .expect("admission");

    let outcome = confirm_decision(
        &mut drop.runtime,
        QueueReplacementConfirmationDecision::Cancel,
    );

    assert!(matches!(
        outcome,
        PlaylistConfirmationApplyOutcome::Cancelled
    ));
    assert_eq!(drop.queue_names(), ["old-0.mkv"]);
    assert!(drop.runtime.pending_playlist_confirmation().is_none());
}

#[test]
fn collection_on_empty_queue_replaces_immediately_in_source_order() {
    let mut drop = DropRuntime::new(0, CollectionOutcome::Clips(3));
    drop.drop_on_video(COLLECTION_URL);
    let collection = drop.release_and_take_collection();

    let admission = drop
        .runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::resolved_url_collection(
            collection,
        ))
        .expect("admission");

    let InAppQueueReplacementAdmission::StartNow(
        AdmittedQueueReplacementIntent::ResolvedUrlCollection(admitted),
    ) = admission
    else {
        panic!("пустая очередь — без вопросов, got {admission:?}");
    };
    assert!(drop.runtime.pending_playlist_confirmation().is_none());
    assert!(matches!(
        drop.runtime
            .replace_queue_with_resolved_url_collection(admitted),
        ServiceUrlReplacementOutcome::Replaced { .. }
    ));
    assert_eq!(drop.queue_names(), ["clip-1", "clip-2", "clip-3"]);
}

#[test]
fn single_entry_resolution_replaces_queue_with_exactly_one_row() {
    let mut drop = DropRuntime::new(2, CollectionOutcome::Clips(1));
    drop.drop_on_video(COLLECTION_URL);
    let collection = drop.release_and_take_collection();
    drop.runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::resolved_url_collection(
            collection,
        ))
        .expect("admission");
    let model = drop
        .runtime
        .pending_playlist_confirmation()
        .expect("pending");
    assert_eq!(model.safe_label(), "1 ролик с collection.example.test");

    let PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(
        AdmittedQueueReplacementIntent::ResolvedUrlCollection(admitted),
    ) = confirm_decision(
        &mut drop.runtime,
        QueueReplacementConfirmationDecision::Confirm,
    )
    else {
        panic!("Confirm должен вернуть коллекцию");
    };
    assert!(matches!(
        drop.runtime
            .replace_queue_with_resolved_url_collection(admitted),
        ServiceUrlReplacementOutcome::Replaced { .. }
    ));
    assert_eq!(drop.queue_names(), ["clip-1"]);
}

#[test]
fn sensitive_collection_composes_persistence_reason_with_replacement() {
    let mut drop = DropRuntime::new(1, CollectionOutcome::Clips(2));
    drop.drop_on_video("https://collection.example.test/playlist?list=1&token=SECRETTOKEN");
    let collection = drop.release_and_take_collection();
    assert!(collection.requires_sensitive_persistence_acknowledgement());

    drop.runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::resolved_url_collection(
            collection,
        ))
        .expect("admission");

    let model = drop
        .runtime
        .pending_playlist_confirmation()
        .expect("pending");
    assert!(model.reasons().queue_replacement());
    assert!(model.reasons().sensitive_url_persistence());
    assert!(!model.safe_label().contains("SECRETTOKEN"));
}

#[test]
fn sensitive_collection_asks_even_with_empty_queue() {
    let mut drop = DropRuntime::new(0, CollectionOutcome::Clips(2));
    drop.drop_on_video("https://collection.example.test/playlist?token=SECRETTOKEN");
    let collection = drop.release_and_take_collection();

    let admission = drop
        .runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::resolved_url_collection(
            collection,
        ))
        .expect("admission");

    assert!(matches!(
        admission,
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    let model = drop
        .runtime
        .pending_playlist_confirmation()
        .expect("pending");
    assert!(!model.reasons().queue_replacement());
    assert!(model.reasons().sensitive_url_persistence());
    assert!(drop.queue_names().is_empty());
}

#[test]
fn user_cancel_of_progress_row_leaves_queue_and_delivers_nothing() {
    let mut drop = DropRuntime::new(2, CollectionOutcome::Clips(3));
    drop.drop_on_video(COLLECTION_URL);

    assert_eq!(
        drop.runtime.cancel_playlist_url_import_by_user(),
        PlaylistUrlImportCancelOutcome::Cancelled
    );

    assert_eq!(drop.indicator_host(), None);
    drop.assert_cancelled_result_never_arrives();
    assert_eq!(drop.queue_names(), ["old-0.mkv", "old-1.mkv"]);
    assert_eq!(drop.feedback_text(), None, "отмена молчит");
    assert!(drop.runtime.pending_playlist_confirmation().is_none());
}

#[test]
fn topology_failure_shows_add_url_text_and_leaves_queue() {
    let reason = WebOpenFailureReason::SiteLoginRequired;
    let mut drop = DropRuntime::new(
        2,
        CollectionOutcome::Fail(PlaylistUrlImportFailure::Rejected(reason)),
    );
    drop.drop_on_video(COLLECTION_URL);
    drop.release.store(true, Ordering::Release);

    wait_until(|| drop.runtime.drain_playlist_url_import_job());

    assert_eq!(
        drop.feedback_text(),
        Some(url_import_failure_message(Some("collection.example.test"), reason).to_string())
    );
    assert!(drop.runtime.take_resolved_url_collection().is_none());
    assert_eq!(drop.queue_names(), ["old-0.mkv", "old-1.mkv"]);
    assert_eq!(drop.indicator_host(), None);
    assert!(drop.runtime.pending_playlist_confirmation().is_none());
}

#[test]
fn another_open_during_resolving_drops_the_result() {
    let mut drop = DropRuntime::new(1, CollectionOutcome::Clips(3));
    drop.drop_on_video(COLLECTION_URL);

    // Пока идёт разбор, пользователь открывает другой файл (admission замены очереди).
    let other = InAppQueueReplacementIntent::local_files(vec![PathBuf::from("other.mkv")]);
    drop.runtime
        .admit_in_app_queue_replacement(other)
        .expect("admission");

    assert_eq!(drop.indicator_host(), None, "прогресс снят вместе с job");
    drop.assert_cancelled_result_never_arrives();
    assert_eq!(drop.queue_names(), ["old-0.mkv"]);
}

#[test]
fn another_open_after_resolving_but_before_apply_drops_the_parked_collection() {
    let mut drop = DropRuntime::new(1, CollectionOutcome::Clips(3));
    drop.drop_on_video(COLLECTION_URL);
    drop.release.store(true, Ordering::Release);
    wait_until(|| drop.runtime.drain_playlist_url_import_job());

    // Коллекция уже разобрана и ждёт оболочку, но началось другое открытие.
    let other = InAppQueueReplacementIntent::local_files(vec![PathBuf::from("other.mkv")]);
    drop.runtime
        .admit_in_app_queue_replacement(other)
        .expect("admission");

    assert!(drop.runtime.take_resolved_url_collection().is_none());
    assert_eq!(drop.queue_names(), ["old-0.mkv"]);
}

#[test]
fn direct_media_url_skips_topology_and_keeps_single_link_path() {
    let mut drop = DropRuntime::new(1, CollectionOutcome::Clips(3));

    let start = drop.runtime.start_dropped_web_url_replacement(
        "https://media.example.test/clips/movie.mp4",
        &enabled_config(),
    );

    assert!(
        matches!(start, DroppedWebUrlReplacementStart::SingleLink(_)),
        "got {start:?}"
    );
    assert_eq!(drop.indicator_host(), None, "job не запускался");
    assert_eq!(drop.queue_names(), ["old-0.mkv"]);
}

#[test]
fn garbage_is_refused_with_safe_text_and_no_job() {
    let mut drop = DropRuntime::new(1, CollectionOutcome::Clips(3));

    let start = drop
        .runtime
        .start_dropped_web_url_replacement("https://user:hunter2@ exa mple", &enabled_config());

    let DroppedWebUrlReplacementStart::Refused { user_message } = start else {
        panic!("ожидался отказ, got {start:?}");
    };
    assert!(!user_message.contains("hunter2"));
    assert_eq!(drop.indicator_host(), None);
}

#[test]
fn panel_drop_of_collection_url_still_appends_after_resolving() {
    let mut drop = DropRuntime::new(1, CollectionOutcome::Clips(2));

    let outcome = drop
        .runtime
        .append_dropped_web_url(COLLECTION_URL, &enabled_config());
    assert_eq!(outcome, DroppedWebUrlAppendOutcome::Accepted);
    drop.started
        .recv_timeout(Duration::from_secs(1))
        .expect("resolver started");
    drop.release.store(true, Ordering::Release);
    wait_until(|| drop.runtime.drain_playlist_url_import_job());

    // Панель: результат идёт в S08 preview и дописывается; в слот замены ничего не попало.
    assert!(drop.runtime.take_resolved_url_collection().is_none());
    let preview_id = drop
        .runtime
        .pending_playlist_import_preview()
        .expect("preview")
        .preview_id();
    assert!(matches!(
        drop.runtime.continue_playlist_import(preview_id),
        PlaylistImportContinueOutcome::Committed(_)
    ));
    assert_eq!(drop.queue_names(), ["old-0.mkv", "clip-1", "clip-2"]);
}
