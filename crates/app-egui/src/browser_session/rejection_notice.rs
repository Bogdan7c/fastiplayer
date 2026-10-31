//! Отказ поколения глазами зрителя окна (мост, сессия 04b, решение 129).
//!
//! Когда плеер отказывается от поколения, странице уходит `stream_rejected{reason}`, и
//! дальше видео — её забота (экстеншен вернёт его странице, сессия 11). Зритель окна при
//! этом видел бы застывший последний кадр и не понимал, что случилось. Поэтому окно
//! показывает короткое сообщение — тостом, а не в центре: слот центра принадлежит
//! сообщению о конце сеанса (`window::end_notice`), которое его ставит и снимает.
//!
//! Здесь только чистые правила: о каком отказе сказать ([`GenerationRejectionNotice`]),
//! каким текстом и с какой меткой журнала. Когда и как показывать — забота окна
//! (`window::app`); безоконный процесс сообщений не показывает (отказ и так в журнале:
//! `Stream generation rejected`). Тексты — только английские (пользовательский текст UI),
//! без домена и заголовка страницы (инвариант 7: журнал и UI ничего о странице не знают).

use browser_bridge_protocol::session::player::StreamRejection;

/// Сказать ли зрителю окна об отказе поколения за этот шаг.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenerationRejectionNotice {
    /// Говорить нечего: отказа не было, или зритель о нём уже знает от самого плеера.
    Nothing,
    /// Странице ушёл отказ с этой причиной, а зритель о нём ещё не знает.
    Rejected(StreamRejection),
}

/// Текст по причине отказа. Короткий: тост читают за пару секунд.
///
/// Перечисление полное намеренно: новая причина протокола не скомпилируется, пока для
/// неё не выбран текст.
pub(crate) const fn rejection_viewer_message(reason: StreamRejection) -> &'static str {
    match reason {
        StreamRejection::ProtectedContent => "Can't play protected video",
        StreamRejection::UnsupportedCodec => "Can't play this video: unsupported format",
        StreamRejection::UnparsableMedia => "Can't play this video: unreadable data",
        StreamRejection::FileUnavailable => "Can't load this video",
        StreamRejection::PlaybackFailed => "Couldn't play this video",
    }
}

/// Стабильная метка причины для журнала (та же, что в протоколе).
pub(crate) const fn rejection_log_label(reason: StreamRejection) -> &'static str {
    match reason {
        StreamRejection::ProtectedContent => "protected_content",
        StreamRejection::UnsupportedCodec => "unsupported_codec",
        StreamRejection::UnparsableMedia => "unparsable_media",
        StreamRejection::FileUnavailable => "file_unavailable",
        StreamRejection::PlaybackFailed => "playback_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_REASONS: [StreamRejection; 5] = [
        StreamRejection::ProtectedContent,
        StreamRejection::UnsupportedCodec,
        StreamRejection::UnparsableMedia,
        StreamRejection::FileUnavailable,
        StreamRejection::PlaybackFailed,
    ];

    #[test]
    fn every_rejection_has_a_short_english_message_without_page_data() {
        for reason in ALL_REASONS {
            let message = rejection_viewer_message(reason);
            // Тост, а не абзац: читается за пару секунд.
            assert!(message.len() <= 48, "{reason:?}: {message}");
            assert!(message.is_ascii(), "{reason:?}: {message}");
        }
    }

    #[test]
    fn the_rejection_toast_leaves_the_center_slot_to_the_end_notice() {
        use std::time::Instant;

        use crate::state::{CenterNotice, NotificationCenter, OpenProgress};
        use crate::ui::animation::UiMotion;

        let now = Instant::now();
        let mut center = NotificationCenter::default();
        // Центр держит сообщение о конце сеанса (`window::end_notice`).
        center.show_center_status("Source tab closed");
        let message = rejection_viewer_message(StreamRejection::PlaybackFailed);
        // Так окно показывает отказ (`BrowserWindowInstance::notify_viewer`).
        center.notify_transient(message, now);

        let frame = center.frame(OpenProgress::Idle, UiMotion::Standard, now);
        assert_eq!(
            frame
                .toasts
                .iter()
                .map(|toast| toast.message.as_ref())
                .collect::<Vec<_>>(),
            [message]
        );
        assert_eq!(
            frame.center,
            Some(CenterNotice::Status("Source tab closed".into()))
        );
    }

    #[test]
    fn the_log_label_is_the_protocol_name_of_the_reason() {
        for reason in ALL_REASONS {
            let protocol_name = serde_json::to_value(reason).expect("serializes");
            assert_eq!(
                protocol_name.as_str(),
                Some(rejection_log_label(reason)),
                "{reason:?}"
            );
        }
    }
}
