//! Уровень громкости пользователя: «последняя слышимая громкость» + mute (UX-11).
//!
//! Это чистый value type без I/O. Он нужен трём слоям сразу:
//! - `SettingsRuntime` сравнивает его с committed config и решает, нужно ли сохранять;
//! - `AppShell` восстанавливает его в новом player binding (запуск, resume после suspend);
//! - окно настроек после Apply переносит его в текущее воспроизведение.
//!
//! Почему не одно число: при mute плеер показывает `volume = 0.0`, а громкость
//! «до mute» держит у себя внутри. Если сохранять только число, после перезапуска
//! громкость стала бы нулём. Поэтому громкость и mute хранятся отдельно.

use fastiplayer_config::AudioConfig;
use player_core::{PlayerCommand, PlayerCommandSender, PlayerSnapshot, PlayerWorkerSendError};

/// Сколько «тысячных» в полной громкости `1.0`.
const VOLUME_PERMILLE_SCALE: u16 = 1000;

/// Громкость в тысячных долях (`0..=1000`).
///
/// Целое число вместо `f32`/`f64` нужно по двум причинам:
/// - сравнение «изменилась ли громкость» становится точным, без допусков для float;
/// - в `config.toml` пишется аккуратное `0.3`, а не `0.30000001192092896` из `f32`.
///
/// Шаг 0.1 % заметно мельче шага слайдера настроек (1 %), поэтому округление не слышно.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VolumePermille(u16);

impl VolumePermille {
    /// Полная тишина.
    pub(crate) const SILENT: Self = Self(0);

    /// Переводит долю `0.0..=1.0` в тысячные с округлением до ближайшей.
    ///
    /// Входы уже проверены своими владельцами (validation config-а, `SetVolume` у
    /// player-core), поэтому значения вне диапазона только прижимаются к границам,
    /// а не считаются ошибкой; NaN (невозможен после validation) даёт тишину.
    #[must_use]
    pub(crate) fn from_ratio(ratio: f64) -> Self {
        if ratio.is_nan() {
            return Self::SILENT;
        }
        let clamped_ratio = ratio.clamp(0.0, 1.0);
        let scaled = (clamped_ratio * f64::from(VOLUME_PERMILLE_SCALE)).round();
        // После clamp значение гарантированно лежит в 0..=1000 и помещается в u16.
        Self(scaled as u16)
    }

    /// Значение для `audio.volume` в config (`f64`).
    #[must_use]
    pub(crate) fn as_config_ratio(self) -> f64 {
        f64::from(self.0) / f64::from(VOLUME_PERMILLE_SCALE)
    }

    /// Значение для `PlayerCommand::SetVolume` (`f32`).
    #[must_use]
    pub(crate) fn as_player_ratio(self) -> f32 {
        f32::from(self.0) / f32::from(VOLUME_PERMILLE_SCALE)
    }

    /// Нулевая громкость неотличима от mute для плеера (он сам ставит `muted = true`).
    #[must_use]
    pub(crate) const fn is_silent(self) -> bool {
        self.0 == 0
    }
}

/// Выключен ли звук пользователем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MuteState {
    /// Звук слышен с громкостью `audible_volume`.
    Audible,
    /// Mute: плеер играет тишину, а `audible_volume` вернётся при включении звука.
    Muted,
}

/// Громкость и mute, как их видит пользователь.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UserAudioLevel {
    /// Последняя слышимая громкость (при mute — громкость «до mute»).
    audible_volume: VolumePermille,

    /// Выключен ли звук.
    mute: MuteState,
}

/// Громкость и mute из одного player snapshot-а, без остальных полей snapshot-а.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlayerAudioObservation {
    /// `PlayerSnapshot::volume`: при mute здесь `0.0`.
    volume: f32,

    /// `PlayerSnapshot::muted`.
    muted: bool,
}

impl PlayerAudioObservation {
    /// Берёт из snapshot-а только поля громкости.
    #[must_use]
    pub(crate) fn from_player_snapshot(snapshot: &PlayerSnapshot) -> Self {
        Self {
            volume: snapshot.volume,
            muted: snapshot.muted,
        }
    }

