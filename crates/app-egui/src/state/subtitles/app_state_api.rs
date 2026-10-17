//! Тонкие обёртки `AppState` над владельцем субтитров: единственная точка входа для UI,
//! хоткеев и внешнего открытия (они не знают ни о владельце, ни о фоновых задачах).
//!
//! Жизненный цикл media (запуск и сброс) подключён в `state/media_jobs.rs`, опрос задач — по
//! wake-событию, а overlay кадра — в `frame_prepare/submit.rs`.

use std::path::{Path, PathBuf};

use player_core::MediaInstanceId;

use super::intents::{AttachOutcome, QueueOutcome, SubtitleVisibility};
use super::menu_view::SubtitleMenuView;
use super::render_worker::clock_tracker::PlaybackObservation;
use super::{
    CurrentMediaForSubtitles, EmbeddedUpdate, PlacedSubtitleOverlay, SubmittedFrameForSubtitles,
    subtitle_source_of,
};
use crate::state::{ActiveMediaSource, AppState};
use crate::subtitle_message::attach_batch_message;

impl AppState {
    /// Media установлено в плеер: запускает (или сбрасывает) загрузку субтитров.
    ///
    /// Единая точка для всех способов открыть файл (см. документацию модуля). Без
    /// `MediaInstanceId` (receipt не `Installed`) субтитры очищаются.
    pub(in crate::state) fn start_subtitles_for_installed_media(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        source: &ActiveMediaSource,
    ) {
        // Действия пользователя в прежнем media уходят в память ДО смены ключа.
        self.capture_user_subtitle_choices();
        match media_instance_id {
            Some(media_instance_id) => {
                let restore = self.begin_track_memory_for_media(media_instance_id, source);
                self.subtitles.begin_media_restoring(
                    media_instance_id,
                    subtitle_source_of(source),
                    restore,
                );
                // Файл, брошенный вместе с видео, - выбор пользователя «для этого видео».
                self.capture_user_subtitle_choices();
            }
            None => {
                self.end_track_memory_for_media();
                self.subtitles.clear_media();
            }
        }
    }

    /// Media исчезло из плеера: субтитры сброшены.
    pub(in crate::state) fn clear_subtitles_for_released_media(&mut self) {
        self.capture_user_subtitle_choices();
        self.end_track_memory_for_media();
        self.subtitles.clear_media();
    }

    /// Применяет секцию `[subtitles]` committed конфига (вызывается при синхронизации
    /// committed snapshot, то есть только после успешной транзакции настроек).
    pub(in crate::state) fn apply_subtitles_config(
        &mut self,
        config: &fastiplayer_config::SubtitlesConfig,
    ) {
        self.subtitles.apply_config(config);
    }

    /// Передаёт владельцу субтитров приёмник трубы встроенных дорожек (один раз при сборке).
    pub(in crate::state) fn attach_subtitle_feed(
        &mut self,
        receiver: player_core::SubtitleFeedReceiver,
    ) {
        self.subtitles.attach_feed_receiver(receiver);
    }

    /// Синхронизирует встроенные дорожки с последним снимком плеера и читает трубу пакетов.
    ///
    /// Идемпотентно и дёшево: неизменный список дорожек не перестраивается, чтение трубы
    /// ограничено по числу сообщений. Зовётся при каждом обновлении снимка плеера.
    pub(in crate::state) fn pump_embedded_subtitles(&mut self) {
        let snapshot = &self.last_player_snapshot;
        let tracks_update = self
            .subtitles
            .sync_embedded_tracks(snapshot.media_instance_id, &snapshot.tracks);
        let feed_update = self.subtitles.drain_embedded_feed();
        if tracks_update.merge(feed_update) == EmbeddedUpdate::Changed {
            // Меню CC или реплика на экране изменились: нужен ещё один кадр.
            self.mark_pending_worker_redraw();
        }
    }

    /// Опрос фоновых задач субтитров (по wake-событию). `true` — нужна перерисовка.
    pub(crate) fn poll_subtitle_jobs(&mut self) -> bool {
        // Порядок важен: ответ диалога «Load file…» приходит по тому же wake-событию.
        let picker_changed = self.poll_subtitle_file_picker();
        self.subtitles.poll_jobs() || picker_changed
    }

