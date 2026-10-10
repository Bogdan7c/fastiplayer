//! Таблица решений «предлагать ли повтор» проверяется по результату для каждой причины.

use super::*;

/// Все причины, при которых повтор осмыслен: обстоятельство внешнее.
fn reasons_that_offer_retry() -> Vec<MediaOpenUserFailureReason> {
    vec![
        LocalOpenFailureReason::FileNotFound.into(),
        LocalOpenFailureReason::AccessDenied.into(),
        LocalOpenFailureReason::ReadFailed.into(),
        LocalOpenFailureReason::ReadTimedOut.into(),
        LocalOpenFailureReason::ChangedDuringOpen.into(),
        WebOpenFailureReason::NoConnection.into(),
        WebOpenFailureReason::TimedOut.into(),
        WebOpenFailureReason::ConnectionInterrupted.into(),
        WebOpenFailureReason::ServerError.into(),
        WebOpenFailureReason::RateLimited.into(),
        WebOpenFailureReason::ExtractorTimedOut.into(),
        PlayerInstallFailureReason::PreparationTimedOut.into(),
        PlayerInstallFailureReason::PlayerNotResponding.into(),
    ]
}

/// Причины, при которых повтор вернёт тот же отказ: формат, ссылка, права владельца ссылки.
fn reasons_that_withhold_retry() -> Vec<MediaOpenUserFailureReason> {
    vec![
        LocalOpenFailureReason::UnrecognizedFormat.into(),
        LocalOpenFailureReason::DamagedOrTruncated.into(),
        LocalOpenFailureReason::IsDirectory.into(),
        LocalOpenFailureReason::EmptyFile.into(),
        LocalOpenFailureReason::NoAudioOrVideo.into(),
        LocalOpenFailureReason::InternalError.into(),
        WebOpenFailureReason::UnrecognizedFormat.into(),
        WebOpenFailureReason::NoPlayableFormat.into(),
        WebOpenFailureReason::NotFound.into(),
        WebOpenFailureReason::Gone.into(),
        WebOpenFailureReason::AuthenticationRequired.into(),
        WebOpenFailureReason::AccessDenied.into(),
        WebOpenFailureReason::SitePrivateMedia.into(),
        WebOpenFailureReason::SiteLoginRequired.into(),
        WebOpenFailureReason::ExtractorNotInstalled.into(),
        WebOpenFailureReason::ExtractorDisabled.into(),
        WebOpenFailureReason::UnsupportedLink.into(),
        WebOpenFailureReason::Unclassified.into(),
        PlayerInstallFailureReason::UnsupportedVideoFormat.into(),
        PlayerInstallFailureReason::UnsupportedAudioFormat.into(),
        PlayerInstallFailureReason::NoSuitableVideoDecoder.into(),
        PlayerInstallFailureReason::InternalError.into(),
    ]
}

#[test]
fn external_circumstances_offer_retry() {
    for reason in reasons_that_offer_retry() {
        assert_eq!(
            reason.retry_advice(),
            RetryAdvice::Offer,
            "для {reason:?} повтор должен предлагаться"
        );
    }
}

#[test]
fn unfixable_failures_do_not_offer_retry() {
    for reason in reasons_that_withhold_retry() {
        assert_eq!(
            reason.retry_advice(),
            RetryAdvice::Withhold,
            "для {reason:?} повтор бессмыслен"
        );
    }
}
