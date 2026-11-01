use std::time::{Duration, Instant};

use media_core::DemuxRetryHint;

use crate::seek_state::PlaybackResumeIntent;
use crate::{MediaInstanceId, PlaybackState, PlayerResult};

use super::PlayerSession;

/// Точная installed-generation, которой принадлежит ожидание следующего demux read-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InstalledDemuxRetryFence {
    /// Exact media instance не позволяет deadline пережить замену source-а.
    media_instance_id: MediaInstanceId,

    /// Seek generation не позволяет читать по deadline от старой demux position.
    seek_generation: u64,
}

/// Одно bounded ожидание readiness для установленного media.
#[derive(Debug, Clone, Copy)]
struct InstalledDemuxRetry {
    /// Lifecycle fence текущего ожидания.
    fence: InstalledDemuxRetryFence,

    /// Самый ранний момент, когда demuxer разрешил повторный read.
    retry_deadline: Instant,

    /// Пользовательское намерение, которое нельзя потерять при реальном underrun-е.
    resume_intent: PlaybackResumeIntent,

    /// Был ли runtime переведён в существующий `Buffering` именно этим ожиданием.
    entered_buffering: bool,
}

/// Доказанная причина, по которой temporary demux readiness уже не оставляет runway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DemuxBufferingCause {
    /// Выбранный audio track не доживёт до следующего разрешённого demux read-а.
    SelectedAudioRunwayWillExpireBeforeRetry,

    /// Для media без audio полностью закончилась вся downstream video работа.
    DownstreamFullyDrained,
}

/// Настраиваемый запас на один scheduling/callback turn перед demux retry.
///
/// Значение приходит из существующего audio demux low-water policy. Newtype не
/// позволяет случайно перепутать этот запас с retry deadline или PCM runway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DemuxAudioStarvationMargin(Duration);

impl DemuxAudioStarvationMargin {
    /// Переводит существующий миллисекундный low-water config в bounded duration.
    #[must_use]
    pub(super) fn from_low_water_mark_ms(low_water_mark_ms: f64) -> Self {
        let sanitized_low_water_mark_ms =
            super::tick::sanitize_audio_demux_low_water_mark(low_water_mark_ms);
        let margin = duration_from_non_negative_millis(sanitized_low_water_mark_ms);
        Self(margin)
    }
}

/// Именованный расчёт, успеет ли output runway пережить temporary source wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectedAudioRunwayBudget {
    /// Реально доступный decoded PCM в output ring.
    usable_runway: Duration,

    /// Оставшееся время до следующего разрешённого demux read-а.
    remaining_retry_wait: Duration,

    /// Конфигурируемый запас на scheduler turn и очередной output callback.
    scheduler_callback_margin: DemuxAudioStarvationMargin,
}

impl SelectedAudioRunwayBudget {
    /// Возвращает true до фактической тишины, если runway уже не покрывает wait+margin.
    fn will_expire_before_retry(self) -> bool {
        let required_runway = self
            .remaining_retry_wait
            .saturating_add(self.scheduler_callback_margin.0);
        self.usable_runway <= required_runway
    }
}

/// Когда источник последний раз ответил «данных пока нет — их несёт живой поставщик»
/// ([`media_core::DemuxDataWait::LiveProducer`]) во время перемотки и когда его счётчик
/// принесённого последний раз рос (решение 151).
///
/// Отметка привязана только к экземпляру media, а не к seek generation: смена
/// описания дорожек посреди ожидания меняет generation, но не отменяет того, что
/// источник честно ждал страницу. Принадлежность текущей перемотке проверяет
/// потребитель (`seek_completion::live_source_wait`): моменты раньше начала этой
/// перемотки он не засчитывает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LiveProducerWaitMark {
    /// Экземпляр media, чей источник это ответил.
    media_instance_id: MediaInstanceId,

    /// Когда пришёл последний такой ответ.
    observed_at: Instant,

    /// Счётчик принесённого в последнем ответе.
    progress: media_core::LiveProducerProgress,

    /// Когда счётчик последний раз изменился (или впервые увиден у этого media).
    progressed_at: Instant,
}