    /// Передаёт субтитрам наблюдение за плеером из последнего snapshot: режим (игра, пауза,
    /// seek, scrub), цель seek/scrub, скорость и оценку длительности кадра. Дёшево: вызывается
    /// на каждом кадре UI; часы потоку-рисовальщику уходят, только если они изменились.
    pub(crate) fn observe_playback_for_subtitles(&mut self) {
        let snapshot = &self.last_player_snapshot;
        let current = CurrentMediaForSubtitles {
            media_instance_id: snapshot.media_instance_id,
            render_generation: snapshot.render_generation,
        };
        let observation = PlaybackObservation::from_snapshot(snapshot);
        self.subtitles.observe_playback(current, observation);
    }

    /// Overlay субтитров для кадра, который прямо сейчас уходит в renderer.
    ///
    /// Читает `media_instance_id` и `render_generation` из последнего snapshot плеера,
    /// не раскрывая их внешнему коду. Вместе с overlay отдаёт сдвиг слоя (позиция из стиля,
    /// подъём над панелью управления): рендер применяет его без перерастеризации.
    pub(crate) fn subtitle_overlay_for_submitted_frame(
        &mut self,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<PlacedSubtitleOverlay<'_>> {
        // Новые пакеты, пришедшие после последнего снимка, видны уже на этом кадре.
        self.subtitles.drain_embedded_feed();
        let current = CurrentMediaForSubtitles {
            media_instance_id: self.last_player_snapshot.media_instance_id,
            render_generation: self.last_player_snapshot.render_generation,
        };
        self.subtitles.placed_overlay_for_frame(current, frame)
    }

    // ---- Intent-API для UI (кнопка CC, меню, хоткеи, внешнее открытие) ----

    /// Добавляет файл субтитров к текущему media: дорожка выбирается, субтитры включаются.
    pub(crate) fn attach_subtitle_file(&mut self, path: &Path) -> AttachOutcome {
        let outcome = self.subtitles.attach_file(path);
        self.capture_user_subtitle_choices();
        outcome
    }

    /// Забывает файлы, ждавшие следующего media (открытие видео не состоялось).
    pub(crate) fn clear_queued_subtitle_files(&mut self) {
        self.subtitles.clear_queued_files();
    }

    /// Бросок одних лишь файлов субтитров: добавляет их к текущему media (последний
    /// становится выбранным) и показывает подсказку, если что-то не вышло.
    pub(crate) fn attach_dropped_subtitle_files(&mut self, paths: &[PathBuf]) {
        let outcomes = self.subtitles.attach_files(paths);
        self.capture_user_subtitle_choices();
        if let Some(message) = attach_batch_message(&outcomes) {
            self.notify_transient(message);
        }
    }

    /// Бросок «видео + субтитры»: файлы ждут видео, которое открывает этот же бросок.
    ///
    /// Забывает их `settle_dropped_subtitles`, если открытие закончилось без установки.
    pub(crate) fn queue_dropped_subtitle_files(&mut self, paths: Vec<PathBuf>) {
        for outcome in self.subtitles.queue_files_for_next_media(paths) {
            if outcome == QueueOutcome::NotASubtitleFile {
                // Классификатор уже отфильтровал расширения; сюда попадает только гонка/баг.
                tracing::debug!("A non-subtitle file reached the subtitle queue and was skipped");
            }
        }
    }

    /// Read-only снимок для кнопки CC и меню.
    pub(crate) fn subtitle_menu_view(&self) -> SubtitleMenuView {
        self.subtitles.menu_view()
    }

    /// Изменение «включено», сделанное пользователем и ещё не сохранённое в конфиг
    /// (автовыбор его не создаёт). Возвращается один раз.
    pub(crate) fn take_subtitle_enabled_preference_change(&mut self) -> Option<SubtitleVisibility> {
        self.subtitles.take_enabled_preference_change()
    }
}
