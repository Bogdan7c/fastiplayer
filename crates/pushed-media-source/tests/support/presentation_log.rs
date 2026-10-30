//! Журнал показанных кадров: каждый показ плеера — ровно одна запись.
//!
//! Плеер сообщает за шаг только ЧИСЛО показанных кадров
//! (`PlayerTickResult::video_frames_presented`) и кадр, стоящий на экране в конце
//! шага (`current_video_frame`). Если шаг затянулся, планировщик может показать
//! за него два кадра подряд — первый тут же вытесняется вторым. Раньше обвязка
//! записывала только кадр на экране и теряла вытесненный, и тест «каждый кадр по
//! порядку» падал, хотя плеер кадр показал.
//!
//! Время вытесненного кадра восстанавливается из того, что плеер сообщает за
//! шаг: времена прочитанных пакетов видео (`demuxed_packets`) минус выброшенные
//! кадры (`dropped_video_frames`). Плеер показывает кадры по возрастанию времени,
//! поэтому вытесненные кадры шага — ближайшие ждущие времена перед кадром на
//! экране. Если таких времён не хватило, недостача считается в
//! [`PresentationLog::unattributed_presentations`] — тест видит, что обвязка не
//! смогла назвать кадр, вместо молчаливой потери.

use std::collections::BTreeSet;
use std::time::Duration;

use media_core::TrackKind;
use player_core::{PlayerSnapshot, PlayerTickResult};

/// Каким обвязка увидела показанный кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSighting {
    /// Кадр стоял на экране в конце шага (его и взял бы рендер окна).
    OnScreen,
    /// Кадр показан и вытеснен следующим внутри того же шага: время восстановлено
    /// по пакетам шага, размер — кадра на экране (свой размер обвязка не видит).
    SupersededWithinTick,
}

/// Показанный кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedFrame {
    /// Время кадра.
    pub pts: Duration,
    /// Размер кадра.
    pub width: u32,
    /// Размер кадра.
    pub height: u32,
    /// Позиция плеера (часы звука) в момент показа.
    pub position: Duration,
    /// Шаг, на котором кадр показан.
    pub tick: usize,
    /// Стоял ли кадр на экране в конце шага.
    pub sighting: FrameSighting,
}

/// Журнал показов за прогон.
#[derive(Debug, Default)]
pub struct PresentationLog {
    /// Показанные кадры по порядку показа.
    frames: Vec<PresentedFrame>,
    /// Времена прочитанных пакетов видео, кадры которых ещё не показаны и не
    /// выброшены (кандидаты на «вытесненный внутри шага»).
    pending_video_pts: BTreeSet<Duration>,
    /// Показы, которым обвязка не нашла времени.
    unattributed_presentations: u64,
}

impl PresentationLog {
    /// Учитывает итог шага и снимок после него.
    pub fn observe_tick(
        &mut self,
        tick: usize,
        result: &PlayerTickResult,
        snapshot: &PlayerSnapshot,
    ) {
        // Половина кадра — допуск совпадения времени кадра с временем пакета
        // (декодер пересчитывает время из своей шкалы; не меньше 1 мс).
        let margin = (snapshot.video_frame_duration_estimate / 2).max(Duration::from_millis(1));
        self.note_demuxed_video(result);
        self.forget_dropped(result, margin);
        let presented = result.video_frames_presented;
        let Some(on_screen) = &snapshot.current_video_frame else {
            // Показ без кадра на экране обвязка назвать не может.
            self.unattributed_presentations += presented;
            return;
        };
        let screen_frame = PresentedFrame {
            pts: on_screen.pts,
            width: on_screen.width,
            height: on_screen.height,
            position: snapshot.current_position,
            tick,
            sighting: FrameSighting::OnScreen,
        };
        if presented == 0 {
            // Счётчик молчит, а кадр на экране сменился (путь показа без счёта):
            // прежнее правило обвязки — записать новый кадр.
            if self
                .frames
                .last()
                .is_none_or(|last| last.pts != screen_frame.pts)
            {
                self.record(screen_frame, margin);
            }
            return;
        }
        let superseded = usize::try_from(presented - 1).unwrap_or(usize::MAX);
        self.record_superseded(screen_frame, superseded, margin);
        self.record(screen_frame, margin);
    }

    /// Команда перемотки: прочитанное старой позиции плеер выбросит без отчёта о
    /// выброшенных кадрах, поэтому кандидаты старой позиции забываются.
    pub fn forget_pending_for_seek(&mut self) {
        self.pending_video_pts.clear();
    }

    /// Показан ли уже хоть один кадр.
    pub fn has_frames(&self) -> bool {
        !self.frames.is_empty()
    }

    /// Показанные кадры и число показов без названного времени.
    pub fn finish(self) -> (Vec<PresentedFrame>, u64) {
        (self.frames, self.unattributed_presentations)
    }

    /// Запоминает времена прочитанных за шаг пакетов видео.
    fn note_demuxed_video(&mut self, result: &PlayerTickResult) {
        self.pending_video_pts.extend(
            result
                .demuxed_packets
                .iter()
                .filter(|packet| packet.kind == TrackKind::Video)
                .map(|packet| packet.pts),
        );
    }

    /// Выброшенные за шаг кадры больше не кандидаты на показ.
    fn forget_dropped(&mut self, result: &PlayerTickResult, margin: Duration) {
        for dropped in &result.dropped_video_frames {
            if let Some(nearest) = self.pending_near(dropped.pts, margin) {
                self.pending_video_pts.remove(&nearest);
            }
        }
    }

    /// Записывает `count` кадров, вытесненных кадром `screen_frame` за этот шаг:
    /// ближайшие ждущие времена перед ним (после последнего записанного кадра,
    /// если плеер шёл вперёд).
    fn record_superseded(&mut self, screen_frame: PresentedFrame, count: usize, margin: Duration) {
        if count == 0 {
            return;
        }
        let after = self
            .frames
            .last()
            .map(|last| last.pts)
            .filter(|last| *last < screen_frame.pts)
            .map_or(Duration::ZERO, |last| last + margin);
        let before = screen_frame.pts.saturating_sub(margin);
        let candidates: Vec<Duration> = if after < before {
            self.pending_video_pts
                .range(after..before)
                .rev()
                .take(count)
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        self.unattributed_presentations += (count - candidates.len()) as u64;
        // Кандидаты собраны от кадра на экране назад — записываем по времени.
        for pts in candidates.into_iter().rev() {
            self.frames.push(PresentedFrame {
                pts,
                sighting: FrameSighting::SupersededWithinTick,
                ..screen_frame
            });
        }
    }

    /// Записывает показ; всё, что раньше показанного кадра, больше не покажется
    /// (плеер показывает по возрастанию времени) и перестаёт быть кандидатом.
    fn record(&mut self, frame: PresentedFrame, margin: Duration) {
        self.pending_video_pts = self.pending_video_pts.split_off(&(frame.pts + margin));
        self.frames.push(frame);
    }

    /// Ждущее время пакета в пределах `margin` от `pts`.
    fn pending_near(&self, pts: Duration, margin: Duration) -> Option<Duration> {
        self.pending_video_pts
            .range(pts.saturating_sub(margin)..=pts + margin)
            .min_by_key(|candidate| candidate.abs_diff(pts))
            .copied()
    }
}