impl LiveProducerWaitMark {
    /// Новый ответ «живой поставщик» с `progress` в `observed_at` после отметки
    /// `previous`: момент роста переносится, только если счётчик тот же у того же media.
    fn after(
        previous: Option<Self>,
        media_instance_id: MediaInstanceId,
        observed_at: Instant,
        progress: media_core::LiveProducerProgress,
    ) -> Self {
        let progressed_at = previous
            .filter(|mark| mark.media_instance_id == media_instance_id && mark.progress == progress)
            .map_or(observed_at, |mark| mark.progressed_at);
        Self {
            media_instance_id,
            observed_at,
            progress,
            progressed_at,
        }
    }
}

/// Что известно о честном ожидании живого поставщика (для срока перемотки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiveProducerWait {
    /// Последний ответ «данных нет, их несёт живой поставщик».
    pub(super) last_wait_at: Instant,
    /// Последний рост счётчика принесённого.
    pub(super) last_progress_at: Instant,
}

/// Session-owned runtime temporary demux readiness.
#[derive(Debug, Default)]
pub(super) struct DemuxRetryRuntime {
    /// Одновременно допустим только один deadline для exact installed generation.
    pending: Option<InstalledDemuxRetry>,

    /// Последний ответ «ждём живого поставщика» во время перемотки. Переживает
    /// terminal events (EOF, смена дорожек): он не про ожидание повтора, а про то,
    /// сколько перемотка ждала источник.
    live_producer_wait: Option<LiveProducerWaitMark>,
}

impl DemuxRetryRuntime {
    /// Забывает ожидание повтора (отметку живого поставщика не трогает).
    fn clear(&mut self) {
        self.pending = None;
    }
}

impl PlayerSession {
    /// Возвращает fence текущей установленной demux position.
    fn current_demux_retry_fence(&self) -> Option<InstalledDemuxRetryFence> {
        Some(InstalledDemuxRetryFence {
            media_instance_id: self.snapshot.media_instance_id?,
            seek_generation: self.pipeline.seek_generation(),
        })
    }

    /// Запоминает earliest retry, не меняя EOF, tracks, timeline или seek state.
    pub(super) fn schedule_installed_demux_retry(
        &mut self,
        observed_at: Instant,
        hint: DemuxRetryHint,
    ) {
        let Some(fence) = self.current_demux_retry_fence() else {
            self.demux_retry.clear();
            return;
        };
        let retry_deadline = observed_at
            .checked_add(hint.retry_after())
            .unwrap_or(observed_at);
        let resume_intent = self
            .demux_retry
            .pending
            .filter(|pending| pending.fence == fence)
            .map_or_else(
                || PlaybackResumeIntent::from_playback_state(self.playback_state()),
                |pending| pending.resume_intent,
            );
        let entered_buffering = self
            .demux_retry
            .pending
            .is_some_and(|pending| pending.fence == fence && pending.entered_buffering);

        self.demux_retry.pending = Some(InstalledDemuxRetry {
            fence,
            retry_deadline,
            resume_intent,
            entered_buffering,
        });
        self.note_live_producer_wait_if_seeking(fence.media_instance_id, observed_at);
    }

    /// Пока перемотка ждёт, спрашивает источник, чего он ждёт: ответ «живой
    /// поставщик» запоминается (по нему срок перемотки не идёт). Вне перемотки
    /// источник не спрашиваем — ответ нужен только сроку перемотки.
    fn note_live_producer_wait_if_seeking(
        &mut self,
        media_instance_id: MediaInstanceId,
        observed_at: Instant,
    ) {
        if !self.has_active_seek_commit() {
            return;
        }
        if let Some(media_core::DemuxDataWait::LiveProducer { progress }) =
            self.pipeline.demux_data_wait()
        {
            self.demux_retry.live_producer_wait = Some(LiveProducerWaitMark::after(
                self.demux_retry.live_producer_wait,
                media_instance_id,
                observed_at,
                progress,
            ));
        }
    }

    /// Честное ожидание живого поставщика у текущего media: последний ответ и
    /// последний рост принесённого (`None` — источник так не отвечал или media другое).
    #[must_use]
    pub(super) fn live_producer_wait(&self) -> Option<LiveProducerWait> {
        let current_instance = self.snapshot.media_instance_id?;
        self.demux_retry
            .live_producer_wait
            .filter(|mark| mark.media_instance_id == current_instance)
            .map(|mark| LiveProducerWait {
                last_wait_at: mark.observed_at,
                last_progress_at: mark.progressed_at,
            })
    }

