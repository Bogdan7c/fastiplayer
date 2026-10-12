//! Сквозные тесты ссылок, брошенных в окно, на настоящем `PlaylistRuntime`.
//!
//! Сети нет: direct-ссылки коммитятся без обращения к сайту, а у yt-dlp-ссылок проверяется
//! только admission замены очереди (процесс yt-dlp не стартует). Проверяются очередь,
//! подтверждение и Row Play — то есть то, что увидит пользователь.

use std::path::PathBuf;

use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistLocator, PlaylistMediaKind,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::InAppQueueReplacementIntent;
use crate::playlist_runtime::actions::{NotUrlInputHint, UrlAppendValidationError};
use crate::playlist_runtime::controller::{ControllerAppendOutcome, ControllerPlayItemOutcome};
use crate::playlist_runtime::replacement_confirmation::{
    AdmittedQueueReplacementIntent, InAppQueueReplacementAdmission, PlaylistConfirmationAction,
    QueueReplacementConfirmationDecision,
};
use crate::playlist_runtime::{PlaylistConfirmationApplyOutcome, RuntimeRowPlayOutcome};
use crate::url_service_adapter::{StartupUrlClassification, classify_startup_url};

const DIRECT_URL: &str = "https://media.example.test/clips/movie.mp4";
const SECRET_DIRECT_URL: &str = "https://user:hunter2@media.example.test/clips/movie.mp4?token=abc";

/// Классифицирует брошенную ссылку для замены очереди (без сети и без изменения очереди).
///
/// `Err` — безопасный текст отказа: ссылка не распознана или такого вида не поддерживается.
pub(crate) fn classify_dropped_web_url(
    url_text: &str,
) -> Result<InAppQueueReplacementIntent, Arc<str>> {
    match classify_startup_url(url_text.trim()) {
        StartupUrlClassification::Supported(locator) => {
            Ok(InAppQueueReplacementIntent::service_url(locator))
        }
        StartupUrlClassification::Unsupported { reason } => Err(url_append_error_message(
            UrlAppendValidationError::Unsupported {
                safe_error: reason.safe_error(),
            },
        )),
        StartupUrlClassification::NotUrl => Err(url_append_error_message(
            UrlAppendValidationError::NotUrl(NotUrlInputHint::Unrecognized),
        )),
    }
}

fn runtime_with_queue(item_count: usize) -> PlaylistRuntime {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let drafts = (0..item_count)
        .map(|index| {
            let name = format!("old-{index}.mkv");
            PlaylistItemDraft::local(
                LocalLocator::Native(PathBuf::from(&name)),
                None,
                CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
            )
        })
        .collect();
    if item_count > 0 {
        assert!(matches!(
            runtime.controller.append(drafts).expect("queue fixture"),
            ControllerAppendOutcome::Added { .. }
        ));
    }
    runtime
}

/// Строки очереди как человекочитаемые маркеры: `local:<имя>` или `url:<точный адрес>`.
fn queue_rows(runtime: &PlaylistRuntime) -> Vec<String> {
    runtime
        .controller
        .queue()
        .iter_playable_items()
        .map(|item| match item.locator() {
            PlaylistLocator::Local(local) => format!(
                "local:{}",
                local
                    .expose_native_path_for_open()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            ),
            PlaylistLocator::Url(url) => format!("url:{}", url.expose_secret_for_persistence()),
        })
        .collect()
}

fn confirm(
    runtime: &mut PlaylistRuntime,
    decision: QueueReplacementConfirmationDecision,
) -> PlaylistConfirmationApplyOutcome {
    let model = runtime
        .pending_playlist_confirmation()
        .expect("Confirmation should wait for a response");
    runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision,
    })
}

fn admit(
    runtime: &mut PlaylistRuntime,
    url: &str,
) -> Result<InAppQueueReplacementAdmission, crate::playlist_runtime::QueueReplacementAdmissionError>
{
    let intent = classify_dropped_web_url(url).expect("reference should be supported");
    runtime.admit_in_app_queue_replacement(intent)
}

#[test]
fn video_drop_on_nonempty_queue_waits_for_confirm_then_replaces_queue_and_starts_install() {
    let mut runtime = runtime_with_queue(2);
    let admission = admit(&mut runtime, SECRET_DIRECT_URL).expect("admission");
    assert!(matches!(
        admission,
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));

    // До ответа очередь та же; в вопросе только домен — ни логина, ни пути, ни query.
    assert_eq!(queue_rows(&runtime), ["local:old-0.mkv", "local:old-1.mkv"]);
    let model = runtime.pending_playlist_confirmation().expect("pending");
    assert!(model.reasons().queue_replacement());
    assert!(model.reasons().sensitive_url_persistence());
    for secret in ["hunter2", "token", "abc", "clips", "movie"] {
        assert!(
            !model.safe_label().contains(secret),
            "Confirmation signature disclosed {secret}: {}",
            model.safe_label()
        );
    }
    assert!(model.safe_label().contains("media.example.test"));

    let outcome = confirm(&mut runtime, QueueReplacementConfirmationDecision::Confirm);
    let PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(
        AdmittedQueueReplacementIntent::ServiceUrl(url_open),
    ) = outcome
    else {
        panic!("Confirm must return the accepted link, got {outcome:?}");
    };
    assert_eq!(
        queue_rows(&runtime),
        ["local:old-0.mkv", "local:old-1.mkv"],
        "Confirm doesn't change the queue"
    );

    let locator = url_open.into_locator();
    let ServiceUrlReplacementOutcome::Replaced { item } =
        runtime.replace_queue_with_service_url(&locator)
    else {
        panic!("The replacement must pass");
    };
    assert_eq!(
        queue_rows(&runtime),
        [format!("url:{SECRET_DIRECT_URL}")],
        "queue - exactly one line with the exact address"
    );
    assert!(matches!(
        runtime.play_playlist_row(item),
        RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall { .. })
    ));
}

