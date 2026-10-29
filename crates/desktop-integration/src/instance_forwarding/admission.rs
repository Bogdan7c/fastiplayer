//! Проверка пересланного запроса до передачи приложению.
//!
//! Правила одинаковы для отправителя и получателя. Отправитель узнаёт о слишком
//! большом запросе сразу, получатель не доверяет отправителю и проверяет всё заново.
//!
//! Лимиты — инварианты протокола, а не пользовательские настройки. Они защищают
//! первый экземпляр от случайно огромного или испорченного запроса и выбраны с
//! большим запасом для реальных сценариев.

use thiserror::Error;

use super::{ForwardedInstanceAction, ForwardedUri};
use crate::peer_admission::{PeerAdmissionDenial, admit_peer_uid};

/// Сколько URI можно передать одним запросом. С запасом покрывает «выделить всё»
/// в большой папке; что делать с таким количеством, решают лимиты очереди приложения.
pub const MAX_FORWARDED_URIS: usize = 10_000;

/// Максимальная длина одного URI в байтах. Самый длинный путь Linux (`PATH_MAX`,
/// 4096 байт), закодированный целиком через `%XX`, занимает около 12 КиБ.
pub const MAX_FORWARDED_URI_BYTES: usize = 16 * 1024;

/// Суммарный размер всех URI запроса в байтах.
pub const MAX_TOTAL_FORWARDED_URI_BYTES: usize = 4 * 1024 * 1024;

/// Максимальная длина билета активации. Реальные билеты KWin/Mutter/X11 — это
/// десятки символов; длинное значение — мусор.
pub const MAX_ACTIVATION_TOKEN_BYTES: usize = 1024;

/// Почему запрос отклонён. Причина пишется в лог обеих сторон и уходит отправителю.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ForwardedRequestRejection {
    /// URI больше, чем [`MAX_FORWARDED_URIS`].
    #[error("Too many URIs in the request: {count} (limit {MAX_FORWARDED_URIS})")]
    TooManyUris {
        /// Сколько URI пришло.
        count: usize,
    },
    /// Один URI длиннее [`MAX_FORWARDED_URI_BYTES`].
    #[error("URI {position} is longer than the limit of {MAX_FORWARDED_URI_BYTES} bytes")]
    UriTooLong {
        /// Номер URI в запросе (с единицы).
        position: usize,
    },
    /// Суммарный размер больше [`MAX_TOTAL_FORWARDED_URI_BYTES`].
    #[error(
        "The total size of the URI is greater than the limit of {MAX_TOTAL_FORWARDED_URI_BYTES} bytes"
    )]
    TotalTooLarge,
    /// Строка не является URI со схемой (`file:`, `https:` …).
    #[error("Element {position} is not a URI with a diagram")]
    UriWithoutScheme {
        /// Номер URI в запросе (с единицы).
        position: usize,
    },
    /// В URI есть управляющие символы (перевод строки, NUL и т.п.).
    #[error("URI #{position} contains control characters")]
    UriWithControlCharacter {
        /// Номер URI в запросе (с единицы).
        position: usize,
    },
    /// Запрос пришёл от процесса другого пользователя.
    #[error("request from another user's process (uid {peer_uid})")]
    ForeignUser {
        /// uid отправителя по данным шины.
        peer_uid: u32,
    },
    /// Отправителя не удалось опознать (шина не сообщила uid).
    #[error("It was not possible to determine the sender of the request")]
    UnidentifiedPeer,
    /// Запрошено действие, которое протокол не исполняет (`ActivateAction`).
    #[error("action not supported")]
    UnsupportedAction,
}

/// Проверяет список URI и превращает его в действие.
///
/// Пустой список трактуется как «только поднять окно»: так ведёт себя и
/// `Open` без файлов у других приложений, и это безопасно.
pub(super) fn admit_open_uris(
    uris: Vec<String>,
) -> Result<ForwardedInstanceAction, ForwardedRequestRejection> {
    if uris.is_empty() {
        return Ok(ForwardedInstanceAction::Activate);
    }
    if uris.len() > MAX_FORWARDED_URIS {
        return Err(ForwardedRequestRejection::TooManyUris { count: uris.len() });
    }

    let mut total_bytes = 0_usize;
    for (index, uri) in uris.iter().enumerate() {
        let position = index + 1;
        check_single_uri(uri, position)?;
        total_bytes = total_bytes.saturating_add(uri.len());
        if total_bytes > MAX_TOTAL_FORWARDED_URI_BYTES {
            return Err(ForwardedRequestRejection::TotalTooLarge);
        }
    }

    Ok(ForwardedInstanceAction::Open(
        uris.into_iter().map(ForwardedUri).collect(),
    ))
}

/// Правила для одного URI: длина, отсутствие управляющих символов, схема.
fn check_single_uri(uri: &str, position: usize) -> Result<(), ForwardedRequestRejection> {
    if uri.len() > MAX_FORWARDED_URI_BYTES {
        return Err(ForwardedRequestRejection::UriTooLong { position });
    }
    // Управляющие символы (включая перевод строки и NUL) в корректном URI всегда
    // закодированы как %XX. Буквальный символ означает мусор или попытку подмены.
    if uri.chars().any(char::is_control) {
        return Err(ForwardedRequestRejection::UriWithControlCharacter { position });
    }
    if !has_uri_scheme(uri) {
        return Err(ForwardedRequestRejection::UriWithoutScheme { position });
    }
    Ok(())
}

/// Схема по RFC 3986: `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` и затем `:`.
fn has_uri_scheme(uri: &str) -> bool {
    let Some((scheme, _rest)) = uri.split_once(':') else {
        return false;
    };
    let mut scheme_characters = scheme.chars();
    let first_is_letter = scheme_characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic());
    first_is_letter
        && scheme_characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// Билет активации — непустая строка из печатных ASCII-символов разумной длины.
pub(super) fn is_valid_activation_token(raw_token: &str) -> bool {
    !raw_token.is_empty()
        && raw_token.len() <= MAX_ACTIVATION_TOKEN_BYTES
        && raw_token.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Политика доступа: принимаются только запросы от процессов того же пользователя.
///
/// Правило общее для всех D-Bus-служб crate-а ([`admit_peer_uid`]); здесь вердикт
/// лишь переводится в отказ пересылки, тексты которого не меняются.
pub(super) fn admit_peer(
    peer_uid: Option<u32>,
    own_uid: u32,
) -> Result<(), ForwardedRequestRejection> {
    admit_peer_uid(peer_uid, own_uid).map_err(ForwardedRequestRejection::from)
}

impl From<PeerAdmissionDenial> for ForwardedRequestRejection {
    fn from(denial: PeerAdmissionDenial) -> Self {
        match denial {
            PeerAdmissionDenial::ForeignUser { peer_uid } => Self::ForeignUser { peer_uid },
            PeerAdmissionDenial::UnidentifiedPeer => Self::UnidentifiedPeer,
        }
    }
}

#[cfg(test)]
mod tests;
