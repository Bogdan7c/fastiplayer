//! Есть ли в pipeline уже прочитанное, но ещё не доигранное.
//!
//! Вопрос задаёт session, когда источник умер посреди файла (общий бэклог п. 24):
//! доигрывать есть смысл только то, что уже лежит у плеера. Ответ собирается из
//! очередей, которыми владеет pipeline, поэтому живёт здесь, а session не читает
//! эти очереди по отдельности.
//!
//! Ответ разделён на две части, потому что они по-разному переживают перемотку:
//! демуксированное media перемотка очищает (дальше в нём только текущий отрезок),
//! а хвост звукового выхода после очистки ещё может доигрывать старый отрезок.

use super::PlaybackPipeline;

impl PlaybackPipeline {
    /// Возвращает `true`, если у плеера есть прочитанное и ещё не показанное media
    /// текущего отрезка (после последнего seek/flush).
    ///
    /// Учитываются:
    /// - считанные, но не декодированные пакеты звука и видео;
    /// - пакеты, отданные video decoder-у и ещё не подтверждённые;
    /// - готовые кадры в очереди показа;
    /// - кадры, которые decoder мог придержать у себя (B-кадры, DPB, потоки FFmpeg):
    ///   так бывает только после того, как decoder получил ключевой кадр отрезка.
    ///
    /// `false` означает «из демуксера ничего не дошло»: например, источник отказал
    /// на самом первом чтении после открытия или перемотки.
    #[must_use]
    pub(crate) fn has_unplayed_demuxed_media(&self) -> bool {
        self.pending_audio_packet_len() > 0
            || self.video_decoder_has_input_work()
            || self.video_present_queue_len() > 0
            || self.video_decoder_may_hold_decoded_frames()
    }

    /// Возвращает `true`, если в звуковом выходе есть записанный и ещё не
    /// прозвучавший PCM (ring buffer или уже отданный устройству хвост).
    #[must_use]
    pub(crate) fn has_unplayed_audio_output_tail(&self) -> bool {
        self.audio_output_unplayed_tail_ms()
            .is_some_and(|unplayed_tail_ms| unplayed_tail_ms > 0.0)
    }

    /// Мог ли активный video decoder придержать у себя ещё не отданные кадры.
    ///
    /// После flush/bootstrap decoder ждёт ключевой кадр и ничего не держит; как только
    /// он принял ключевой кадр, кадры текущего отрезка могут сидеть внутри него до
    /// EOF/DPB drain-а. Точного счётчика у decoder boundary нет, поэтому ответ
    /// консервативный: «мог» достаточно, чтобы не выбросить хвост.
    fn video_decoder_may_hold_decoded_frames(&self) -> bool {
        self.has_selected_video_track()
            && self.has_active_video_decoder()
            && !self.video_decoder_needs_keyframe()
    }
}
