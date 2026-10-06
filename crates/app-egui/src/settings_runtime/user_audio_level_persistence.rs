//! Сохранение громкости и mute пользователя в `config.toml` (UX-11).
//!
//! Решения владельца: громкость живёт в основном config (`audio.volume` = последняя
//! слышимая громкость, `audio.muted` = mute), пишется не на каждое движение слайдера,
//! а через паузу после изменения и при выходе; при старте побеждает сохранённое.
//!
//! Модуль владеет только «что и когда записать». Сама запись идёт через обычную
//! runtime settings transaction (как ширина sidebar), поэтому сохраняются все
//! защиты config-а: atomic replace, validation, режим «только в памяти» (сессия 05)
//! и синхронизация с открытым окном настроек.
//!
//! Источник правды о громкости во время работы — snapshot плеера: туда сходятся
//! слайдер, клавиша M, MPRIS и Apply из окна настроек. Модуль только наблюдает его.

use super::*;
use crate::user_audio_level::{PlayerAudioObservation, UserAudioLevel, VolumePermille};

/// Пауза после последнего изменения громкости перед записью config-а.
///
/// Три секунды покрывают «подвигал слайдер и отпустил» одной записью, но достаточно
/// коротки, чтобы громкость пережила падение или kill процесса.
pub(super) const USER_AUDIO_LEVEL_PERSIST_DEBOUNCE: Duration = Duration::from_secs(3);

/// Сколько ждать, пока новый player binding подтвердит восстановленный уровень.
///
/// Обычно подтверждение приходит за миллисекунды; лимит нужен только чтобы
/// сохранение не выключилось навсегда, если подтверждающий snapshot пропущен.
pub(super) const PLAYER_AUDIO_LEVEL_SYNC_TIMEOUT: Duration = Duration::from_secs(2);

/// Stable setting id громкости.
const AUDIO_VOLUME_SETTING_ID: &str = "audio.volume";

/// Stable setting id mute.
const AUDIO_MUTED_SETTING_ID: &str = "audio.muted";

/// Можно ли сейчас верить snapshot-ам плеера о громкости.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayerAudioLevelSync {
    /// Player binding ещё не создан: snapshot-ы — заглушка (`volume = 1.0`).
    NoPlayerBinding,
    /// Плееру отправлен уровень, ждём snapshot, который его подтвердит.
    ///
    /// До подтверждения snapshot-ы показывают старое состояние (пустой snapshot
    /// нового worker-а или уровень до Apply) — сохранять их нельзя, иначе в config
    /// попадёт мусор вместо громкости пользователя.
    AwaitingConfirmation {
        expected: UserAudioLevel,
        give_up_at: Instant,
    },
    /// Snapshot-ы отражают действия пользователя.
    Following,
}

/// Состояние сохранения громкости, принадлежащее `SettingsRuntime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UserAudioLevelPersistence {
    /// Последний известный уровень пользователя (из плеера или из config-а).
    current: UserAudioLevel,

    /// Доверие к snapshot-ам плеера.
    sync: PlayerAudioLevelSync,

    /// Когда записать `current`, если он отличается от committed config.
    persist_at: Option<Instant>,
}

impl UserAudioLevelPersistence {
    /// Стартовое состояние: уровень из config, плеера ещё нет.
    pub(super) fn from_committed(committed: &AppConfig) -> Self {
        Self {
            current: UserAudioLevel::from_audio_config(&committed.audio),
            sync: PlayerAudioLevelSync::NoPlayerBinding,
            persist_at: None,
        }
    }
}

/// Результат попытки записи без смешения «нечего делать» и ошибки transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserAudioLevelFlushOutcome {
    /// Записывать нечего или пауза ещё не прошла.
    NoPending,
    /// Config синхронизирован с уровнем пользователя (или запись была no-op).
    Succeeded,
    /// Transaction вернула typed failure; громкость в плеере не тронута.
    Failed,
}

impl UserAudioLevelFlushOutcome {
    /// Завершённая попытка меняет status/config и требует перерисовки.
    #[must_use]
    pub(crate) fn needs_redraw(self) -> bool {
        self != Self::NoPending
    }
}

