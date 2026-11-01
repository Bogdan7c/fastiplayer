//! С какого места начинается новое поколение потока (сессия 03e, решение 111).
//!
//! Новый `MediaSource` (ручная смена качества YouTube, реклама, следующая серия)
//! сбрасывает время элемента страницы в начало, и плеер страницы тут же ставит
//! `currentTime` туда, где зритель был. Экстеншен пересказывает это обычным
//! `seek_started{page_seek}` — позиция в протоколе уже есть. Байты нового
//! поколения при этом начинаются не с нуля и не с позиции, а с ближайшей коробки
//! ДО неё (живой YouTube: позиция 27,2 с, видео с 24,4 с, звук с 20,0 с).
//!
//! Правило (подтверждено владельцем):
//! - страница назвала позицию, пока поколение открывалось, — поколение ставится в
//!   плеер сразу на неё ([`GenerationStart::PageSeekedTo`]);
//! - не назвала (реклама и новая серия начинаются с нуля, страница не перематывает)
//!   — с первого момента, где у потока есть данные ВСЕХ полос
//!   ([`GenerationStart::FirstCommonData`]): плеер, стартующий с нуля при данных с
//!   20 с, ждал бы в пустоте.
//!
//! Перемотка страницы, пришедшая ПОСЛЕ загрузки, — обычная перемотка плеера (её
//! ведёт `SessionMedia::follow_page`), этот модуль её не касается.
//!
//! Как позиция доходит до плеера, зависит от вида поколения:
//! - сегменты MSE: демуксер потока перематывается здесь же, синхронно (перемотка
//!   источника никогда не ждёт байтов), и `PreparedMedia` несёт уже готовую позицию
//!   ([`PreparedInitialPosition::PositionedAt`]) — плеер ставит медиа сразу на неё,
//!   без «загрузить с нуля, потом перемотать»: часы не начинаются с нуля, звук и
//!   кадры до позиции не играют. Странице перемотка не шлётся: источник уже знает,
//!   что страница идёт туда сама (`note_page_seek` в порядке провода). С 04d (путь C)
//!   поколение с позицией уходит плееру, только когда полка её держит — когда и с
//!   какого места отдать, решают ворота [`super::generation_placement`];
//! - файл `<video src>`: его демуксер — фоновый и с запросами байтов, синхронно не
//!   перематывается; плеер перематывается командой сразу после загрузки
//!   ([`AfterLoad::SeekTo`]).

use std::time::Duration;

use media_core::{DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaTime};
use player_core::{PreparedDemuxSeekLandingPolicy, PreparedInitialPosition};
use pushed_media_source::{PushedStreamDemuxer, PushedStreamMonitor};
use tracing::{debug, info, warn};

/// Откуда новое поколение начинает показ. Владелец — ждущее поколение
/// `SessionMedia`: оно знает, сказала ли страница своё «я на T», пока поколение
/// открывалось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GenerationStart {
    /// Страница не называла позицию: с первого момента, где есть данные всех полос.
    FirstCommonData,
    /// Страница перемоталась сюда, пока поколение открывалось (последняя побеждает).
    PageSeekedTo(Duration),
}

/// Что сделать сразу после загрузки поколения, чтобы оно стартовало с нужного места.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AfterLoad {
    /// Ничего: медиа уже стоит где нужно (или начинается с начала).
    Nothing,
    /// Перемотать плеер сюда (поколение-файл: демуксер синхронно не перематывается).
    SeekTo(Duration),
}

impl GenerationStart {
    /// Что сделать после загрузки поколения-файла.
    pub(super) const fn after_file_load(self) -> AfterLoad {
        match self {
            // Файл читается с начала; где лежат его байты, решает сам файл.
            Self::FirstCommonData => AfterLoad::Nothing,
            Self::PageSeekedTo(target) => AfterLoad::SeekTo(target),
        }
    }

    /// Куда поставить демуксер поколения-сегментов (`None` — читать с начала).
    fn segments_target(self, monitor: &PushedStreamMonitor) -> Option<Duration> {
        match self {
            Self::PageSeekedTo(target) => Some(target),
            // Данные с нуля — обычное начало: перематывать незачем.
            Self::FirstCommonData => monitor
                .earliest_common_start()
                .filter(|start| !start.is_zero()),
        }
    }
}

/// Можно ли поставить поколение-сегменты на позицию (те же условия, что у
/// [`position_segments_demuxer`]): не эфир и поток с известной длительностью.
///
/// Только такое поколение ворота 04d держат до данных у цели; остальные стартуют там,
/// где лежат байты страницы, и ждать им нечего.
pub(super) fn is_positionable(demuxer: &PushedStreamDemuxer, is_live: bool) -> bool {
    !is_live && demuxer.seekability() == DemuxSeekability::Seekable
}

/// Ставит демуксер поколения-сегментов на старт и возвращает позицию для
/// `PreparedMedia` (`Beginning` — читать с начала).
///
/// Живой эфир не перематывается: вход в эфир решает сам плеер (live join), а
/// позиции страницы в эфире относятся к живому краю.
///
/// Поток без известной длительности тоже не позиционируется: источник объявляет его
/// неперематываемым, а ядро ставит медиа «сразу на позицию» только перематываемым
/// (иначе загрузка кончилась бы отказом плеера). Такое поколение играет с начала —
/// ограничение 03b «длительность после открытия не подхватывается».
///
/// Перемотку источник отвергает, только когда поток уже сломан: поколение тогда
/// ставится «с начала», а настоящую причину плеер получит при чтении демуксера и
/// сообщит странице отказом. В журнал — только текст ошибки источника (без адресов и
/// заголовков страницы).
pub(super) fn position_segments_demuxer(
    start: GenerationStart,
    monitor: &PushedStreamMonitor,
    demuxer: &mut PushedStreamDemuxer,
    is_live: bool,
) -> PreparedInitialPosition {
    let Some(target) = start.segments_target(monitor) else {
        return PreparedInitialPosition::Beginning;
    };
    if is_live {
        debug!(
            target_us = target.as_micros(),
            "Live stream generation starts at the live join, not at the page position"
        );
        return PreparedInitialPosition::Beginning;
    }
    if demuxer.seekability() != DemuxSeekability::Seekable {
        info!(
            target_us = target.as_micros(),
            "Stream generation without a known duration cannot be positioned; it starts from its beginning"
        );
        return PreparedInitialPosition::Beginning;
    }
    let target_position = MediaTime::from_duration(target);
    match demuxer.seek_with_request(DemuxSeekRequest::accurate(target)) {
        Ok(result) => positioned_at(target_position, result),
        Err(seek_error) => {
            warn!(
                %seek_error,
                target_us = target.as_micros(),
                "The stream generation could not be positioned; it starts from its beginning"
            );
            PreparedInitialPosition::Beginning
        }
    }
}

/// Готовая позиция для плеера по итогу перемотки демуксера.
///
/// Перемотка по полке садится на коробку, содержащую цель (начало не позже цели), —
/// плеер декодирует вперёд и прячет всё до цели. Если ближайшая коробка начинается
/// чуть ПОСЛЕ цели (полка допускает до 100 мс), плеер начинает с неё: «декодировать
/// вперёд до цели» с такой посадкой ядро справедливо считает противоречием.
fn positioned_at(target_position: MediaTime, result: DemuxSeekResult) -> PreparedInitialPosition {
    let landing_policy = if result.actual_position <= target_position {
        PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget
    } else {
        PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget
    };
    PreparedInitialPosition::PositionedAt {
        target_position,
        result,
        landing_policy,
    }
}

#[cfg(test)]
#[path = "generation_start_tests.rs"]
mod tests;
