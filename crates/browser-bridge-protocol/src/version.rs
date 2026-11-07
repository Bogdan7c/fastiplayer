//! Версия протокола и договорённость о ней (решение владельца 86).
//!
//! Правило «договорились — значит строго»:
//! 1. Версия — пара `major.minor`. Внутри одного `major` новая `minor` только
//!    добавляет сообщения, поля и значения; старое не меняется и не удаляется.
//! 2. В `Hello` стороны называют свои версии. Общая версия — тот же `major` и
//!    меньшая из двух `minor`.
//! 3. Более новая сторона говорит на языке общей версии: не шлёт того, чего
//!    в ней нет. Поэтому получатель разбирает строго — незнакомое поле или тип
//!    означает ошибку отправителя, а не «новую фичу», и ничего не теряется молча
//!    (инвариант 5).
//! 4. Разные `major` — понятный отказ «обновите» с указанием, кого обновлять.
//! 5. Каждое сообщение знает, в какой версии оно появилось
//!    (`PageSessionMessage::introduced_in`, `PlayerSessionMessage::introduced_in`).
//!    Сообщение новее общей версии — нарушение отправителя: получатель его не
//!    исполняет (на стороне плеера это проверяет `crate::stream_gate::StreamGate`),
//!    а отправитель до отправки сверяет [`ProtocolVersion::includes`].
//!
//! Форма самого `Hello` и отказа по версии не меняется никогда, даже между
//! `major`: иначе стороны не смогут договориться о том, что они разные.
//!
//! История `minor` внутри `major` 1:
//! - **1.0** (сессии 01–10e) — MSE, файл `<video src>` (`file_source_added`,
//!   `fetch_range`), реклама, часы и команды;
//! - **1.1** (сессия 10a1, решение 228) — нативный HLS по номеркам ресурсов:
//!   `hls_source_added`, `fetch_resource`, `resource_fetch_finished`. При общей
//!   1.0 их не шлёт ни одна сторона, и HLS-поколение не начинается.
//! - **1.2** (сессия 10a3, решение 232) — HLS live: `live_window` (плеер →
//!   страница, честное окно перемотки эфира). VOD HLS по-прежнему хватает 1.1;
//!   при общей 1.1 плеер не шлёт `live_window`, а экстеншен оставляет для live
//!   HLS честный отказ «not supported yet».

use crate::strict_shape::{strict_label, strict_object};
use serde::{Deserialize, Serialize};

// Строгая форма JSON (`crate::strict_shape`): объекты и перечисления с меткой —
// только объектом, метки — только строкой; чужая форма — `wrong_value_type`.
strict_object!(ProtocolVersion);
strict_label!(BridgeSoftware);

/// Версия протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct ProtocolVersion {
    /// Несовместимые изменения.
    pub major: u16,
    /// Совместимые добавления внутри `major`.
    pub minor: u16,
}

impl ProtocolVersion {
    /// Первая версия (сессия 01): MSE, файл `<video src>`, часы, команды, реклама.
    pub const V1_0: Self = Self { major: 1, minor: 0 };

    /// Нативный HLS по номеркам ресурсов (сессия 10a1, решение 228).
    pub const V1_1: Self = Self { major: 1, minor: 1 };

    /// HLS live: окно эфира `live_window` (сессия 10a3, решение 232).
    pub const V1_2: Self = Self { major: 1, minor: 2 };

    /// Входит ли в эту версию (обычно — общую версию сеанса) то, что появилось
    /// в версии `introduced_in`.
    ///
    /// Тот же `major` и не меньшая `minor`: `minor` только добавляет (правило 1).
    /// Другой `major` не входит никогда — с ним стороны не договорились бы вовсе.
    #[must_use]
    pub const fn includes(self, introduced_in: Self) -> bool {
        self.major == introduced_in.major && self.minor >= introduced_in.minor
    }
}

/// Версия протокола, которую реализует этот crate.
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V1_2;

