//! «Нужна точка входа»: после выброшенного кадра видео ждём следующий ключевой.
//!
//! Кадр, ушедший в отрицательное время после `timestampOffset`, выбрасывается —
//! как в MSE. Но кадры после него ссылаются на него: без опорного кадра декодер
//! показал бы мусор. MSE в этом случае выбрасывает всё до следующей точки входа
//! («need random access point»), источник делает так же. Звук не трогаем: каждый
//! пакет звука сам по себе точка входа.

use std::collections::BTreeSet;

use media_core::{PacketKeyframe, TrackId, TrackKind};

/// Дорожки видео, которые ждут следующий ключевой кадр.
#[derive(Debug, Default)]
pub(crate) struct RandomAccessGate {
    /// Публичные ID дорожек видео после выброшенного кадра.
    awaiting_keyframe: BTreeSet<TrackId>,
}

/// Решение по пакету.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RandomAccessDecision {
    /// Пакет можно отдавать.
    Keep,
    /// Пакет зависит от выброшенного кадра — выбросить.
    Drop,
}

impl RandomAccessGate {
    /// Пакет выброшен (отрицательное время): дорожка видео ждёт ключевой кадр.
    pub(crate) fn note_dropped(&mut self, track: TrackId, kind: TrackKind) {
        if kind == TrackKind::Video {
            self.awaiting_keyframe.insert(track);
        }
    }

    /// Перемотка: дорожка видео ждёт ключевой кадр — новый эпизод начинается с
    /// начала сегмента, и не каждый сегмент обязан начинаться с ключевого кадра.
    pub(crate) fn require_keyframe(&mut self, track: TrackId) {
        self.awaiting_keyframe.insert(track);
    }

    /// Решение по пакету, который прошёл проверку времени.
    pub(crate) fn admit(
        &mut self,
        track: TrackId,
        kind: TrackKind,
        keyframe: PacketKeyframe,
    ) -> RandomAccessDecision {
        if kind != TrackKind::Video || !self.awaiting_keyframe.contains(&track) {
            return RandomAccessDecision::Keep;
        }
        if keyframe == PacketKeyframe::Keyframe {
            self.awaiting_keyframe.remove(&track);
            RandomAccessDecision::Keep
        } else {
            RandomAccessDecision::Drop
        }
    }
}

#[cfg(test)]
mod tests {
    use media_core::{PacketKeyframe, TrackId, TrackKind};

    use super::{RandomAccessDecision, RandomAccessGate};

    const VIDEO: TrackId = TrackId::new(1);
    const AUDIO: TrackId = TrackId::new(2);

    #[test]
    fn dependent_video_frames_wait_for_the_next_keyframe() {
        let mut gate = RandomAccessGate::default();
        assert_eq!(
            gate.admit(VIDEO, TrackKind::Video, PacketKeyframe::NotKeyframe),
            RandomAccessDecision::Keep
        );
        gate.note_dropped(VIDEO, TrackKind::Video);
        for keyframe in [PacketKeyframe::NotKeyframe, PacketKeyframe::Unknown] {
            assert_eq!(
                gate.admit(VIDEO, TrackKind::Video, keyframe),
                RandomAccessDecision::Drop
            );
        }
        assert_eq!(
            gate.admit(VIDEO, TrackKind::Video, PacketKeyframe::Keyframe),
            RandomAccessDecision::Keep
        );
        assert_eq!(
            gate.admit(VIDEO, TrackKind::Video, PacketKeyframe::NotKeyframe),
            RandomAccessDecision::Keep
        );
    }

    #[test]
    fn audio_is_never_held_back() {
        let mut gate = RandomAccessGate::default();
        gate.note_dropped(AUDIO, TrackKind::Audio);
        assert_eq!(
            gate.admit(AUDIO, TrackKind::Audio, PacketKeyframe::NotKeyframe),
            RandomAccessDecision::Keep
        );
    }
}
