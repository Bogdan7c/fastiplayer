//! Выходной фронт video decoder-а: какой кадр decoder выдаст следующим.
//!
//! Зачем это нужно. Окно decode-ahead (`decode_ahead_target_ms`/`max_decode_ahead_ms`)
//! должно ограничивать готовые кадры, которые убежали вперёд звука. Раньше оно мерило
//! опережение по PTS очередного *отправляемого packet-а*. Для decoder-а без задержки
//! выхода (VA-API без reorder) это одно и то же. Но frame-threaded FFmpeg с N потоками
//! (+ задержка reorder `has_b_frames`) выдаёт кадр k только после ~N следующих packets:
//! packets внутри его конвейера ещё не могут дать кадр, но уже «съедали» окно. При
//! `N × длительность кадра` больше окна decoder голодал, а кадры приходили поздно.
//!
//! Чем этот фронт отличается от `video_decode_in_flight_packets`. In-flight — packets,
//! которые decoder thread ещё не *принял* (ack приходит после `avcodec_send_packet`).
//! Packets, которые FFmpeg уже принял в свой конвейер потоков, in-flight не видит — их
//! видно только по тому, что кадров на выходе пока нет. Поэтому фронт строится по
//! наблюдаемому выходу: последний полученный кадр + сколько packets ушло после него.
//!
//! Владелец — `PlaybackPipeline`, тот же уровень, что и in-flight accounting: фронт живёт
//! в той же «эпохе decoder-а» и сбрасывается вместе с ним (seek, flush, замена decoder-а,
//! fatal error, новый media) через `reset_video_decode_in_flight`.

use std::time::Duration;

use super::{
    MAX_OBSERVED_VIDEO_FRAME_DURATION, MIN_OBSERVED_VIDEO_FRAME_DURATION, PlaybackPipeline,
};

/// Наблюдаемый выход decoder-а в текущей эпохе decoder-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VideoDecoderOutputFrontier {
    /// PTS последнего кадра, который decoder выдал в этой эпохе.
    ///
    /// `None` — кадров ещё не было: прогнозировать выход не по чему.
    latest_output_pts: Option<Duration>,

    /// Сколько packets ушло в decoder после последнего полученного кадра.
    ///
    /// Каждый такой packet (в установившемся режиме) освобождает на выходе ещё один кадр,
    /// поэтому прогноз следующего кадра сдвигается на кадр за каждый отправленный packet —
    /// и внутри одного send-прохода окно не пропускает лишнего.
    packets_submitted_since_latest_output: usize,

    /// Шаг PTS между двумя последними кадрами выхода, если он похож на длительность кадра.
    ///
    /// Нужен как нижняя граница шага прогноза: сглаженная оценка pipeline стартует с 60 fps
    /// и первые ~0,5 с занижает длительность кадра у 24/30 fps, а заниженный шаг пропускал бы
    /// кадр сверх окна. Скачки PTS (склейка, пропуск) сюда не попадают.
    latest_output_spacing: Option<Duration>,
}

impl VideoDecoderOutputFrontier {
    /// Отмечает packet, принятый decoder boundary.
    fn note_packet_submitted(&mut self) {
        self.packets_submitted_since_latest_output =
            self.packets_submitted_since_latest_output.saturating_add(1);
    }

    /// Отмечает кадр, который decoder реально выдал в текущей эпохе.
    ///
    /// Берётся именно последний кадр, а не максимум: при скачке PTS назад (склейка потока)
    /// фронт обязан пойти за новым временем, а не залипнуть на старом большом PTS.
    fn note_output_frame(&mut self, pts: Duration) {
        self.latest_output_spacing = self
            .latest_output_pts
            .and_then(|previous_pts| pts.checked_sub(previous_pts))
            .filter(|spacing| {
                (MIN_OBSERVED_VIDEO_FRAME_DURATION..=MAX_OBSERVED_VIDEO_FRAME_DURATION)
                    .contains(spacing)
            });
        self.latest_output_pts = Some(pts);
        self.packets_submitted_since_latest_output = 0;
    }