/// Доставлен ли уровень громкости в текущее воспроизведение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackAudioLevelDelivery {
    /// Команды приняты очередью player worker-а.
    Sent,
    /// Активного player binding нет или очередь отказала (причина уже в логе).
    NotDelivered,
}

impl SettingsRuntime {
    /// Уровень, который нужно восстановить в новом player binding.
    ///
    /// Вызывается при каждом создании player binding (запуск, resume после suspend).
    /// После вызова snapshot-ы игнорируются, пока плеер не подтвердит этот уровень,
    /// но не дольше `PLAYER_AUDIO_LEVEL_SYNC_TIMEOUT`.
    pub(crate) fn begin_player_audio_level_restore(&mut self, now: Instant) -> UserAudioLevel {
        let level = self.user_audio_level.current;
        self.user_audio_level.sync = PlayerAudioLevelSync::AwaitingConfirmation {
            expected: level,
            give_up_at: now + PLAYER_AUDIO_LEVEL_SYNC_TIMEOUT,
        };
        level
    }

    /// Учитывает громкость из очередного snapshot-а плеера.
    ///
    /// Возвращает `true`, если уровень пользователя изменился. Если уровень вернулся к
    /// уже сохранённому, отложенная запись отменяется: писать то же самое незачем.
    pub(crate) fn record_player_audio_observation(
        &mut self,
        observation: PlayerAudioObservation,
        now: Instant,
    ) -> bool {
        if !self.accepts_player_audio_observation(observation, now) {
            return false;
        }

        let persistence = &mut self.user_audio_level;
        let observed_level = persistence.current.after_player_observation(observation);
        if observed_level == persistence.current {
            return false;
        }

        persistence.current = observed_level;
        let committed_level = UserAudioLevel::from_audio_config(&self.controller.committed().audio);
        // Каждое новое изменение переносит запись: пауза отсчитывается от последнего.
        persistence.persist_at =
            (observed_level != committed_level).then(|| now + USER_AUDIO_LEVEL_PERSIST_DEBOUNCE);
        true
    }

    /// Решает, можно ли верить snapshot-у, и снимает ожидание подтверждения.
    fn accepts_player_audio_observation(
        &mut self,
        observation: PlayerAudioObservation,
        now: Instant,
    ) -> bool {
        match self.user_audio_level.sync {
            PlayerAudioLevelSync::NoPlayerBinding => false,
            PlayerAudioLevelSync::Following => true,
            PlayerAudioLevelSync::AwaitingConfirmation {
                expected,
                give_up_at,
            } => {
                if expected.is_confirmed_by(observation) {
                    self.user_audio_level.sync = PlayerAudioLevelSync::Following;
                    return true;
                }
                if now < give_up_at {
                    return false;
                }
                tracing::warn!(
                    expected_muted = expected.is_muted(),
                    "Плеер не подтвердил восстановленную громкость вовремя; сохранение громкости продолжает следить за плеером"
                );
                self.user_audio_level.sync = PlayerAudioLevelSync::Following;
                true
            }
        }
    }

    /// Ближайший момент записи громкости для idle event loop.
    #[must_use]
    pub(crate) fn next_user_audio_level_persist_deadline(&self) -> Option<Instant> {
        self.user_audio_level.persist_at
    }

