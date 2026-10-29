//! Источник одноразовых пропусков сеанса (инвариант: пропуск секретен и случаен).
//!
//! Пропуск защищает loopback-порт от чужих процессов и страниц (решение 82:
//! `Origin` не годится, у Firefox он — UUID на установку). Поэтому он берётся из
//! генератора ОС, а источник спрятан за трейтом: тесты подставляют
//! детерминированный или падающий.
//!
//! base64url мы кодируем сами (15 строк, `encode_base64url`), а не тянем crate
//! `base64`: нужен один вид кодирования фиксированной длины, а каждая лишняя
//! зависимость — это поверхность аудита (память проекта о непроверенных crate-ах).
//! Корректность закреплена векторами RFC 4648.

use browser_bridge_protocol::SessionToken;
use browser_bridge_protocol::error::ValueViolation;

/// Сколько случайных байт в пропуске: 24 байта = 192 бита.
const TOKEN_ENTROPY_BYTES: usize = 24;

/// Алфавит base64url (RFC 4648, раздел 5).
const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Почему не удалось выдать пропуск.
#[derive(Debug, thiserror::Error)]
pub enum TokenSourceError {
    /// Генератор случайных чисел ОС недоступен.
    #[error("operating system random source failed: {reason}")]
    EntropyUnavailable {
        /// Текст ошибки ОС.
        reason: String,
    },
    /// Источник выдал строку, не подходящую под правила `SessionToken`.
    #[error("token source produced a malformed token: {0}")]
    MalformedToken(#[from] ValueViolation),
}

/// Выдаёт новые пропуски. `Send`: источник живёт внутри сервера и используется из
/// разных потоков (под замком).
pub trait SessionTokenSource: Send {
    /// Следующий пропуск; каждый вызов — новое непредсказуемое значение.
    ///
    /// # Errors
    /// [`TokenSourceError`] — пропуск выдать нельзя, сеанс не открывается.
    fn next_token(&mut self) -> Result<SessionToken, TokenSourceError>;
}

/// Боевой источник: случайные байты ОС (`getrandom`) → base64url без добивки.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsRandomTokenSource;

impl SessionTokenSource for OsRandomTokenSource {
    fn next_token(&mut self) -> Result<SessionToken, TokenSourceError> {
        let mut random_bytes = [0_u8; TOKEN_ENTROPY_BYTES];
        getrandom::fill(&mut random_bytes).map_err(|error| {
            TokenSourceError::EntropyUnavailable {
                reason: error.to_string(),
            }
        })?;
        Ok(SessionToken::new(encode_base64url(&random_bytes))?)
    }
}

/// base64url без добивки: каждые 3 байта дают 4 символа, остаток — 2 или 3.
fn encode_base64url(input: &[u8]) -> String {
    let mut encoded = String::with_capacity(input.len().div_ceil(3) * 4);
    for group in input.chunks(3) {
        // Складываем до трёх байт в 24-битное число (недостающие — нули).
        let mut bits = 0_u32;
        for (position, byte) in group.iter().enumerate() {
            bits |= u32::from(*byte) << (16 - 8 * position);
        }
        // Из группы в n байт получается n + 1 символ по 6 бит.
        for symbol_index in 0..=group.len() {
            let sextet = (bits >> (18 - 6 * symbol_index)) & 0x3F;
            encoded.push(alphabet_char(sextet));
        }
    }
    encoded
}

/// Символ алфавита по шестибитному числу (старшие биты отбрасываются маской).
fn alphabet_char(sextet: u32) -> char {
    let index = (sextet & 0x3F) as usize;
    BASE64URL_ALPHABET
        .get(index)
        .map_or('A', |symbol| char::from(*symbol))
}

#[cfg(test)]
mod tests;
