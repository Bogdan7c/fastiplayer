//! «Шов» между эпизодами полосы: повтор уже выданного времени плееру не уходит.
//!
//! Полка подаёт сегмент, который «перешагивает» курсор (другая нарезка того же
//! времени — например, страница сменила качество и прислала B[6, 10] после
//! поданных A[0, 4], A[4, 8]), в НОВОМ эпизоде (`shelf::LaneShelf::next_to_feed`).
//! Новый демуксер честно выдаст кадры B с 6 с — а плеер уже получил кадры A до
//! 8 с. Время назад без перемотки ломает плеер (повтор кадров, звук дважды).
//!
//! Поэтому демуксер полосы помнит, докуда дошли дорожки ([`EpisodeSeam::note_emitted`]),
//! и после смены эпизода БЕЗ перемотки ([`EpisodeSeam::arm`]) отсекает пакеты не
//! позже этой границы («пол»):
//! - звук (и прочее, кроме видео) — каждый пакет сам по себе точка входа: пакет
//!   с временем после пола проходит, и пол снимается;
//! - видео — кадры после ключевого зависят от него: пол снимается только
//!   ключевым кадром после пола, всё до него выбрасывается (как MSE «need random
//!   access point»).
//!
//! Перемотка ([`EpisodeSeam::reset`]) сбрасывает и пол, и память: после неё время
//! законно начинается заново.

use std::collections::BTreeMap;
use std::time::Duration;

use media_core::{PacketKeyframe, TrackId, TrackKind};

/// Решение по пакету нового эпизода.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeamDecision {
    /// Пакет можно отдавать.
    Keep,
    /// Пакет повторяет выданное время (или зависит от выброшенного) — выбросить.
    Drop,
}

/// Пол и память выданного времени по дорожкам полосы (публичные ID).
#[derive(Debug, Default)]
pub(crate) struct EpisodeSeam {
    /// Самое позднее выданное время показа по дорожкам.
    last_emitted: BTreeMap<TrackId, Duration>,
    /// Действующие полы: пакеты не позже пола выбрасываются.
    floors: BTreeMap<TrackId, Duration>,
}

impl EpisodeSeam {
    /// Пакет выдан плееру.
    pub(crate) fn note_emitted(&mut self, track: TrackId, pts: Duration) {
        self.last_emitted
            .entry(track)
            .and_modify(|latest| *latest = (*latest).max(pts))
            .or_insert(pts);
    }

    /// Начался следующий эпизод без перемотки: пол = выданное до сих пор.
    pub(crate) fn arm(&mut self) {
        self.floors.clone_from(&self.last_emitted);
    }

    /// Перемотка: время начинается заново, полов и памяти нет.
    pub(crate) fn reset(&mut self) {
        self.last_emitted.clear();
        self.floors.clear();
    }

    /// Решение по пакету (правила — в описании модуля).
    pub(crate) fn admit(
        &mut self,
        track: TrackId,
        kind: TrackKind,
        pts: Duration,
        keyframe: PacketKeyframe,
    ) -> SeamDecision {
        let Some(floor) = self.floors.get(&track).copied() else {
            return SeamDecision::Keep;
        };
        let after_floor = pts > floor;
        let opens = match kind {
            TrackKind::Video => after_floor && keyframe == PacketKeyframe::Keyframe,
            _ => after_floor,
        };
        if opens {
            self.floors.remove(&track);
            SeamDecision::Keep
        } else {
            SeamDecision::Drop
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::{PacketKeyframe, TrackId, TrackKind};

    use super::{EpisodeSeam, SeamDecision};

    const VIDEO: TrackId = TrackId::new(1);
    const AUDIO: TrackId = TrackId::new(2);

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn repeated_time_is_dropped_until_audio_passes_the_floor_and_video_gets_a_keyframe() {
        let mut seam = EpisodeSeam::default();
        seam.note_emitted(VIDEO, ms(360));
        seam.note_emitted(AUDIO, ms(380));
        seam.arm();
        let video = |seam: &mut EpisodeSeam, at: u64, keyframe| {
            seam.admit(VIDEO, TrackKind::Video, ms(at), keyframe)
        };
        // Ключевой кадр в прошлом и кадры после него (зависят от выброшенного).
        assert_eq!(
            video(&mut seam, 300, PacketKeyframe::Keyframe),
            SeamDecision::Drop
        );
        assert_eq!(
            video(&mut seam, 400, PacketKeyframe::NotKeyframe),
            SeamDecision::Drop
        );
        assert_eq!(
            video(&mut seam, 500, PacketKeyframe::Keyframe),
            SeamDecision::Keep
        );
        assert_eq!(
            video(&mut seam, 540, PacketKeyframe::NotKeyframe),
            SeamDecision::Keep
        );
        // Звук: ровно выданное время — повтор; дальше — проходит.
        assert_eq!(
            seam.admit(AUDIO, TrackKind::Audio, ms(380), PacketKeyframe::Keyframe),
            SeamDecision::Drop
        );
        assert_eq!(
            seam.admit(AUDIO, TrackKind::Audio, ms(400), PacketKeyframe::Keyframe),
            SeamDecision::Keep
        );
    }

    #[test]
    fn seek_reset_lets_time_start_over() {
        let mut seam = EpisodeSeam::default();
        seam.note_emitted(VIDEO, ms(800));
        seam.reset();
        seam.arm();
        assert_eq!(
            seam.admit(VIDEO, TrackKind::Video, ms(0), PacketKeyframe::NotKeyframe),
            SeamDecision::Keep
        );
    }
}
