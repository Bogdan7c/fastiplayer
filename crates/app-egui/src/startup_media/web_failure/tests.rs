use anyhow::Context;
use media_source_open::web_open_failure::WebOpenFailureReason;
use service_ytdlp::{YtDlpRejectionReason, YtDlpServiceError};

use super::StartupWebPreparationFailure;

/// Фоновый job классифицирует ошибку сразу: причина не теряется в строке, а
/// диагностика для лога сохраняет всю цепочку.
#[test]
fn preparation_error_keeps_typed_reason_and_full_diagnostic_chain() {
    let error = anyhow::Error::new(YtDlpServiceError::ExtractorRejection {
        stderr_bytes: 64,
        reason: YtDlpRejectionReason::LoginRequired,
    })
    .context("Не удалось подготовить YtDlp media через candidate transport path");

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
        .expect_err("ошибка");

    let failure = StartupWebPreparationFailure::from_preparation_error(&error);

    assert_eq!(failure.reason(), WebOpenFailureReason::ExtractorDisabled);
}

/// Отказ без типизированной причины не выдумывает её.
#[test]
fn unclassified_failure_has_generic_reason_and_keeps_diagnostic() {
    let failure = StartupWebPreparationFailure::unclassified("job завершился без результата");

    assert_eq!(failure.reason(), WebOpenFailureReason::Unclassified);
    assert_eq!(failure.diagnostic(), "job завершился без результата");
}
