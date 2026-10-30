//! Начало непрерывного отрезка звука: записан ли уже первый PCM после разрыва времени.
//!
//! Зачем это нужно. Часы звука привязаны так, что первый PCM, записанный в пустой
//! output, звучит ровно в `media_clock_base` (старт, цель seek-а, позиция смены
//! дорожки). Если данные звука начинаются позже базы, session перед первым PCM
//! вставляет тишину (`session/audio_runtime/leading_silence.rs`). Решать это можно только для
//! ПЕРВОГО PCM отрезка: дальше в output уже лежит звук, и следующий sample ляжет сразу
//! за ним, а не в базу.
//!
//! Почему не `written_audio_end`. Конец записанного звука забывается на каждой
//! перепривязке часов — в том числе на seek commit-е, смене скорости и пересоздании
//! output-а после потери устройства, когда в output уже лежит звук этого же отрезка.
//! Признак «первый PCM уже записан» такие перепривязки переживает. Он сбрасывается
//! только там, где следующий PCM действительно ляжет в точку привязки часов:
//! - очистка output-а под seek (`clear_audio_output_for_seek`). Само новое поколение
//!   seek-а отрезок НЕ начинает: выход из scrub без цели и rebase seek-а по
//!   `TracksChanged` меняют поколение, не трогая ring (rebase при этом удаляет
//!   output через `clear_audio_output`, и отрезок начинается уже там);
//! - удаление output-а (новое media, смена дорожки, отключение звука, новый список
//!   дорожек) — следующий output создаётся пустым;
//! - возврат звука после разрыва (`resume_audio_clock_after_gap`): output доиграл всё,
//!   а часы перепривязаны к текущей позиции.
//!
//! Вместе с признаком отрезок помнит media-время своего первого sample-а (его сообщает
//! session перед записью первого PCM). Audio-only seek коммитит позицию именно туда:
//! если звук цели начинается позже неё, позиция должна показывать время звучащего звука
//! (`session/seek_completion.rs`), а не цель.
//!
//! Владелец — [`PlaybackPipeline`]: он же владеет output-ом и маршрутизацией PCM в него.

use std::time::Duration;

use super::{AudioOutputRoutingStatus, PlaybackPipeline};

/// Состояние текущего непрерывного отрезка звука.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioSegmentStart {
    /// В output ещё не ушло ни одного PCM отрезка: следующий звучит в `media_clock_base`.
    AwaitingFirstPcm {
        /// Media-время первого sample-а, который session собирается записать первым
        /// (`None` — session его ещё не сообщила). Становится фактом только после записи.
        planned_first_sample: Option<Duration>,
    },

    /// PCM отрезка уже записан: новый ляжет следом за ним.
    FirstPcmWritten {
        /// Media-время первого прозвучавшего sample-а отрезка, если session его сообщила.
        first_sample: Option<Duration>,
    },
}

impl Default for AudioSegmentStart {
    /// Новый pipeline начинает с пустого отрезка: первый PCM ещё впереди.
    fn default() -> Self {
        Self::AwaitingFirstPcm {
            planned_first_sample: None,
        }
    }
}

impl PlaybackPipeline {
    /// `true`, пока в текущий отрезок звука не записан ни один PCM.
    ///
    /// Только в этом состоянии следующий PCM прозвучит в `media_clock_base`, поэтому
    /// только тогда session выравнивает его по PTS тишиной.
    #[must_use]
    pub(crate) fn audio_segment_awaits_first_pcm(&self) -> bool {
        matches!(
            self.audio_segment_start,
            AudioSegmentStart::AwaitingFirstPcm { .. }
        )
    }

    /// Сообщает media-время первого sample-а, который session сейчас запишет первым.
    ///
    /// Вне ожидания первого PCM — no-op: время уже начатого отрезка не переписывается.
    /// Фактом время становится только после успешной записи (`note_audio_segment_pcm_routed`).
    pub(crate) fn plan_audio_segment_first_sample(&mut self, first_sample: Duration) {
        if let AudioSegmentStart::AwaitingFirstPcm {
            planned_first_sample,
        } = &mut self.audio_segment_start
        {
            *planned_first_sample = Some(first_sample);
        }
    }

    /// Media-время первого записанного sample-а текущего отрезка.
    ///
    /// `None` — отрезок ещё ждёт первый PCM, либо session время не сообщала.
    #[must_use]
    pub(crate) fn audio_segment_first_sample(&self) -> Option<Duration> {
        match self.audio_segment_start {
            AudioSegmentStart::AwaitingFirstPcm { .. } => None,
            AudioSegmentStart::FirstPcmWritten { first_sample } => first_sample,
        }
    }

    /// Начинает новый отрезок звука: следующий PCM ляжет в точку привязки часов.
    ///
    /// Вызывается только на настоящих разрывах (см. модуль), а не на каждой
    /// перепривязке часов.
    pub(super) fn begin_audio_segment(&mut self) {
        self.audio_segment_start = AudioSegmentStart::default();
    }

    /// Отмечает, что PCM отрезка ушёл в output (или в tempo processor перед ним).
    ///
    /// Отказ записи и отсутствие output-а ничего не меняют: такой PCM никуда не попал,
    /// а ошибку обрабатывает session. Запланированное время первого sample-а становится
    /// временем отрезка только здесь, на первой успешной записи.
    pub(super) fn note_audio_segment_pcm_routed(
        &mut self,
        write_status: &AudioOutputRoutingStatus,
    ) {
        if !matches!(write_status, AudioOutputRoutingStatus::Written(_)) {
            return;
        }
        if let AudioSegmentStart::AwaitingFirstPcm {
            planned_first_sample,
        } = self.audio_segment_start
        {
            self.audio_segment_start = AudioSegmentStart::FirstPcmWritten {
                first_sample: planned_first_sample,
            };
        }
    }
}
