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
//!
//! Форма самого `Hello` и отказа по версии не меняется никогда, даже между
//! `major`: иначе стороны не смогут договориться о том, что они разные.

use serde::{Deserialize, Serialize};

/// Версия протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    /// Несовместимые изменения.
    pub major: u16,
    /// Совместимые добавления внутри `major`.
    pub minor: u16,
}

/// Версия протокола, которую реализует этот crate.
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// Кто из участников моста.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