#[test]
fn cancel_keeps_queue_untouched_and_clears_confirmation() {
    let mut runtime = runtime_with_queue(1);
    admit(&mut runtime, DIRECT_URL).expect("admission");

    let outcome = confirm(&mut runtime, QueueReplacementConfirmationDecision::Cancel);

    assert!(matches!(
        outcome,
        PlaylistConfirmationApplyOutcome::Cancelled
    ));
    assert!(runtime.pending_playlist_confirmation().is_none());
    assert_eq!(queue_rows(&runtime), ["local:old-0.mkv"]);
}

#[test]
fn video_drop_on_empty_queue_with_plain_url_starts_immediately() {
    let mut runtime = runtime_with_queue(0);

    let admission = admit(&mut runtime, DIRECT_URL).expect("admission");

    let InAppQueueReplacementAdmission::StartNow(AdmittedQueueReplacementIntent::ServiceUrl(
        url_open,
    )) = admission
    else {
        panic!("Empty queue and usual link - no questions asked, got {admission:?}");
    };
    assert!(runtime.pending_playlist_confirmation().is_none());
    assert!(matches!(
        runtime.replace_queue_with_service_url(&url_open.into_locator()),
        ServiceUrlReplacementOutcome::Replaced { .. }
    ));
    assert_eq!(queue_rows(&runtime), [format!("url:{DIRECT_URL}")]);
}

#[test]
fn replacement_before_load_decision_is_reported_and_creates_no_queue() {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    assert!(runtime.queue_load_decision_is_pending());
    let intent = classify_dropped_web_url(DIRECT_URL).expect("supported");
    let admitted = runtime.admit_in_app_queue_replacement(intent);
    let Ok(InAppQueueReplacementAdmission::StartNow(AdmittedQueueReplacementIntent::ServiceUrl(
        url_open,
    ))) = admitted
    else {
        panic!("before load decision admission allows start, got {admitted:?}");
    };
    let locator = url_open.into_locator();

    assert_eq!(
        runtime.replace_queue_with_service_url(&locator),
        ServiceUrlReplacementOutcome::LoadDecisionPending
    );
    assert!(runtime.queue_load_decision_is_pending());
}

#[test]
fn unrecognized_text_is_refused_with_safe_message_and_never_echoes_input() {
    let refusal = classify_dropped_web_url("https://user:hunter2@ exa mple");
    let Err(message) = refusal else {
        panic!("Garbage should not be a reference.");
    };
    assert!(!message.is_empty());
    assert!(!message.contains("hunter2"));
}

#[test]
fn panel_drop_of_plain_url_appends_one_row_and_keeps_existing_rows() {
    let mut runtime = runtime_with_queue(1);

    let outcome =
        runtime.append_dropped_web_url(DIRECT_URL, &fastiplayer_config::YtDlpConfig::default());

    assert_eq!(outcome, DroppedWebUrlAppendOutcome::Accepted);
    assert_eq!(
        queue_rows(&runtime),
        vec!["local:old-0.mkv".to_owned(), format!("url:{DIRECT_URL}")]
    );
}

#[test]
fn panel_drop_of_sensitive_url_waits_for_persistence_confirmation_like_add_url() {
    let mut runtime = runtime_with_queue(1);

    let outcome = runtime.append_dropped_web_url(
        SECRET_DIRECT_URL,
        &fastiplayer_config::YtDlpConfig::default(),
    );

    assert_eq!(outcome, DroppedWebUrlAppendOutcome::Accepted);
    assert_eq!(
        queue_rows(&runtime),
        ["local:old-0.mkv"],
        "queue does not change until confirmed"
    );
    let model = runtime.pending_playlist_confirmation().expect("pending");
    assert!(model.reasons().sensitive_url_persistence());
    assert!(
        !model.reasons().queue_replacement(),
        "Add URL does not replace a queue"
    );
    assert!(!model.safe_label().contains("hunter2"));

    assert!(matches!(
        confirm(&mut runtime, QueueReplacementConfirmationDecision::Confirm),
        PlaylistConfirmationApplyOutcome::UrlAppended
    ));
    assert_eq!(
        queue_rows(&runtime),
        vec![
            "local:old-0.mkv".to_owned(),
            format!("url:{SECRET_DIRECT_URL}")
        ]
    );
}

#[test]
fn panel_drop_of_garbage_is_refused_without_touching_queue() {
    let mut runtime = runtime_with_queue(1);

    let outcome = runtime.append_dropped_web_url(
        "https://user:hunter2@ exa mple",
        &fastiplayer_config::YtDlpConfig::default(),
    );

    let DroppedWebUrlAppendOutcome::Refused { user_message } = outcome else {
        panic!("waiver");
    };
    assert!(!user_message.contains("hunter2"));
    assert_eq!(queue_rows(&runtime), ["local:old-0.mkv"]);
}
