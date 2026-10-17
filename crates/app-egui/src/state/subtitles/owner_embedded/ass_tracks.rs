//! Сценарии ASS встроенных дорожек: заголовок разбирается один раз, события — по нему.
//!
//! # Контракт
//! - Заголовок (`codec_private`) приходит в `MediaStarted` раньше пакетов своего media (труба
//!   одна, порядок сохраняется), поэтому к первому пакету заголовок уже известен; сам разбор
//!   выполняется один раз при первом ASS-пакете дорожки (кодек к тому моменту точно известен).
//! - Дорожка со сценарием выдаёт `SubtitleContent::Ass` (стили, отступы, теги слоя 1).
//!   Дорожка без сценария (нет заголовка, он больше лимита, сообщение потеряно трубой или
//!   дорожка появилась позже через `TracksChanged`) остаётся на запасном пути «только текст».
//! - `CueKey` обещает «тот же ключ = то же содержимое», поэтому смена сценария дорожки
//!   обязана менять `CueSourceId` (владелец делает это, если заголовок дорожки заменён).
//!
//! # Дедупликация по `ReadOrder`
//! Matroska повторяет события в блоках, а после seek назад демуксер перечитывает те же
//! пакеты. libass принимает событие только если его `ReadOrder` ещё не встречался; здесь то
//! же правило: у дорожки есть ограниченное множество уже виданных `ReadOrder`. Одного
//! таймлайна для этого мало: он отбрасывает только полностью совпавшие события, а два
//! разных события с одним `ReadOrder` (ошибка муксера) libass считает повтором.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bytes::Bytes;
use media_core::TrackId;
use subtitle_core::{AssScript, SubtitleContent, SubtitleEvent};
use subtitle_formats::{
    AssHeaderReport, AssParseError, AssParseLimits, AssTagStats, EmbeddedCueLimits,
    parse_ass_script, parse_embedded_ass_event,
};

use player_core::SubtitleFeedPacket;

use crate::state::subtitles::embedded_events::{
    PacketEventOutcome, PacketRejection, ass_event_duration, cue_interval,
};

/// Что известно о сценарии дорожки в момент первого ASS-пакета.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum AssScriptAvailability {
    /// Сценарий готов: события дорожки идут по ASS-пути.
    Ready,
    /// Заголовка нет: дорожка остаётся на пути «только текст» (без счётчика: это норма для
    /// дорожек, добавленных позже, и для потерянного `MediaStarted`).
    NoHeader,
    /// Заголовок отвергнут при разборе (больше лимита): путь «только текст». Сообщается
    /// ровно один раз, на пакете, который вызвал разбор.
    JustRejected(AssParseError),
    /// Заголовок уже был отвергнут раньше: путь «только текст», повторно не сообщается.
    Unusable,
}

/// Что сделала `stash_header` с уже известным заголовком дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum HeaderStash {
    /// Заголовка этой дорожки ещё не было.
    First,
    /// Заголовок заменил прежний: `CueSourceId` дорожки обязан смениться.
    ReplacedPrevious,
}

/// Состояние заголовка одной дорожки.
enum HeaderState {
    /// Байты получены, разбор отложен до первого ASS-пакета: кодек дорожки на момент
    /// `MediaStarted` может быть ещё неизвестен (снимок плеера отстаёт от трубы), а
    /// разбирать заголовок не-ASS дорожки (mov_text и др.) незачем.
    Pending(Bytes),
    /// Разобран.
    Ready(AssTrackScript),
    /// Отвергнут: дальше дорожка живёт без сценария.
    Unusable,
}

/// Сценарий и накопленное состояние разбора одной дорожки.
struct AssTrackScript {
    script: Arc<AssScript>,
    /// Статистика тегов (неподдержанные теги слоёв 2–3, неизвестные, битые) для диагностики.
    tag_stats: AssTagStats,
    /// Уже принятые `ReadOrder` (правило libass «повтор игнорируется»).
    seen_read_orders: HashSet<i32>,
}

/// Заголовки и сценарии ASS дорожек одного media (создаётся вместе с `MediaSubtitles`).
///
/// Память ограничена: по одному заголовку на дорожку (размер ограничен демуксером), после
/// разбора сырые байты освобождаются.
#[derive(Default)]
pub(in crate::state::subtitles) struct EmbeddedAssTracks {
    headers: HashMap<TrackId, HeaderState>,
}

impl EmbeddedAssTracks {
    /// Запоминает заголовок дорожки из `MediaStarted` (разбор — при первом ASS-пакете).
    pub(in crate::state::subtitles) fn stash_header(
        &mut self,
        track_id: TrackId,
        header: Bytes,
    ) -> HeaderStash {
        match self.headers.insert(track_id, HeaderState::Pending(header)) {
            Some(_) => HeaderStash::ReplacedPrevious,
            None => HeaderStash::First,
        }
    }

