//! Когда и с каким образцом текста просить поток-рисовальщик заранее подготовить шрифты.
//!
//! Первая реплика с CJK-символами платит за чтение системного шрифта с диска. Чтобы заплатить
//! раньше, презентатор, как только дорожка попала в зеркало потока, отправляет короткий
//! образец её «непростого» текста. Поток выполнит прогрев в простое (см.
//! `render_worker::font_warm_up`).
//!
//! Что считается «непростым» текстом: символы выше `U+024F` (латиница и расширения покрыты
//! встроенными шрифтами, прогревать их незачем). Образец ограничен и по числу просмотренных
//! реплик, и по числу различных символов: прогрев нужен для первых реплик, а не для всей
//! дорожки (остальные лица подгрузятся обычным путём).

use std::collections::BTreeSet;

use subtitle_core::{AssTextItem, SubtitleContent, SubtitleTimeline};
use subtitle_raster::CueSourceId;

use super::super::active_track::ActiveSubtitleTrack;
use super::super::render_worker::commands::WorkerCommand;

/// Сколько первых реплик дорожки просматривается при сборе образца.
const SAMPLE_EVENT_LIMIT: usize = 32;
/// Сколько различных символов попадает в образец.
const SAMPLE_CHAR_LIMIT: usize = 64;
/// Первый кодпоинт, который встроенные шрифты уже не обязаны покрывать (после Latin Extended-B).
const FIRST_NON_LATIN_CODEPOINT: u32 = 0x0250;

/// Решает, нужно ли отправлять запрос на прогрев для показываемой дорожки.
#[derive(Debug, Default)]
pub(super) struct FontWarmUpGate {
    /// Дорожка, для которой запрос уже отправлен (или прогревать нечего).
    settled_source: Option<CueSourceId>,
}

impl FontWarmUpGate {
    /// Команда прогрева для дорожки или `None`: уже отправлено, образец пока пуст
    /// (встроенная дорожка ещё растёт: решим при следующем дописывании) или прогревать нечего.
    pub(super) fn command_for(&mut self, track: &ActiveSubtitleTrack) -> Option<WorkerCommand> {
        let source = track.source_id();
        if self.settled_source == Some(source) {
            return None;
        }
        let sample = collect_sample(track.timeline());
        if sample.is_empty() {
            // Просмотрены все первые реплики и непростого текста нет: дальше искать незачем.
            if track.timeline().len() >= SAMPLE_EVENT_LIMIT {
                self.settled_source = Some(source);
            }
            return None;
        }
        self.settled_source = Some(source);
        Some(WorkerCommand::WarmUpFonts {
            source,
            sample,
            language: track.language().cloned(),
        })
    }

    /// Зеркала потока забыты (новый поток или смена media): запрос нужно отправить заново.
    pub(super) fn reset(&mut self) {
        self.settled_source = None;
    }
}

/// Собирает образец: различные символы выше `U+024F` из первых реплик дорожки.
fn collect_sample(timeline: &SubtitleTimeline) -> String {
    let mut chars: BTreeSet<char> = BTreeSet::new();
    let mut remember = |text: &str| {
        for ch in text.chars() {
            if chars.len() >= SAMPLE_CHAR_LIMIT {
                return;
            }
            if u32::from(ch) >= FIRST_NON_LATIN_CODEPOINT && !ch.is_whitespace() {
                chars.insert(ch);
            }
        }
    };
    for (_, event) in timeline.iter().take(SAMPLE_EVENT_LIMIT) {
        match event.content() {
            SubtitleContent::Text(cue) => remember(&cue.text.plain_text()),
            SubtitleContent::Ass(cue) => {
                for item in &cue.text {
                    if let AssTextItem::Text(text) = item {
                        remember(text);
                    }
                }
            }
            // Картинки (PGS) шрифтов не используют; будущие виды содержимого — тоже.
            _ => {}
        }
    }
    chars.into_iter().collect()
}