    /// Забывает выход прошлой эпохи decoder-а.
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Прогноз PTS кадра, который выйдет из decoder-а, если отправить ещё один packet.
    ///
    /// `frame_duration` — текущая оценка длительности кадра; шаг прогноза не меньше
    /// последнего наблюдаемого шага выхода (заниженный шаг пропускал бы лишний кадр).
    /// `None`, пока в этой эпохе не было ни одного кадра.
    fn projected_next_output_pts(self, frame_duration: Duration) -> Option<Duration> {
        let latest_output_pts = self.latest_output_pts?;
        let frame_duration = self
            .latest_output_spacing
            .map_or(frame_duration, |spacing| spacing.max(frame_duration));
        // Следующий кадр — через один кадр после последнего, плюс по кадру за каждый packet,
        // уже отправленный после него (их кадры выйдут раньше).
        let frames_until_projected_output =
            self.packets_submitted_since_latest_output.saturating_add(1);
        let frames_until_projected_output =
            u32::try_from(frames_until_projected_output).unwrap_or(u32::MAX);
        let projected_distance = frame_duration.saturating_mul(frames_until_projected_output);

        Some(latest_output_pts.saturating_add(projected_distance))
    }
}

impl PlaybackPipeline {
    /// Отмечает packet, принятый decoder boundary, для прогноза выхода decoder-а.
    pub(super) fn note_video_packet_submitted_to_output_frontier(&mut self) {
        self.video_decoder_output_frontier.note_packet_submitted();
    }

    /// Сбрасывает прогноз выхода вместе с остальным accounting эпохи decoder-а.
    pub(super) fn reset_video_decoder_output_frontier(&mut self) {
        self.video_decoder_output_frontier.reset();
    }

    /// Отмечает кадр текущего поколения, полученный от decoder-а.
    ///
    /// Вызывать для каждого принятого кадра, даже если дальше он будет отброшен (seek
    /// preroll, playback window): это всё равно выход decoder-а.
    pub(crate) fn note_video_decoder_output_frame(&mut self, pts: Duration) {
        self.video_decoder_output_frontier.note_output_frame(pts);
    }

    /// PTS последнего кадра, который decoder выдал в текущей эпохе (с момента последнего
    /// seek/flush). Это фактический прогресс декодирования, а не желаемая цель.
    ///
    /// `None` — в текущей эпохе decoder-а кадров ещё не было.
    #[must_use]
    pub(crate) fn latest_video_decoder_output_pts(&self) -> Option<Duration> {
        self.video_decoder_output_frontier.latest_output_pts
    }