    /// Проверяет, запрещён ли новый demux read до exact-generation deadline-а.
    #[must_use]
    pub(super) fn installed_demux_read_is_blocked(&self, now: Instant) -> bool {
        let Some(current_fence) = self.current_demux_retry_fence() else {
            return false;
        };
        self.demux_retry
            .pending
            .is_some_and(|pending| pending.fence == current_fence && now < pending.retry_deadline)
    }

    /// Возвращает оставшуюся задержку только для актуальной installed generation.
    #[must_use]
    pub(super) fn installed_demux_retry_delay(&self, now: Instant) -> Option<Duration> {
        let current_fence = self.current_demux_retry_fence()?;
        let pending = self.demux_retry.pending?;
        (pending.fence == current_fence)
            .then(|| pending.retry_deadline.saturating_duration_since(now))
    }

    /// Не даёт preroll разморозить output, пока source всё ещё ждёт matching retry.
    #[must_use]
    pub(super) fn installed_demux_retry_blocks_buffering_resume(&self) -> bool {
        let Some(current_fence) = self.current_demux_retry_fence() else {
            return false;
        };
        self.demux_retry
            .pending
            .is_some_and(|pending| pending.fence == current_fence)
    }

    /// Запрещает ли retry fence завершить текущий preroll (`Buffering` → `Playing`).
    ///
    /// Fence доказывает восстановление источника после demux-owned freeze: накопленный
    /// звук сам по себе не значит, что источник ожил. У первого старта freeze-а не было
    /// (он бывает только из `Playing`): «данных пока нет» на старте значит лишь, что всё
    /// доступное уже прочитано. Если при этом звук и первый кадр готовы, старт обязан
    /// состояться: живой источник (страница браузера) шлёт новые байты только когда часы
    /// плеера пошли, и ожидание байтов до старта стало бы вечным `Buffering`
    /// (сессия 03e моста). Дальше нехватку данных в `Playing` ловит обычный runway-check
    /// `enter_buffering_for_demux_underrun_if_needed`.
    #[must_use]
    pub(super) fn installed_demux_retry_blocks_preroll_finish(&self) -> bool {
        !self.startup_autoplay_preroll_pending()
            && self.installed_demux_retry_blocks_buffering_resume()
    }

    /// Нужен ли пробный read источника в обход downstream backpressure.
    ///
    /// В `Buffering` часы стоят: очередь показа, слоты выгрузки кадров и texture-ресурсы
    /// не освобождаются, пока playback не продолжится. Если resume при этом ждёт только
    /// снятия retry fence, а снять его может лишь принятый event источника, обычная
    /// admission не разрешает read никогда — взаимная блокировка. Её нашла сессия 03a
    /// плана моста (решение 93): поток только с видео и паузой в данных на старте
    /// навсегда оставался в `Buffering`. Один пробный read на срок повтора её разрывает:
    /// принятый packet снимает fence (дальше — обычный preroll gate), а повторное
    /// `TemporarilyUnavailable` ставит новый срок.
    #[must_use]
    pub(super) fn buffering_resume_awaits_source_probe(&self, now: Instant) -> bool {
        // Перемотка ведёт admission сама (свои правила preroll-а) — её не трогаем.
        self.snapshot.playback_state == PlaybackState::Buffering
            && !self.has_active_seek_commit()
            && self.installed_demux_retry_blocks_buffering_resume()
            && !self.installed_demux_read_is_blocked(now)
    }

    /// Отмечает успешное продолжение source-а, не объявляя playback готовым.
    ///
    /// Первый принятый packet доказывает только восстановление demux source-а.
    /// Возврат из `Buffering` остаётся за общим autoplay preroll gate, который
    /// независимо проверяет audio runway и video readiness.
    pub(super) fn complete_installed_demux_retry_after_event(&mut self) {
        let Some(_current_fence) = self.current_demux_retry_fence() else {
            self.demux_retry.clear();
            return;
        };
        // Любой принятый event принадлежит уже текущему pipeline turn-у. Поэтому
        // он снимает как matching retry, так и забытый stale fence без state resume.
        let _completed_retry = self.demux_retry.pending.take();
    }