    /// Явный конструктор для тестов, где нет полноценного snapshot-а.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn new(volume: f32, muted: bool) -> Self {
        Self { volume, muted }
    }
}

impl UserAudioLevel {
    /// Явный конструктор (тесты и сравнения).
    #[must_use]
    pub(crate) const fn new(audible_volume: VolumePermille, mute: MuteState) -> Self {
        Self {
            audible_volume,
            mute,
        }
    }

    /// Уровень, сохранённый в config.
    ///
    /// Нулевая громкость нормализуется в mute: плеер всё равно покажет `muted = true`
    /// после `SetVolume(0.0)`, и без нормализации каждый запуск с `volume = 0`
    /// порождал бы лишнюю запись `muted = true` в config.
    #[must_use]
    pub(crate) fn from_audio_config(audio: &AudioConfig) -> Self {
        let audible_volume = VolumePermille::from_ratio(audio.volume);
        let mute = if audio.muted || audible_volume.is_silent() {
            MuteState::Muted
        } else {
            MuteState::Audible
        };
        Self::new(audible_volume, mute)
    }

    /// Последняя слышимая громкость.
    #[must_use]
    pub(crate) const fn audible_volume(self) -> VolumePermille {
        self.audible_volume
    }

    /// Выключен ли звук.
    #[must_use]
    pub(crate) const fn is_muted(self) -> bool {
        matches!(self.mute, MuteState::Muted)
    }

    /// Громкость, которую реально слышно (при mute — `0.0`), для MPRIS и плеера.
    #[must_use]
    pub(crate) fn effective_player_volume(self) -> f32 {
        if self.is_muted() {
            0.0
        } else {
            self.audible_volume.as_player_ratio()
        }
    }

    /// Новый уровень после очередного snapshot-а плеера.
    ///
    /// Слышимая громкость обновляется только когда звук включён: при mute плеер
    /// показывает `0.0`, и это не «новая громкость», а тишина поверх прежней.
    #[must_use]
    pub(crate) fn after_player_observation(self, observation: PlayerAudioObservation) -> Self {
        if observation.muted {
            return Self::new(self.audible_volume, MuteState::Muted);
        }
        Self::new(
            VolumePermille::from_ratio(f64::from(observation.volume)),
            MuteState::Audible,
        )
    }

    /// Подтверждает ли snapshot, что плеер уже применил этот уровень.
    ///
    /// При mute слышимую громкость по snapshot-у проверить нельзя (там `0.0`),
    /// поэтому сравнивается только сам факт mute.
    #[must_use]
    pub(crate) fn is_confirmed_by(self, observation: PlayerAudioObservation) -> bool {
        if observation.muted != self.is_muted() {
            return false;
        }
        self.is_muted()
            || VolumePermille::from_ratio(f64::from(observation.volume)) == self.audible_volume
    }

    /// Команды, которые переводят плеер в этот уровень без потери громкости «до mute».
    ///
    /// Порядок важен: сначала `SetVolume(слышимая)` — плеер запоминает её как
    /// громкость для включения звука, потом `SetVolume(0.0)` — mute. Обратный
    /// порядок или один `SetVolume(0.0)` после unmute вернул бы громкость из
    /// fallback-а вместо сохранённой.
    #[must_use]
    pub(crate) fn player_restore_commands(self) -> Vec<PlayerCommand> {
        let mut commands = vec![PlayerCommand::SetVolume(
            self.audible_volume.as_player_ratio(),
        )];
        if self.is_muted() && !self.audible_volume.is_silent() {
            commands.push(PlayerCommand::SetVolume(0.0));
        }
        commands
    }
}

/// Отправляет уровень в player binding неблокирующими командами.
///
/// Ошибка не глотается: вызывающий решает, логировать её или считать уровень
/// недоставленным. Команды после первой неудачной не отправляются, чтобы плеер
/// не получил «половину» уровня (громкость без mute или наоборот) молча.
pub(crate) fn send_user_audio_level_to_player(
    sender: &PlayerCommandSender,
    level: UserAudioLevel,
) -> Result<(), PlayerWorkerSendError> {
    for command in level.player_restore_commands() {
        sender.try_send(command)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
