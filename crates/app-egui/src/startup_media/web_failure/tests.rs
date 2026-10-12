use anyhow::Context;
use media_source_open::web_open_failure::WebOpenFailureReason;
use service_ytdlp::{YtDlpRejectionReason, YtDlpServiceError};

use super::StartupWebPreparationFailure;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::external_open::request::DroppedWebUrl;
use crate::startup_media::{StartupMediaController, StartupMediaTarget};
use crate::state::{MediaRetryTarget, RetryPlan};

/// Фоновый job классифицирует ошибку сразу: причина не теряется в строке, а
/// диагностика для лога сохраняет всю цепочку.
#[test]
fn preparation_error_keeps_typed_reason_and_full_diagnostic_chain() {
    let error = anyhow::Error::new(YtDlpServiceError::ExtractorRejection {
        stderr_bytes: 64,
        reason: YtDlpRejectionReason::LoginRequired,
    })
    .context("YtDlp media failed to prepare via candidate transport path");

    let failure = StartupWebPreparationFailure::from_preparation_error(&error);

    assert_eq!(failure.reason(), WebOpenFailureReason::SiteLoginRequired);
    assert!(failure.diagnostic().contains("candidate transport path"));
    assert!(failure.diagnostic().contains("login-required"));
}

/// «yt-dlp отключён» в native-fallback пути — типизированная причина, а не строка.
#[test]
fn disabled_extractor_fallback_is_typed_disabled_reason() {
    let error = Err::<(), _>(YtDlpServiceError::AdapterDisabled)
        .context(
            "native HLS admission requires extractor fallback (NativeProfileCompatibilityFallback)",
        )
        .expect_err("error");

    let failure = StartupWebPreparationFailure::from_preparation_error(&error);

    assert_eq!(failure.reason(), WebOpenFailureReason::ExtractorDisabled);
}

/// Отказ без типизированной причины не выдумывает её.
#[test]
fn unclassified_failure_has_generic_reason_and_keeps_diagnostic() {
    let failure = StartupWebPreparationFailure::unclassified("The job ended without a result.");

    assert_eq!(failure.reason(), WebOpenFailureReason::Unclassified);
    assert_eq!(failure.diagnostic(), "The job ended without a result.");
}

/// Контроллер, который открывает ссылку из командной строки и держит её черновик.
fn controller_opening_cli_url(url: &str) -> StartupMediaController {
    let mut controller = StartupMediaController::with_wake_port(
        None,
        None,
        AppWakePort::disconnected(AppWakeOwner::StartupMedia),
    );
    controller
        .orchestration
        .begin_target(StartupMediaTarget::CliReplacement);
    controller.cli_url_target_draft = Some(playlist_core::PlaylistItemDraft::url(
        playlist_core::SecretUrlLocator::from_reopenable_url(url).expect("valid test URL"),
        playlist_core::CachedPlaylistMetadata::new(
            "link",
            playlist_core::PlaylistMediaKind::Unknown,
        ),
    ));
    controller
}

/// B9: сети нет при открытии ссылки из командной строки — «Повторить» откроет ту же ссылку.
#[test]
fn cli_url_network_failure_offers_retry_of_the_same_url() {
    let controller = controller_opening_cli_url("http://media.test/video.mp4?token=1");

    let plan = controller.web_failure_retry_plan(WebOpenFailureReason::NoConnection);

    assert_eq!(
        plan,
        RetryPlan::Offered(MediaRetryTarget::WebUrl(DroppedWebUrl::new(
            "http://media.test/video.mp4?token=1".to_owned()
        )))
    );
}

/// Ссылка не найдена или формат не тот: повтор ничего не изменит, кнопки нет.
#[test]
fn cli_url_permanent_failures_offer_no_retry() {
    let controller = controller_opening_cli_url("http://media.test/video.mp4");

    for reason in [
        WebOpenFailureReason::NotFound,
        WebOpenFailureReason::UnrecognizedFormat,
        WebOpenFailureReason::SitePrivateMedia,
    ] {
        assert_eq!(
            controller.web_failure_retry_plan(reason),
            RetryPlan::NotOffered
        );
    }
}

/// Без цели (отказ после того, как target снят) повтор не предлагается, даже для сети.
#[test]
fn failure_without_startup_target_offers_no_retry() {
    let controller = StartupMediaController::with_wake_port(
        None,
        None,
        AppWakePort::disconnected(AppWakeOwner::StartupMedia),
    );

    assert_eq!(
        controller.web_failure_retry_plan(WebOpenFailureReason::NoConnection),
        RetryPlan::NotOffered
    );
}
