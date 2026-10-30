//! Как ошибка источника `pushed-media-source` превращается в ответ странице.
//!
//! Правило (решения 93–94, `user/browser-bridge/03b-seek-backpressure-live.md`):
//! - защищённое видео, неподдержанный формат, нечитаемые байты — это свойство
//!   ПОКОЛЕНИЯ: странице уходит `stream_rejected{reason}`, экстеншен возвращает видео
//!   странице, а сеанс живёт (следующее поколение может оказаться играбельным);
//! - нарушение порядка вызовов (`InputMisuse`: неизвестный SourceBuffer, медиа до
//!   init, переполнение, …) — ошибка того, кто приносит байты: сеанс закрывается
//!   кодом 4003 (`ProtocolViolation`).
//!
//! У файла `<video src>` (03c) то же правило плюс один класс: файл, который окончательно
//! недоступен (сеть не вернулась, файл сменился на сервере, диапазон отвергнут), —
//! тоже свойство поколения: `stream_rejected{file_unavailable}`, видео возвращается
//! странице, сеанс живёт.

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_protocol::session::player::StreamRejection;
use pushed_media_source::{
    PushedFileError, PushedFileFailureClass, PushedStreamError, PushedStreamFailureClass,
    UnsupportedMediaType,
};

/// Чем ответить на ошибку потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureResponse {
    /// Отказаться от поколения: странице — `stream_rejected`.
    RejectGeneration(StreamRejection),
    /// Закрыть сеанс этим кодом.
    CloseSession(SessionCloseReason),
}

/// Ответ на ошибку потока по её грубому классу.
pub(crate) const fn response_to_stream_failure(error: &PushedStreamError) -> FailureResponse {
    match error.failure_class() {
        PushedStreamFailureClass::ProtectedContent => {
            FailureResponse::RejectGeneration(StreamRejection::ProtectedContent)
        }
        PushedStreamFailureClass::UnsupportedMedia => {
            FailureResponse::RejectGeneration(StreamRejection::UnsupportedCodec)
        }
        PushedStreamFailureClass::UnparsableMedia => {
            FailureResponse::RejectGeneration(StreamRejection::UnparsableMedia)
        }
        PushedStreamFailureClass::InputMisuse => {
            FailureResponse::CloseSession(SessionCloseReason::ProtocolViolation)
        }
    }
}

/// Ответ на ошибку файла по её грубому классу.
pub(crate) const fn response_to_file_failure(error: &PushedFileError) -> FailureResponse {
    match error.failure_class() {
        PushedFileFailureClass::ProtectedContent => {
            FailureResponse::RejectGeneration(StreamRejection::ProtectedContent)
        }
        PushedFileFailureClass::UnsupportedMedia => {
            FailureResponse::RejectGeneration(StreamRejection::UnsupportedCodec)
        }
        PushedFileFailureClass::UnparsableMedia => {
            FailureResponse::RejectGeneration(StreamRejection::UnparsableMedia)
        }
        PushedFileFailureClass::SourceUnavailable => {
            FailureResponse::RejectGeneration(StreamRejection::FileUnavailable)
        }
        PushedFileFailureClass::InputMisuse => {
            FailureResponse::CloseSession(SessionCloseReason::ProtocolViolation)
        }
    }
}

/// Ответ на MIME, который источник не умеет (`addSourceBuffer`/`changeType`):
/// формат плееру не по силам — отказ от поколения, а не нарушение протокола.
pub(crate) const fn response_to_unsupported_media_type(
    _unsupported: &UnsupportedMediaType,
) -> FailureResponse {
    FailureResponse::RejectGeneration(StreamRejection::UnsupportedCodec)
}

#[cfg(test)]
mod tests {
    use pushed_media_source::{LaneId, LaneLayoutProblem, PushedContainer, UnsupportedFileLayout};

    use super::*;

    const LANE: LaneId = LaneId::new(0);

    #[test]
    fn each_failure_class_maps_to_its_page_answer() {
        let cases = [
            (
                PushedStreamError::ProtectedContent { lane: LANE },
                FailureResponse::RejectGeneration(StreamRejection::ProtectedContent),
            ),
            (
                PushedStreamError::UnsupportedLayout {
                    problem: LaneLayoutProblem::NoMedia,
                },
                FailureResponse::RejectGeneration(StreamRejection::UnsupportedCodec),
            ),
            (
                PushedStreamError::Demux {
                    lane: LANE,
                    reason: "broken".to_owned(),
                },
                FailureResponse::RejectGeneration(StreamRejection::UnparsableMedia),
            ),
            (
                PushedStreamError::UnknownLane { lane: LANE },
                FailureResponse::CloseSession(SessionCloseReason::ProtocolViolation),
            ),
            (
                PushedStreamError::MediaBeforeInitialization { lane: LANE },
                FailureResponse::CloseSession(SessionCloseReason::ProtocolViolation),
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(response_to_stream_failure(&error), expected, "{error}");
        }
    }

    #[test]
    fn each_file_failure_class_maps_to_its_page_answer() {
        let cases = [
            (
                PushedFileError::ProtectedContent,
                FailureResponse::RejectGeneration(StreamRejection::ProtectedContent),
            ),
            (
                PushedFileError::UnsupportedLayout {
                    layout: UnsupportedFileLayout::MovieAtEndWithoutKnownSize,
                },
                FailureResponse::RejectGeneration(StreamRejection::UnsupportedCodec),
            ),
            (
                PushedFileError::UnparsableMedia {
                    reason: "broken".to_owned(),
                },
                FailureResponse::RejectGeneration(StreamRejection::UnparsableMedia),
            ),
            (
                PushedFileError::RetryBudgetExhausted { attempts: 3 },
                FailureResponse::RejectGeneration(StreamRejection::FileUnavailable),
            ),
            (
                PushedFileError::ResourceChanged,
                FailureResponse::RejectGeneration(StreamRejection::FileUnavailable),
            ),
            (
                PushedFileError::SealedBeforeOpen,
                FailureResponse::RejectGeneration(StreamRejection::FileUnavailable),
            ),
            (
                PushedFileError::UnissuedRequest { id: 9 },
                FailureResponse::CloseSession(SessionCloseReason::ProtocolViolation),
            ),
            (
                PushedFileError::NonContiguousBytes {
                    id: 1,
                    expected: 0,
                    actual: 5,
                },
                FailureResponse::CloseSession(SessionCloseReason::ProtocolViolation),
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(response_to_file_failure(&error), expected, "{error}");
        }
    }

    #[test]
    fn unsupported_mime_rejects_the_generation_as_an_unsupported_codec() {
        let unsupported =
            PushedContainer::from_mime_type("video/x-flv").expect_err("flv is not supported");
        assert_eq!(
            response_to_unsupported_media_type(&unsupported),
            FailureResponse::RejectGeneration(StreamRejection::UnsupportedCodec)
        );
    }
}
