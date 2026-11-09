//! Строка журнала «Browser quality shown …»: что стенд моста узнаёт о качестве.
//!
//! Пишется при смене **подписи** («1080p» → «720p»), а не при каждом кадре. У каждой подписи —
//! не больше двух строк: в миг смены и при ПЕРВОМ обновлении числа битрейта после неё. Вторая
//! нужна, потому что в миг смены число ещё старое:
//! - подпись следует кадру на экране, а позицию источнику сеанс сообщает с отставанием на
//!   проход: в миг первого кадра 720p позиция у источника ещё в последнем куске 360p, и вес
//!   считается по нему (стенд `quality`: 720p показывал 509 кбит/с ассета 360p);
//! - у первого кадра нового поколения вес ещё неизвестен (поколение приводится к странице на
//!   следующем проходе).
//!
//! Через проход меню уже показывает число нового качества — его и пишет вторая строка. Дальше
//! число меняется со сценой, но журнал о нём молчит (иначе строка на каждый сегмент).
//! Формат фиксирован — его разбирает стенд (`browser-bridge-bench`, берёт последнее известное
//! число подписи): `Browser quality shown label=<label> width=<w> height=<h> bitrate_bps=<n|unknown>`.

use pushed_media_source::MediaBitrate;
use tracing::info;

use super::{QualityMenuView, ShownPicture};

/// Начало строки (сообщение журнала): после него идут поля `label`, `width`, `height`,
/// `bitrate_bps`.
pub(crate) const QUALITY_SHOWN_LINE: &str = "Browser quality shown";

/// Что записать в журнал о показанном качестве.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QualityJournalEntry {
    pub(crate) label: String,
    pub(crate) picture: ShownPicture,
    pub(crate) bitrate: Option<MediaBitrate>,
}

/// Помнит последнюю записанную подпись и что о ней уже сказано.
#[derive(Debug, Default)]
pub(crate) struct QualityLabelJournal {
    last_label: Option<String>,
    /// Битрейт в последней записанной строке (`None` — «unknown»).
    last_bitrate: Option<MediaBitrate>,
    /// Вторая строка этой подписи уже записана: больше о ней не пишем.
    label_settled: bool,
}

/// Записана ли строка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QualityJournalOutcome {
    /// Подпись та же (или кнопки нет): ничего не записано.
    Unchanged,
    /// Подпись сменилась: строка записана.
    Logged,
}

impl QualityJournalOutcome {
    /// Строка была записана.
    pub(crate) const fn was_logged(self) -> bool {
        matches!(self, Self::Logged)
    }
}

impl QualityLabelJournal {
    /// Записывает строку при смене подписи и один раз — при первом обновлении её битрейта
    /// (правило — в описании модуля).
    pub(crate) fn note(&mut self, view: &QualityMenuView) -> QualityJournalOutcome {
        let Some(entry) = view.journal_entry() else {
            return QualityJournalOutcome::Unchanged;
        };
        let same_label = self.last_label.as_deref() == Some(entry.label.as_str());
        if same_label {
            // Пропавшее число (поколение сменилось и ещё не приведено) — не новость.
            let first_update = !self.label_settled
                && entry.bitrate.is_some()
                && entry.bitrate != self.last_bitrate;
            if !first_update {
                return QualityJournalOutcome::Unchanged;
            }
            self.label_settled = true;
        } else {
            self.label_settled = false;
        }
        // Битрейт в строке — числом бит/с (стенд сверяет его с весом потока) или `unknown`.
        let bitrate = entry.bitrate.map_or_else(
            || "unknown".to_owned(),
            |rate| rate.bits_per_second().to_string(),
        );
        info!(
            label = %entry.label,
            width = entry.picture.width,
            height = entry.picture.height,
            bitrate_bps = %bitrate,
            "{QUALITY_SHOWN_LINE}"
        );
        self.last_bitrate = entry.bitrate;
        self.last_label = Some(entry.label);
        QualityJournalOutcome::Logged
    }
}