/// Кто из участников моста.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum BridgeSoftware {
    /// Экстеншен Fastiplayer Bridge в браузере.
    Extension,
    /// Плеер Fastiplayer.
    Player,
}

/// Версии несовместимы: кого обновить, чтобы заработало.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("browser bridge protocol versions are incompatible; update {update:?}")]
pub struct IncompatibleVersion {
    /// Сторона со старым `major`.
    pub update: BridgeSoftware,
}

/// Договаривается об общей версии протокола.
///
/// Аргументы названы по ролям, а не «моя/чужая», чтобы обе стороны вызывали
/// одну функцию и получали одинаковый ответ.
///
/// # Errors
/// Разные `major` — [`IncompatibleVersion`] со стороной, у которой `major`
/// меньше: обновлять надо отставшего.
pub fn negotiate_protocol_version(
    player: ProtocolVersion,
    extension: ProtocolVersion,
) -> Result<ProtocolVersion, IncompatibleVersion> {
    match player.major.cmp(&extension.major) {
        std::cmp::Ordering::Less => Err(IncompatibleVersion {
            update: BridgeSoftware::Player,
        }),
        std::cmp::Ordering::Greater => Err(IncompatibleVersion {
            update: BridgeSoftware::Extension,
        }),
        std::cmp::Ordering::Equal => Ok(ProtocolVersion {
            major: player.major,
            minor: player.minor.min(extension.minor),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    #[test]
    fn same_major_agrees_on_the_older_minor_for_both_roles() {
        assert_eq!(
            negotiate_protocol_version(version(1, 3), version(1, 1)),
            Ok(version(1, 1))
        );
        assert_eq!(
            negotiate_protocol_version(version(1, 0), version(1, 7)),
            Ok(version(1, 0))
        );
    }

    #[test]
    fn a_version_includes_only_its_own_and_older_minors_of_the_same_major() {
        assert!(ProtocolVersion::V1_1.includes(ProtocolVersion::V1_0));
        assert!(ProtocolVersion::V1_1.includes(ProtocolVersion::V1_1));
        assert!(!ProtocolVersion::V1_0.includes(ProtocolVersion::V1_1));
        assert!(ProtocolVersion::V1_2.includes(ProtocolVersion::V1_1));
        assert!(ProtocolVersion::V1_2.includes(ProtocolVersion::V1_0));
        assert!(!ProtocolVersion::V1_1.includes(ProtocolVersion::V1_2));
        // Другой major не входит ни в какую сторону, даже с меньшей minor.
        assert!(!version(2, 5).includes(ProtocolVersion::V1_0));
        assert!(!ProtocolVersion::V1_1.includes(version(2, 0)));
    }

    #[test]
    fn this_crate_speaks_protocol_1_2() {
        // Сессия 10a3: HLS live. Число закреплено — его же зеркалит экстеншен
        // (`CURRENT_PROTOCOL_VERSION` в `version.ts`).
        assert_eq!(CURRENT_PROTOCOL_VERSION, version(1, 2));
    }

    #[test]
    fn a_1_2_player_agrees_on_the_older_minor_of_a_1_1_or_1_0_extension() {
        // Новый плеер со старым экстеншеном говорит на языке старого (правило 3).
        for older in [ProtocolVersion::V1_1, ProtocolVersion::V1_0] {
            assert_eq!(
                negotiate_protocol_version(ProtocolVersion::V1_2, older),
                Ok(older)
            );
            assert_eq!(
                negotiate_protocol_version(older, ProtocolVersion::V1_2),
                Ok(older)
            );
        }
    }

    #[test]
    fn older_major_side_is_told_to_update() {
        assert_eq!(
            negotiate_protocol_version(version(1, 9), version(2, 0)),
            Err(IncompatibleVersion {
                update: BridgeSoftware::Player
            })
        );
        assert_eq!(
            negotiate_protocol_version(version(3, 0), version(2, 5)),
            Err(IncompatibleVersion {
                update: BridgeSoftware::Extension
            })
        );
    }
}