    /// Завершает terminal/non-packet ожидание без восстановления playback intent.
    pub(super) fn clear_installed_demux_retry_after_terminal_event(&mut self) {
        self.demux_retry.clear();
    }

    /// Замораживает playing pipeline, когда temporary demux readiness исчерпала runway.
    ///
    /// Для A/V audio является presentation master: пустой audio runway требует
    /// `Buffering`, даже если video presentation queue всё ещё содержит кадры.
    /// Для media без выбранного audio сохраняется прежний полный video-drain gate.
    pub(super) fn enter_buffering_for_demux_underrun_if_needed(
        &mut self,
        observed_at: Instant,
        audio_starvation_margin: DemuxAudioStarvationMargin,
    ) -> PlayerResult<bool> {
        let Some(current_fence) = self.current_demux_retry_fence() else {
            self.demux_retry.clear();
            return Ok(false);
        };
        let Some(mut pending) = self.demux_retry.pending else {
            return Ok(false);
        };
        if pending.fence != current_fence || pending.entered_buffering {
            return Ok(false);
        }
        if pending.resume_intent != PlaybackResumeIntent::Play
            || self.playback_state() != PlaybackState::Playing
            || self
                .demux_buffering_cause(observed_at, pending, audio_starvation_margin)
                .is_none()
        {
            return Ok(false);
        }

        // Pause/freeze должен завершиться до state transition: при backend error
        // session не публикует ложный `Buffering` с продолжающимся audio clock.
        self.freeze_playback_for_demux_buffering()?;
        pending.entered_buffering = true;
        self.demux_retry.pending = Some(pending);
        self.set_playback_state(PlaybackState::Buffering);
        Ok(true)
    }

    /// Классифицирует exhaustion без раскрытия storage наружу из session boundary.
    fn demux_buffering_cause(
        &self,
        observed_at: Instant,
        pending_retry: InstalledDemuxRetry,
        audio_starvation_margin: DemuxAudioStarvationMargin,
    ) -> Option<DemuxBufferingCause> {
        if self.pipeline.has_selected_audio_track() {
            return self
                .selected_audio_runway_will_expire_before_retry(
                    observed_at,
                    pending_retry,
                    audio_starvation_margin,
                )
                .then_some(DemuxBufferingCause::SelectedAudioRunwayWillExpireBeforeRetry);
        }

        self.video_downstream_is_drained()
            .then_some(DemuxBufferingCause::DownstreamFullyDrained)
    }

    /// Проверяет риск исчерпания decoded/output runway до разрешённого source retry.
    fn selected_audio_runway_will_expire_before_retry(
        &self,
        observed_at: Instant,
        pending_retry: InstalledDemuxRetry,
        audio_starvation_margin: DemuxAudioStarvationMargin,
    ) -> bool {
        if !self.pipeline.pending_audio_packet_is_empty() {
            return false;
        }

        let usable_runway = self
            .audio_buffer_level_ms()
            .map_or(Duration::ZERO, duration_from_non_negative_millis);
        SelectedAudioRunwayBudget {
            usable_runway,
            remaining_retry_wait: pending_retry
                .retry_deadline
                .saturating_duration_since(observed_at),
            scheduler_callback_margin: audio_starvation_margin,
        }
        .will_expire_before_retry()
    }

    /// Проверяет полный drain video path для media без audio presentation master-а.
    fn video_downstream_is_drained(&self) -> bool {
        !self.pipeline.has_selected_video_track()
            || (self.pipeline.pending_video_packet_len() == 0
                && self
                    .pipeline
                    .video_decoder_packet_queue_depth()
                    .unwrap_or(0)
                    == 0
                && self.pipeline.video_decode_in_flight_packets() == 0
                && self.pipeline.video_present_queue_is_empty())
    }

    /// Явно инвалидирует deadline при install/stop/shutdown boundary.
    pub(super) fn clear_installed_demux_retry(&mut self) {
        self.demux_retry.clear();
        self.demux_retry.live_producer_wait = None;
    }
}

/// Без паники нормализует backend-reported milliseconds в saturating duration.
fn duration_from_non_negative_millis(milliseconds: f64) -> Duration {
    let non_negative_seconds = (milliseconds / 1_000.0).max(0.0);
    Duration::try_from_secs_f64(non_negative_seconds).unwrap_or(Duration::MAX)
}