    /// Гарантирует, что заголовок дорожки разобран, если он есть и ещё не разбирался.
    pub(in crate::state::subtitles) fn ensure_script(
        &mut self,
        track_id: TrackId,
        ass_limits: &AssParseLimits,
    ) -> AssScriptAvailability {
        let Some(state) = self.headers.get_mut(&track_id) else {
            return AssScriptAvailability::NoHeader;
        };
        let header_bytes = match state {
            HeaderState::Ready(_) => return AssScriptAvailability::Ready,
            HeaderState::Unusable => return AssScriptAvailability::Unusable,
            HeaderState::Pending(bytes) => bytes.clone(),
        };
        match parse_ass_script(&header_bytes, ass_limits) {
            Ok((script, report)) => {
                log_header_report(&report);
                *state = HeaderState::Ready(AssTrackScript {
                    script,
                    tag_stats: AssTagStats::new(ass_limits),
                    seen_read_orders: HashSet::new(),
                });
                AssScriptAvailability::Ready
            }
            Err(error) => {
                *state = HeaderState::Unusable;
                AssScriptAvailability::JustRejected(error)
            }
        }
    }

    /// Забывает виденные `ReadOrder` дорожки (её кэш реплик начался заново). Заголовок и
    /// сценарий не трогаются: они приходят только с `MediaStarted` и всё ещё верны.
    pub(in crate::state::subtitles) fn forget_seen_read_orders(&mut self, track_id: TrackId) {
        if let Some(HeaderState::Ready(entry)) = self.headers.get_mut(&track_id) {
            entry.seen_read_orders.clear();
        }
    }

    /// Статистика тегов дорожки (для тестов и диагностики).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "статистику тегов читают тесты и будущая диагностика матрицы"
        )
    )]
    pub(in crate::state::subtitles) fn tag_stats(&self, track_id: TrackId) -> Option<&AssTagStats> {
        match self.headers.get(&track_id)? {
            HeaderState::Ready(entry) => Some(&entry.tag_stats),
            HeaderState::Pending(_) | HeaderState::Unusable => None,
        }
    }

    /// Превращает пакет в ASS-событие по разобранному сценарию дорожки; `None` — сценария
    /// нет (вызывающий берёт путь «только текст»; перед вызовом нужен `ensure_script`).
    ///
    /// `limits.max_remembered_read_orders` ограничивает множество виданных `ReadOrder` (то же
    /// число, что лимит реплик дорожки): сверх предела новые значения не запоминаются, а повтор
    /// тогда ловит таймлайн по полному совпадению.
    pub(in crate::state::subtitles) fn event_from_packet(
        &mut self,
        packet: &SubtitleFeedPacket,
        limits: AssPacketLimits,
    ) -> Option<PacketEventOutcome> {
        let Some(HeaderState::Ready(entry)) = self.headers.get_mut(&packet.track_id) else {
            return None;
        };
        let embedded = match parse_embedded_ass_event(
            &packet.payload,
            &entry.script,
            &limits.cue_parse,
            &limits.ass_parse,
            &mut entry.tag_stats,
        ) {
            Ok(embedded) => embedded,
            Err(error) => {
                return Some(PacketEventOutcome::Rejected(PacketRejection::Malformed(
                    error,
                )));
            }
        };
        let read_order = embedded.read_order;
        // Правило libass: событие с уже виданным `ReadOrder` игнорируется целиком.
        if entry.seen_read_orders.contains(&read_order) {
            return Some(PacketEventOutcome::DuplicateReadOrder);
        }
        // Блоки одних тегов и пустой текст рисовать нечем: не событие, но и не ошибка.
        // `ReadOrder` здесь не запоминается: событие не вставлено в кэш.
        if !embedded.has_visible_text() {
            return Some(PacketEventOutcome::Empty);
        }
        // libass: событие без длительности (или нулевой) невидимо (`end == start`), поэтому
        // пятисекундный запасной показ «только текста» для ASS не применяется.
        let Some(duration) = ass_event_duration(packet) else {
            return Some(PacketEventOutcome::ZeroDuration);
        };
        let Some((start, end)) = cue_interval(packet, duration) else {
            return Some(PacketEventOutcome::Rejected(
                PacketRejection::InvalidInterval,
            ));
        };
        let event = SubtitleEvent::new(
            start,
            end,
            embedded.layer,
            SubtitleContent::Ass(embedded.cue),
        );
        Some(match event {
            Ok(event) => {
                // Запоминаем только реально принятое событие (с учётом предела памяти).
                if entry.seen_read_orders.len() < limits.max_remembered_read_orders {
                    entry.seen_read_orders.insert(read_order);
                }
                PacketEventOutcome::Event(event)
            }
            Err(_) => PacketEventOutcome::Rejected(PacketRejection::InvalidInterval),
        })
    }
}

/// Пределы разбора одного ASS-пакета (собираются владельцем из `EmbeddedLimits`).
#[derive(Debug, Clone, Copy)]
pub(in crate::state::subtitles) struct AssPacketLimits {
    /// Пределы размера и длины пакета.
    pub(in crate::state::subtitles) cue_parse: EmbeddedCueLimits,
    /// Пределы разбора ASS (теги на реплику).
    pub(in crate::state::subtitles) ass_parse: AssParseLimits,
    /// Сколько `ReadOrder` запоминает дорожка.
    pub(in crate::state::subtitles) max_remembered_read_orders: usize,
}

/// Диагностика заголовка без его содержимого (в лог не попадают тексты и имена).
fn log_header_report(report: &AssHeaderReport) {
    if report.skipped_styles > 0 {
        tracing::warn!(
            skipped_styles = report.skipped_styles,
            "An ASS header had more styles than the limit; extra styles were skipped"
        );
    }
    if report.had_replacement_characters {
        tracing::debug!("An ASS header was not valid UTF-8; some characters were replaced");
    }
}