    /// Записывает громкость, только когда пауза после изменения уже прошла.
    pub(crate) fn flush_due_user_audio_level<A>(
        &mut self,
        now: Instant,
        runtime_adapter: &mut A,
    ) -> SettingsResult<UserAudioLevelFlushOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match self.user_audio_level.persist_at {
            Some(persist_at) if now >= persist_at => {
                self.flush_pending_user_audio_level(runtime_adapter)
            }
            _ => Ok(UserAudioLevelFlushOutcome::NoPending),
        }
    }

    /// Принудительно записывает отложенную громкость (выход, suspend).
    pub(crate) fn flush_pending_user_audio_level<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<UserAudioLevelFlushOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        if self.user_audio_level.persist_at.take().is_none() {
            return Ok(UserAudioLevelFlushOutcome::NoPending);
        }

        let target = self.user_audio_level.current;
        for request in self.user_audio_level_commit_requests(target) {
            let report =
                self.commit_runtime_setting_with_runtime_adapter(request, runtime_adapter)?;
            if !matches!(
                report.final_state,
                ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
            ) {
                // Controller уже откатил transaction. Громкость в плеере не трогаем:
                // пользователь слышит то, что выбрал, просто config остался прежним.
                self.invalidate_ui_model();
                self.status = status_from_apply_report(&report);
                tracing::error!(
                    final_state = ?report.final_state,
                    attempted_volume = target.audible_volume().as_config_ratio(),
                    attempted_muted = target.is_muted(),
                    "Не удалось сохранить громкость в config"
                );
                return Ok(UserAudioLevelFlushOutcome::Failed);
            }
        }
        Ok(UserAudioLevelFlushOutcome::Succeeded)
    }

    /// Одно-полевые runtime commit-ы только для реально отличающихся полей.
    ///
    /// Runtime transaction умеет ровно одно поле за раз. Поэтому одновременная смена
    /// громкости и mute даёт две записи — это редкий случай (слайдер при mute), а
    /// обычное движение слайдера или нажатие M — одна запись.
    fn user_audio_level_commit_requests(
        &self,
        target: UserAudioLevel,
    ) -> Vec<RuntimeSettingCommitRequest> {
        let committed_audio = &self.controller.committed().audio;
        let mut requests = Vec::with_capacity(2);
        let target_volume = target.audible_volume();
        // Сравнение в тысячных: 0.30000001 в файле и 0.3 от плеера — одна громкость.
        if VolumePermille::from_ratio(committed_audio.volume) != target_volume {
            requests.push(RuntimeSettingCommitRequest::new(
                AUDIO_VOLUME_SETTING_ID,
                SettingValue::Float(target_volume.as_config_ratio()),
            ));
        }
        if committed_audio.muted != target.is_muted() {
            requests.push(RuntimeSettingCommitRequest::new(
                AUDIO_MUTED_SETTING_ID,
                SettingValue::Bool(target.is_muted()),
            ));
        }
        requests
    }

    /// После Apply/OK из окна настроек переносит изменённую громкость в плеер.
    ///
    /// Решение владельца: в окне настроек показывается настоящая громкость, поэтому
    /// Apply меняет и текущее воспроизведение. Иначе при выходе слайдер плеера
    /// перезаписал бы значение, которое пользователь только что ввёл в настройках.
    /// Автосохранение (runtime commit) сюда не попадает и плеер не трогает.
    pub(super) fn deliver_user_applied_audio_level<A>(
        &mut self,
        level_before_apply: UserAudioLevel,
        runtime_adapter: &mut A,
        now: Instant,
    ) where
        A: SettingsRuntimeReconfigureHost,
    {
        let applied_level = UserAudioLevel::from_audio_config(&self.controller.committed().audio);
        if applied_level == level_before_apply {
            return;
        }

        // Явное действие пользователя в настройках новее любой отложенной записи.
        self.user_audio_level.current = applied_level;
        self.user_audio_level.persist_at = None;
        match runtime_adapter.apply_user_audio_level_to_playback(applied_level) {
            PlaybackAudioLevelDelivery::Sent => {
                self.user_audio_level.sync = PlayerAudioLevelSync::AwaitingConfirmation {
                    expected: applied_level,
                    give_up_at: now + PLAYER_AUDIO_LEVEL_SYNC_TIMEOUT,
                };
            }
            PlaybackAudioLevelDelivery::NotDelivered => {
                // Config уже записан; плеер применит его при следующем binding.
                tracing::warn!(
                    "Громкость из настроек сохранена, но не доставлена в текущее воспроизведение"
                );
            }
        }
    }

    /// Уровень из committed config до Apply, чтобы потом понять, менял ли его пользователь.
    pub(super) fn committed_user_audio_level(&self) -> UserAudioLevel {
        UserAudioLevel::from_audio_config(&self.controller.committed().audio)
    }
}
