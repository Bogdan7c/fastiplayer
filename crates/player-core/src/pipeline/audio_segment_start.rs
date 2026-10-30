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
//! Курсор отрезка. После первого PCM отрезок помнит media-время, в которое прозвучит
//! СЛЕДУЮЩИЙ sample: конец уже записанного звука по PTS packet-а и числу декодированных
//! frames (не по длительности из контейнера: у дыры в MP4 последний sample перед ней
//! «длится» до звука после неё, а декодер отдаёт обычный кадр). Session сравнивает с ним
//! PTS следующего packet-а и закрывает тишиной дыру посреди отрезка
//! (`session/audio_runtime/mid_stream_gap.rs`, дефект D3). Курсор живёт столько же,
//! сколько отрезок: перепривязки часов его не трогают, сбрасывает только новый отрезок.
//! Вставленная тишина курсор не двигает — он меряет только звук самого файла.
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

        /// Media-время, в которое прозвучит следующий sample (курсор отрезка, см. модуль).
        ///
        /// `None` — в output ушла только тишина начала отрезка, а сам packet ещё не учтён
        /// (или запись packet-а не удалась): сравнивать следующий packet не с чем.
        expected_next_sample: Option<Duration>,
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

/// Докуда в media-времени дошёл PCM, уже записанный в текущий отрезок звука.
///
/// Вопрос задаёт session перед тем, как начать отрезок заново в новой цели без нового
/// demux seek-а (forward extension живого скраба): записанный PCM при этом выбрасывается,
/// а demux его повторно не прочитает. Ветки не схлопнуты в `bool`: «звука нет», «PCM ещё
/// не записан» и «конец неизвестен» вызывающий код обрабатывает по-разному.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioSegmentWrittenReach {
    /// Аудиодорожка не выбрана: терять нечего.
    NoSelectedAudio,

    /// В текущий отрезок ещё не записан ни один PCM: очистка ничего не выбросит.
    NoPcmWritten,

    /// PCM отрезка записан; его media-конец — не раньше этой точки.
    ///
    /// Значение может быть завышено (конец прежнего отрезка не забывается очисткой
    /// под seek), но не занижено: для решения «не потерять звук» это безопасная сторона.
    EndsAt(Duration),

    /// PCM записан, но его конец забыт перепривязкой часов (seek commit, смена скорости,
    /// новый output): сколько звука лежит в output-е, неизвестно.
    EndUnknown,
}

impl PlaybackPipeline {
    /// Сообщает, докуда дошёл PCM текущего отрезка звука (см. [`AudioSegmentWrittenReach`]).
    ///
    /// Только чтение: ни отрезок, ни учёт записанного звука не меняются.
    #[must_use]
    pub(crate) fn audio_segment_written_reach(&self) -> AudioSegmentWrittenReach {
        if !self.has_selected_audio_track() {
            return AudioSegmentWrittenReach::NoSelectedAudio;
        }
        if self.audio_segment_awaits_first_pcm() {
            return AudioSegmentWrittenReach::NoPcmWritten;
        }
        match self.written_audio_end() {
            Some(written_end) => AudioSegmentWrittenReach::EndsAt(written_end),
            None => AudioSegmentWrittenReach::EndUnknown,
        }
    }

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
            AudioSegmentStart::FirstPcmWritten { first_sample, .. } => first_sample,
        }
    }

    /// Media-время, в которое прозвучит следующий записанный sample текущего отрезка.
    ///
    /// Это конец уже записанного звука файла (вставленная тишина не в счёт). `None` —
    /// отрезок ещё ждёт первый PCM (тогда следующий sample звучит в `media_clock_base`)
    /// или его packet ещё не учтён. Перепривязки часов (seek commit, скорость, новый
    /// output) значение не забывают — в отличие от `written_audio_end`.
    #[must_use]
    pub(crate) fn expected_next_audio_sample_time(&self) -> Option<Duration> {
        match self.audio_segment_start {
            AudioSegmentStart::AwaitingFirstPcm { .. } => None,
            AudioSegmentStart::FirstPcmWritten {
                expected_next_sample,
                ..
            } => expected_next_sample,
        }
    }

    /// Двигает курсор отрезка к media-концу только что записанного packet-а.
    ///
    /// Зовётся после успешной записи PCM packet-а (`note_audio_packet_written`). Курсор
    /// не отступает назад: перекрывающиеся packets (priming, повтор) его не сдвигают.
    /// Пока отрезок ждёт первый PCM, это no-op: записи не было, а следующий PCM всё
    /// равно ляжет в точку привязки часов.
    pub(super) fn advance_audio_segment_cursor(&mut self, written_pcm_end: Duration) {
        if let AudioSegmentStart::FirstPcmWritten {
            expected_next_sample,
            ..
        } = &mut self.audio_segment_start
        {
            *expected_next_sample = Some(
                expected_next_sample.map_or(written_pcm_end, |previous_end| {
                    previous_end.max(written_pcm_end)
                }),
            );
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
                // Конец звука учтёт `advance_audio_segment_cursor` после записи packet-а:
                // здесь мог пройти только кусок тишины начала отрезка.
                expected_next_sample: None,
            };
        }
    }
}