    /// Прогноз PTS следующего кадра на выходе decoder-а, если отправить ещё один packet.
    ///
    /// `None` — в текущей эпохе decoder-а кадров ещё не было.
    #[must_use]
    pub(crate) fn projected_next_video_decoder_output_pts(&self) -> Option<Duration> {
        self.video_decoder_output_frontier
            .projected_next_output_pts(self.video_frame_duration_estimate())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Длительность кадра 30 fps для прогноза.
    const FRAME: Duration = Duration::from_micros(33_333);

    #[test]
    fn frontier_without_output_has_no_projection() {
        let mut frontier = VideoDecoderOutputFrontier::default();
        frontier.note_packet_submitted();
        frontier.note_packet_submitted();

        assert_eq!(frontier.projected_next_output_pts(FRAME), None);
    }

    #[test]
    fn projection_is_one_frame_after_latest_output() {
        let mut frontier = VideoDecoderOutputFrontier::default();
        frontier.note_output_frame(Duration::from_secs(2));

        assert_eq!(
            frontier.projected_next_output_pts(FRAME),
            Some(Duration::from_secs(2) + FRAME)
        );
    }

    #[test]
    fn each_packet_after_latest_output_moves_projection_by_one_frame() {
        let mut frontier = VideoDecoderOutputFrontier::default();
        frontier.note_output_frame(Duration::from_secs(1));
        for _ in 0..3 {
            frontier.note_packet_submitted();
        }

        assert_eq!(
            frontier.projected_next_output_pts(FRAME),
            Some(Duration::from_secs(1) + FRAME * 4)
        );
    }

    #[test]
    fn new_output_restarts_projection_from_that_frame() {
        let mut frontier = VideoDecoderOutputFrontier::default();
        frontier.note_output_frame(Duration::from_secs(1));
        frontier.note_packet_submitted();
        frontier.note_packet_submitted();
        // Скачок PTS назад: фронт идёт за последним кадром, а не за максимумом.
        frontier.note_output_frame(Duration::from_millis(500));

        assert_eq!(
            frontier.projected_next_output_pts(FRAME),
            Some(Duration::from_millis(500) + FRAME)
        );
    }

    #[test]
    fn observed_output_spacing_bounds_underestimated_frame_duration() {
        let mut frontier = VideoDecoderOutputFrontier::default();
        frontier.note_output_frame(Duration::from_secs(1));
        frontier.note_output_frame(Duration::from_secs(1) + FRAME);
        // Оценка pipeline ещё на bootstrap 60 fps, а выход идёт с шагом 30 fps.
        let underestimated = FRAME / 2;

        assert_eq!(
            frontier.projected_next_output_pts(underestimated),
            Some(Duration::from_secs(1) + FRAME * 2)
        );
    }

    #[test]
    fn pts_jump_is_not_taken_as_output_spacing() {
        let mut frontier = VideoDecoderOutputFrontier::default();
        frontier.note_output_frame(Duration::from_secs(1));
        // Пропуск 10 с в потоке — не длительность кадра.
        frontier.note_output_frame(Duration::from_secs(11));

        assert_eq!(
            frontier.projected_next_output_pts(FRAME),
            Some(Duration::from_secs(11) + FRAME)
        );
    }

    #[test]
    fn reset_forgets_previous_decoder_epoch() {
        let mut frontier = VideoDecoderOutputFrontier::default();
        frontier.note_output_frame(Duration::from_secs(5));
        frontier.note_packet_submitted();

        frontier.reset();

        assert_eq!(frontier, VideoDecoderOutputFrontier::default());
        assert_eq!(frontier.projected_next_output_pts(FRAME), None);
    }

    #[test]
    fn projection_saturates_instead_of_overflowing() {
        let mut frontier = VideoDecoderOutputFrontier::default();
        frontier.note_output_frame(Duration::MAX - FRAME);
        frontier.note_packet_submitted();

        assert_eq!(
            frontier.projected_next_output_pts(FRAME),
            Some(Duration::MAX)
        );
    }

    #[test]
    fn pipeline_reset_of_decode_in_flight_also_resets_output_frontier() {
        let mut pipeline = PlaybackPipeline::default();
        pipeline.note_video_decoder_output_frame(Duration::from_secs(3));
        pipeline.note_video_packet_sent_to_decoder();
        assert!(pipeline.projected_next_video_decoder_output_pts().is_some());

        pipeline.reset_video_decode_in_flight();

        assert_eq!(pipeline.projected_next_video_decoder_output_pts(), None);
        assert_eq!(pipeline.video_decode_in_flight_packets(), 0);
    }

    #[test]
    fn pipeline_packet_send_moves_projection_by_estimated_frame_duration() {
        let mut pipeline = PlaybackPipeline::default();
        pipeline.note_video_decoder_output_frame(Duration::from_secs(3));
        let frame_duration = pipeline.video_frame_duration_estimate();
        let before = pipeline.projected_next_video_decoder_output_pts();

        pipeline.note_video_packet_sent_to_decoder();

        assert_eq!(before, Some(Duration::from_secs(3) + frame_duration));
        assert_eq!(
            pipeline.projected_next_video_decoder_output_pts(),
            Some(Duration::from_secs(3) + frame_duration * 2)
        );
        // In-flight accounting не изменилось по смыслу: packet всё ещё ждёт ack.
        assert_eq!(pipeline.video_decode_in_flight_packets(), 1);
    }
}
