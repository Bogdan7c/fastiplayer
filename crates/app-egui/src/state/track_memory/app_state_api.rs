//! Склейка памяти выбора с `AppState`: тонкие методы-намерения для остальных модулей.
//!
//! Правило: память пишет ТОЛЬКО то, что сделал пользователь. Сюда приходят:
//! - выбор аудио — из воронки `select_audio_track_by_user`;
//! - действия с субтитрами — через `capture_user_subtitle_choices` сразу после каждого
//!   пользовательского входа (меню/хоткеи, подключение файла, бросок файла).
//!   Автовыбор и восстановление из памяти сюда не попадают по построению.

use std::time::Instant;

use media_core::TrackId;
use player_core::{MediaInstanceId, PlayerSnapshot};
use playlist_state::TrackMemoryCapacity;

use super::RememberedAudioPlan;
use super::bridge::{record_subtitle_changes, record_user_audio_choice, subtitle_restore_request};
use super::hints::plan_audio_restore;
use super::identity::video_key_for_source;
use super::owner::RecordOutcome;
use crate::state::subtitles::SubtitleRestoreRequest;
use crate::state::{ActiveMediaSource, AppState};

impl AppState {
    /// Media установлено: память переключается на его видео, освежает запись и возвращает
    /// то, что надо восстановить в субтитрах (звук восстанавливается позже, по снимку плеера).
    pub(in crate::state) fn begin_track_memory_for_media(
        &mut self,
        media_instance_id: MediaInstanceId,
        source: &ActiveMediaSource,
    ) -> SubtitleRestoreRequest {
        let key = match video_key_for_source(source) {
            Ok(key) => Some(key),
            Err(error) => {
                tracing::debug!(?error, "Track memory is unavailable for this media source");
                None
            }
        };
        let mut owner = self.track_memory.lock();
        owner.begin_video(media_instance_id, key, Instant::now());
        subtitle_restore_request(&owner.remembered_for(media_instance_id))
    }

    /// Media исчезло из плеера: память ни к чему не привязана.
    pub(in crate::state) fn end_track_memory_for_media(&mut self) {
        self.track_memory.lock().end_video();
    }

    /// Аудиодорожка выбрана пользователем (после успешной отправки команды плееру).
    pub(in crate::state) fn remember_user_audio_choice(
        &mut self,
        player_snapshot: &PlayerSnapshot,
        track_id: TrackId,
    ) {
        let Some(media_instance_id) = player_snapshot.media_instance_id else {
            return;
        };
        let outcome = record_user_audio_choice(
            &mut self.track_memory.lock(),
            media_instance_id,
            &player_snapshot.tracks,
            track_id,
            Instant::now(),
        );
        if outcome == RecordOutcome::NotApplicable {
            tracing::debug!("The audio choice was not remembered");
        }
    }

    /// Язык звука web-видео, выбранный пользователем, ТОЛЬКО что установился: запоминаем.
    ///
    /// Вызывается завершением same-item switch-а (не стартом): отказ и устаревший выбор
    /// сюда не доходят. Ключ — исходная ссылка активного источника, то есть то же видео.
    pub(in crate::state) fn remember_user_web_audio_language(
        &mut self,
        language: playlist_state::RememberedWebAudioLanguage,
    ) {
        let Some(source) = self.active_media_source.as_ref() else {
            return;
        };
        let Ok(key) = video_key_for_source(source) else {
            tracing::debug!("The audio language cannot be remembered for this media source");
            return;
        };
        let outcome =
            self.track_memory
                .lock()
                .remember_web_audio_language(key, language, Instant::now());
        if outcome == RecordOutcome::NotApplicable {
            tracing::debug!("The audio language was not remembered");
        }
    }

    /// Запомненный язык звука для источника (восстановление при открытии).
    pub(in crate::state) fn remembered_web_audio_language(
        &self,
        source: &ActiveMediaSource,
    ) -> Option<playlist_state::RememberedWebAudioLanguage> {
        let key = video_key_for_source(source).ok()?;
        self.track_memory
            .lock()
            .remembered_for_key(&key)
            .web_audio_language
    }

    /// Забирает действия пользователя с субтитрами и записывает их в память.
    ///
    /// Вызывается сразу после каждого пользовательского входа в субтитры, поэтому изменение
    /// всегда попадает в запись того media, в котором оно сделано.
    pub(in crate::state) fn capture_user_subtitle_choices(&mut self) {
        let Some(changes) = self.subtitles.take_memory_changes() else {
            return;
        };
        record_subtitle_changes(&mut self.track_memory.lock(), changes, Instant::now());
    }

    /// План восстановления звука для только что установленного media.
    ///
    /// Читает память по ключу ИСТОЧНИКА, а не по «текущему видео»: звук выбирается до того,
    /// как установка опубликована (`record_installed_media`). Снимок обязан быть снимком
    /// установленного media (вызывающий это проверил), иначе дорожки были бы чужими.
    pub(in crate::state) fn remembered_audio_plan(
        &self,
        source: &ActiveMediaSource,
        player_snapshot: &PlayerSnapshot,
    ) -> RememberedAudioPlan {
        if player_snapshot.media_instance_id.is_none() {
            return RememberedAudioPlan::NothingRemembered;
        }
        let Ok(key) = video_key_for_source(source) else {
            return RememberedAudioPlan::NothingRemembered;
        };
        let remembered = self.track_memory.lock().remembered_for_key(&key);
        plan_audio_restore(
            remembered.audio.as_ref(),
            &player_snapshot.tracks,
            player_snapshot.selected_tracks.audio_track,
        )
    }

    /// Применяет ёмкость памяти из committed config (после успешной транзакции настроек).
    pub(in crate::state) fn sync_track_memory_capacity(&mut self) {
        let entries = self
            .committed_config_snapshot
            .as_config()
            .playlist
            .track_memory_max_entries;
        let capacity = usize::try_from(entries)
            .ok()
            .and_then(|entries| TrackMemoryCapacity::new(entries).ok());
        let Some(capacity) = capacity else {
            tracing::warn!(entries, "Track memory capacity from config is out of range");
            return;
        };
        let evicted = self
            .track_memory
            .lock()
            .apply_capacity(capacity, Instant::now());
        if evicted > 0 {
            tracing::debug!(evicted, "Track memory shrank to the configured capacity");
        }
    }

    /// Когда event loop должен проснуться ради записи памяти.
    pub(crate) fn track_memory_wake_deadline(&self) -> Option<Instant> {
        self.track_memory.lock().next_wake_deadline()
    }

    /// Периодический шаг памяти (неблокирующий): отправка записи и отчёты о сбоях.
    pub(crate) fn tick_track_memory(&mut self) {
        self.track_memory.lock().poll(Instant::now());
    }
}
