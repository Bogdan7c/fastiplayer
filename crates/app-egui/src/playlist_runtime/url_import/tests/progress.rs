//! Сквозные тесты «Добавить URL» (UX сессия 09) на настоящем `PlaylistRuntime`.
//!
//! Fake заменяет только процесс yt-dlp: worker-поток, generation fence, drain, S08
//! preview/commit и read model для toolbar — production.

use std::io::Write;
use std::sync::mpsc::Receiver;

use super::*;
use crate::web_open_message::url_import_failure_message;

/// Что fake вернёт после разрешения теста.
#[derive(Clone, Copy)]
enum GatedOutcome {
    /// Одна доступная запись.
    Single,
    /// Типизированный отказ.
    Fail(PlaylistUrlImportFailure),
    /// Panic внутри resolver-а (дефект, а не отказ сайта).
    Panic,
}

/// Resolver, который ждёт сигнала теста и сообщает exact locator, полученный worker-ом.
struct GatedResolver {
    started: SyncSender<String>,
    release: Arc<AtomicBool>,
    observed_cancellations: Arc<AtomicUsize>,
    outcome: GatedOutcome,
}

impl PlaylistUrlTopologyResolver for GatedResolver {
    fn resolve(
        &self,
        locator: &service_ytdlp::YtDlpMediaLocator,
        _yt_dlp_config: &YtDlpConfig,
        sensitive_durable_locator_count: usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaylistImportDraft, PlaylistUrlImportFailure> {
        self.started
            .send(locator.expose_secret_for_open().to_owned())
            .expect("test receiver alive");
        while !self.release.load(Ordering::Acquire) && !is_cancelled() {
            thread::yield_now();
        }
        if is_cancelled() {
            self.observed_cancellations.fetch_add(1, Ordering::AcqRel);
            return Err(PlaylistUrlImportFailure::Cancelled);
        }
        match self.outcome {
            GatedOutcome::Single => Ok(PlaylistImportDraft::new(
                vec![PlaylistImportEntryDraft::Single(part(
                    "video",
                    PlaylistImportAvailability::Available,
                ))],
                Vec::new(),
                None,
                sensitive_durable_locator_count,
            )),
            GatedOutcome::Fail(failure) => Err(failure),
            GatedOutcome::Panic => panic!("intentional resolver defect"),
        }
    }
}

/// Runtime с gated resolver-ом и ручками управления им.
struct GatedRuntime {
    runtime: PlaylistRuntime,
    started: Receiver<String>,
    release: Arc<AtomicBool>,
    observed_cancellations: Arc<AtomicUsize>,
}

impl GatedRuntime {
    fn new(outcome: GatedOutcome) -> Self {
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.resolve_missing_state_for_test();
        let (started_sender, started) = mpsc::sync_channel(4);
        let release = Arc::new(AtomicBool::new(false));
        let observed_cancellations = Arc::new(AtomicUsize::new(0));
        runtime
            .url_import
            .replace_resolver_for_test(Arc::new(GatedResolver {
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

    /// Добавляет ссылку и ждёт, пока worker начнёт её обрабатывать.
    fn submit_and_wait_started(&mut self, input: &str) -> String {
        assert_eq!(
            self.runtime
                .append_playlist_url(input, &enabled_config())
                .expect("topology admission"),
            UrlAppendActionOutcome::ResolvingTopology
        );
        self.started
            .recv_timeout(Duration::from_secs(1))
            .expect("resolver started")
    }

    /// Домен индикатора toolbar; `None` — индикатора нет.
    fn indicator(&self) -> Option<PlaylistUrlImportProgress> {
        self.runtime
            .playlist_interaction_model()
            .url_import_progress
    }

    /// Текст в области проблем плейлиста.
    fn feedback_text(&self) -> Option<String> {
        self.runtime
            .playlist_interaction_model()
            .safe_feedback
            .map(|feedback| feedback.message.to_string())
    }
}

fn enabled_config() -> YtDlpConfig {
    YtDlpConfig {
        enabled: true,
        ..YtDlpConfig::default()
    }
}

fn host_indicator(host: &str) -> Option<PlaylistUrlImportProgress> {
    Some(PlaylistUrlImportProgress {
        display_host: Some(Arc::from(host)),
    })
}

#[test]
fn indicator_stays_until_result_then_rows_are_inserted_and_indicator_disappears() {
    let mut gated = GatedRuntime::new(GatedOutcome::Single);
    assert_eq!(gated.indicator(), None);

    gated.submit_and_wait_started("https://www.collection.example.test/root");

    // Пока yt-dlp работает, индикатор виден и показывает только домен.
    assert_eq!(gated.indicator(), host_indicator("collection.example.test"));
    assert!(!gated.runtime.drain_playlist_url_import_job());
    assert_eq!(gated.indicator(), host_indicator("collection.example.test"));

    gated.release.store(true, Ordering::Release);
    wait_until(|| gated.runtime.drain_playlist_url_import_job());

    assert_eq!(gated.indicator(), None);
    let preview_id = gated
        .runtime
        .pending_playlist_import_preview()
        .expect("preview после результата")
        .preview_id();
    assert!(matches!(
        gated.runtime.continue_playlist_import(preview_id),
        PlaylistImportContinueOutcome::Committed(_)
    ));
    assert_eq!(gated.runtime.controller.queue().retained_item_count(), 1);
    assert_eq!(gated.feedback_text(), None);
}

#[test]
fn user_cancel_stops_worker_without_rows_error_or_playback_change() {
    let mut gated = GatedRuntime::new(GatedOutcome::Single);
    gated.submit_and_wait_started("https://collection.example.test/root");

    assert_eq!(
        gated.runtime.cancel_playlist_url_import_by_user(),
        PlaylistUrlImportCancelOutcome::Cancelled
    );

    // Индикатор исчезает сразу, worker видит отмену через generation fence.
    assert_eq!(gated.indicator(), None);
    wait_until(|| gated.observed_cancellations.load(Ordering::Acquire) == 1);
    // Даже разрешённый позже fake не может доставить результат.
    gated.release.store(true, Ordering::Release);
    thread::sleep(Duration::from_millis(20));
    assert!(!gated.runtime.drain_playlist_url_import_job());
    assert!(gated.runtime.pending_playlist_import_preview().is_none());
    assert_eq!(gated.runtime.controller.queue().retained_item_count(), 0);
    assert!(gated.runtime.controller.active_media().is_none());
    // Отмена — не ошибка: в области проблем ничего не появилось.
    assert_eq!(gated.feedback_text(), None);
    // Повторная отмена честно сообщает, что отменять нечего.
    assert_eq!(
        gated.runtime.cancel_playlist_url_import_by_user(),
        PlaylistUrlImportCancelOutcome::NothingActive
    );
}

#[test]
fn user_cancel_without_active_import_keeps_url_draft_untouched() {
    let mut gated = GatedRuntime::new(GatedOutcome::Single);
    gated.runtime.open_playlist_url_editor();
    gated
        .runtime
        .update_playlist_url_draft("https://draft.example.test/kept".to_owned());

    assert_eq!(
        gated.runtime.cancel_playlist_url_import_by_user(),
        PlaylistUrlImportCancelOutcome::NothingActive
    );

    let model = gated.runtime.playlist_interaction_model();
    assert!(model.url_editor_open);
    assert_eq!(model.url_text, "https://draft.example.test/kept");
}

#[test]
fn every_rejection_reason_reaches_its_own_text_with_domain() {
    let reasons = [
        WebOpenFailureReason::SitePrivateMedia,
        WebOpenFailureReason::SiteLoginRequired,
        WebOpenFailureReason::ExtractorNotInstalled,
        WebOpenFailureReason::ExtractorTimedOut,
        WebOpenFailureReason::CollectionTooLarge,
        WebOpenFailureReason::SiteRejected,
    ];
    let mut seen_texts = Vec::new();
    for reason in reasons {
        let mut gated = GatedRuntime::new(GatedOutcome::Fail(PlaylistUrlImportFailure::Rejected(
            reason,
        )));
        gated.submit_and_wait_started("https://www.youtube.com/playlist?list=PLsecret&token=x");
        gated.release.store(true, Ordering::Release);
        wait_until(|| gated.runtime.drain_playlist_url_import_job());

        let text = gated.feedback_text().expect("причина отказа видна");
        assert_eq!(
            text,
            url_import_failure_message(Some("youtube.com"), reason)
        );
        // В тексте только домен: ни пути, ни query с токеном.
        assert!(!text.contains("PLsecret") && !text.contains("token"));
        assert_eq!(gated.indicator(), None);
        assert!(gated.runtime.pending_playlist_import_preview().is_none());
        seen_texts.push(text);
    }
    seen_texts.sort();
    seen_texts.dedup();
    assert_eq!(
        seen_texts.len(),
        reasons.len(),
        "у каждой причины свой текст"
    );
}

#[test]
fn resolver_panic_is_internal_failure_not_a_site_reason() {
    let mut gated = GatedRuntime::new(GatedOutcome::Panic);
    gated.submit_and_wait_started("https://collection.example.test/root");
    gated.release.store(true, Ordering::Release);
    wait_until(|| gated.runtime.drain_playlist_url_import_job());

    assert_eq!(
        gated.feedback_text().as_deref(),
        Some("Не удалось добавить ссылку (collection.example.test) — попробуйте ещё раз")
    );
    assert_eq!(gated.indicator(), None);
}

#[test]
fn latest_url_replaces_running_one_with_single_indicator() {
    let mut gated = GatedRuntime::new(GatedOutcome::Single);
    gated.submit_and_wait_started("https://first.example.test/root");
    gated.submit_and_wait_started("https://second.example.test/root");

    // Один индикатор — для последней ссылки; первая отменена worker-ом.
    assert_eq!(gated.indicator(), host_indicator("second.example.test"));
    wait_until(|| gated.observed_cancellations.load(Ordering::Acquire) == 1);

    gated.release.store(true, Ordering::Release);
    wait_until(|| gated.runtime.drain_playlist_url_import_job());
    assert_eq!(gated.indicator(), None);
    assert!(gated.runtime.pending_playlist_import_preview().is_some());
}

#[test]
fn whitespace_around_pasted_url_yields_the_same_locator() {
    let mut gated = GatedRuntime::new(GatedOutcome::Single);
    let padded = gated.submit_and_wait_started("  https://collection.example.test/root?id=7 \n");
    let exact = gated.submit_and_wait_started("https://collection.example.test/root?id=7");

    assert_eq!(padded, exact);
    assert_eq!(padded, "https://collection.example.test/root?id=7");
}

#[test]
fn bare_domain_gets_https_hint_and_random_text_gets_generic_error() {
    let mut gated = GatedRuntime::new(GatedOutcome::Single);
    let mut submit_draft = |text: &str| {
        gated.runtime.open_playlist_url_editor();
        gated.runtime.update_playlist_url_draft(text.to_owned());
        assert!(gated.runtime.submit_playlist_url_draft(&enabled_config()));
        let model = gated.runtime.playlist_interaction_model();
        // Ошибка формы не закрывает редактор и не запускает извлечение.
        assert!(model.url_editor_open);
        assert_eq!(model.url_import_progress, None);
        model
            .url_safe_error
            .expect("ошибка формы")
            .message()
            .to_owned()
    };

    assert_eq!(
        submit_draft("youtube.com/watch?v=abc"),
        "Похоже, в начале ссылки не хватает https:// — например, https://youtube.com/…"
    );
    assert_eq!(
        submit_draft("  www.youtube.com/watch?v=abc\n"),
        "Похоже, в начале ссылки не хватает https:// — например, https://youtube.com/…"
    );
    assert_eq!(submit_draft("привет"), "Введите корректный http(s) URL");
    assert_eq!(submit_draft("hello"), "Введите корректный http(s) URL");
}

/// `Write`, который копит вывод subscriber-а в общий буфер теста.
#[derive(Clone, Default)]
struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for CapturedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("poisoned log buffer"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn topology_error_is_logged_with_reason_and_mapped_without_payload() {
    let captured = CapturedLog::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();

    let (private_failure, cancelled_failure) =
        tracing::subscriber::with_default(subscriber, || {
            (
                topology_extraction_failure(
                    &service_ytdlp::YtDlpTopologyError::ExtractorRejection {
                        stderr_bytes: 120,
                        reason: service_ytdlp::YtDlpRejectionReason::PrivateMedia,
                    },
                ),
                topology_extraction_failure(&service_ytdlp::YtDlpTopologyError::Cancellation),
            )
        });

    assert_eq!(
        private_failure,
        PlaylistUrlImportFailure::Rejected(WebOpenFailureReason::SitePrivateMedia)
    );
    assert_eq!(cancelled_failure, PlaylistUrlImportFailure::Cancelled);
    let log_text =
        String::from_utf8(captured.0.lock().expect("log buffer").clone()).expect("UTF-8 log");
    assert!(log_text.contains("SitePrivateMedia"), "{log_text}");
    assert!(log_text.contains("private-media"), "{log_text}");
    // Отмена — не ошибка: в лог как отказ не пишется.
    assert_eq!(
        log_text.matches("yt-dlp не получил структуру URL").count(),
        1
    );
}

#[test]
fn dropped_url_on_panel_starts_the_same_import_job_as_add_url_with_the_same_indicator() {
    let mut gated = GatedRuntime::new(GatedOutcome::Single);

    let outcome = gated.runtime.append_dropped_web_url(
        "  https://www.collection.example.test/root?list=1\n",
        &enabled_config(),
    );

    // Бросок принят, worker получил точный адрес (без пробелов), а индикатор и признак
    // «идёт открытие» (его читает busy-предикат drop-а) показывают только домен.
    assert_eq!(
        outcome,
        crate::playlist_runtime::DroppedWebUrlAppendOutcome::Accepted
    );
    assert_eq!(
        gated
            .started
            .recv_timeout(Duration::from_secs(1))
            .expect("resolver started"),
        "https://www.collection.example.test/root?list=1"
    );
    assert_eq!(gated.indicator(), host_indicator("collection.example.test"));
    assert!(gated.runtime.playlist_url_import_progress().is_some());
}
