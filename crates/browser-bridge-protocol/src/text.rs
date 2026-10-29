//! Строки протокола с правилами допуска.
//!
//! Всё, что пришло со страницы, — данные врага (инвариант 1). Поэтому каждая
//! строка протокола — отдельный тип, который нельзя получить в обход проверки:
//! ни при разборе JSON, ни в коде (конструктор `new` проверяет так же).
//!
//! Правила двух видов:
//! - **текст для человека** (заголовок, подписи): любой Unicode — эмодзи,
//!   арабский, иврит, — кроме управляющих символов, разделителей строк и
//!   символов, переворачивающих направление текста (ими подделывают надписи:
//!   «exe.mp4» вместо «mp4.exe»). Чистит заголовок перед отправкой экстеншен;
//!   протокол такой текст не исправляет, а отвергает;
//! - **машинные строки** (домен, MIME, идентификаторы, токен): узкий набор ASCII.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ValueViolation;
use crate::limits::{
    MAX_CAPABILITY_TOKEN_BYTES, MAX_MIME_TYPE_BYTES, MAX_OPAQUE_ID_BYTES, MAX_PAGE_TITLE_BYTES,
    MAX_SESSION_TOKEN_BYTES, MAX_SITE_DOMAIN_BYTES, MAX_SOFTWARE_VERSION_BYTES,
    MAX_VARIANT_LABEL_BYTES, MIN_SESSION_TOKEN_BYTES,
};

/// Можно ли строке быть пустой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emptiness {
    Allowed,
    Forbidden,
}

/// Правило допуска одного вида строк.
#[derive(Debug, Clone, Copy)]
struct TextRule {
    emptiness: Emptiness,
    min_bytes: usize,
    max_bytes: usize,
    allowed_char: fn(char) -> bool,
}

impl TextRule {
    /// Проверяет строку целиком. Порядок проверок фиксирован (пустота → длина →
    /// символы), чтобы причина отказа была однозначной для фикстур.
    fn check(self, text: &str) -> Result<(), ValueViolation> {
        if text.is_empty() {
            return match self.emptiness {
                Emptiness::Allowed => Ok(()),
                Emptiness::Forbidden => Err(ValueViolation::TextEmpty),
            };
        }
        if text.len() > self.max_bytes {
            return Err(ValueViolation::TextTooLong);
        }
        // Нижняя граница есть только у токена: короткий легко подобрать.
        if text.len() < self.min_bytes {
            return Err(ValueViolation::TextTooShort);
        }
        if text.chars().all(self.allowed_char) {
            Ok(())
        } else {
            Err(ValueViolation::TextForbiddenCharacter)
        }
    }
}

/// Символ допустим в тексте для человека.
///
/// Запрещены:
/// - управляющие C0 (U+0000–U+001F, включая нулевой байт, табуляцию и перевод
///   строки), DEL (U+007F) и C1 (U+0080–U+009F);
/// - разделители строки и абзаца U+2028/U+2029: подпись однострочная;
/// - встраивания, переопределения и изоляции направления текста
///   (U+202A–U+202E, U+2066–U+2069) — инструмент подделки надписей.
///
/// Метки направления U+200E/U+200F и соединитель U+200D (эмодзи-семьи)
/// разрешены: они нужны настоящему тексту и не переворачивают его.
fn is_allowed_display_char(character: char) -> bool {
    !(character.is_control()
        || matches!(character, '\u{2028}' | '\u{2029}')
        || matches!(character, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
}

/// Символ допустим в домене: имя хоста в нижнем регистре (IDN уже в punycode,
/// так отдаёт `location.hostname`), плюс `[`, `]`, `:` для адресов IPv6.
fn is_allowed_domain_char(character: char) -> bool {
    character.is_ascii_lowercase()
        || character.is_ascii_digit()
        || matches!(character, '-' | '.' | '[' | ']' | ':')
}

/// Символ допустим в MIME-типе: печатный ASCII с пробелом
/// (`video/mp4; codecs="avc1.64001f"`).
fn is_allowed_mime_char(character: char) -> bool {
    matches!(character, ' '..='~')
}

/// Символ допустим в идентификаторе и токене: алфавит base64url.
fn is_allowed_opaque_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

/// Символ допустим в метке возможности (`cheap_codec.h264`).
fn is_allowed_capability_char(character: char) -> bool {
    character.is_ascii_lowercase()
        || character.is_ascii_digit()
        || matches!(character, '_' | '.' | '-')
}

/// Символ допустим в строке версии программы (`0.2.0`, `1.4.0-beta.2+abc`).
fn is_allowed_version_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+')
}

/// Объявляет проверяемую строку протокола.
///
/// Даёт одинаковую обвязку всем видам строк: разбор из JSON только через
/// проверку (`try_from = "String"`), запись в JSON как обычная строка,
/// конструктор `new` с той же проверкой и чтение `as_str`. Сами правила —
/// в константе `RULE` у каждого типа.
macro_rules! checked_text {
    ($(#[$meta:meta])* $name:ident, $rule:expr) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Правило допуска этого вида строк.
            const RULE: TextRule = $rule;

            /// Проверяет строку и оборачивает её.
            ///
            /// # Errors
            /// Нарушение правила допуска — пустота, длина или запрещённый символ.
            pub fn new(text: impl Into<String>) -> Result<Self, ValueViolation> {
                let text = text.into();
                Self::RULE.check(&text)?;
                Ok(Self(text))
            }

            /// Строка как есть.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValueViolation;

            fn try_from(text: String) -> Result<Self, Self::Error> {
                Self::new(text)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

checked_text!(
    /// Заголовок вкладки для подписи окна. Может быть пустым (страница без `<title>`).
    #[derive(Debug)]
    PageTitle,
    TextRule {
        emptiness: Emptiness::Allowed,
        min_bytes: 0,
        max_bytes: MAX_PAGE_TITLE_BYTES,
        allowed_char: is_allowed_display_char,
    }
);

checked_text!(
    /// Домен страницы (`www.youtube.com`) — единственное, что плеер знает об адресе
    /// (инвариант 2: ни путей, ни параметров, ни кук).
    #[derive(Debug)]
    SiteDomain,
    TextRule {
        emptiness: Emptiness::Forbidden,
        min_bytes: 1,
        max_bytes: MAX_SITE_DOMAIN_BYTES,
        allowed_char: is_allowed_domain_char,
    }
);

checked_text!(
    /// MIME-тип с кодеками из `addSourceBuffer`/`changeType`.
    #[derive(Debug)]
    MimeType,
    TextRule {
        emptiness: Emptiness::Forbidden,
        min_bytes: 1,
        max_bytes: MAX_MIME_TYPE_BYTES,
        allowed_char: is_allowed_mime_char,
    }
);

checked_text!(
    /// Ключ сеанса: экстеншен даёт его одному видеоэлементу в одной вкладке.
    ///
    /// Повтор `open_session` с тем же ключом не открывает второго окна
    /// (инвариант 6, `crate::open_session`).
    #[derive(Debug)]
    SessionKey,
    TextRule {
        emptiness: Emptiness::Forbidden,
        min_bytes: 1,
        max_bytes: MAX_OPAQUE_ID_BYTES,
        allowed_char: is_allowed_opaque_char,
    }
);

checked_text!(
    /// Идентификатор варианта в меню (озвучка, качество, субтитры). Его придумывает
    /// адаптер сайта и узнаёт обратно в команде `select_variant`.
    #[derive(Debug)]
    VariantId,
    TextRule {
        emptiness: Emptiness::Forbidden,
        min_bytes: 1,
        max_bytes: MAX_OPAQUE_ID_BYTES,
        allowed_char: is_allowed_opaque_char,
    }
);

checked_text!(
    /// Подпись варианта в меню плеера («Українська», «1080p», «English (auto)»).
    #[derive(Debug)]
    VariantLabel,
    TextRule {
        emptiness: Emptiness::Forbidden,
        min_bytes: 1,
        max_bytes: MAX_VARIANT_LABEL_BYTES,
        allowed_char: is_allowed_display_char,
    }
);

checked_text!(
    /// Метка возможности стороны в `Hello` (`cheap_codec.h264`).
    #[derive(Debug)]
    CapabilityToken,
    TextRule {
        emptiness: Emptiness::Forbidden,
        min_bytes: 1,
        max_bytes: MAX_CAPABILITY_TOKEN_BYTES,
        allowed_char: is_allowed_capability_char,
    }
);

checked_text!(
    /// Версия программы для диагностики (`0.2.0`). На совместимость не влияет —
    /// для этого есть версия протокола.
    #[derive(Debug)]
    SoftwareVersion,
    TextRule {
        emptiness: Emptiness::Forbidden,
        min_bytes: 1,
        max_bytes: MAX_SOFTWARE_VERSION_BYTES,
        allowed_char: is_allowed_version_char,
    }
);

checked_text!(
    /// Одноразовый пропуск в сеанс по WebSocket (решение 82).
    ///
    /// Его выдаёт плеер через посредника, и предъявляет экстеншен первым
    /// сообщением сеанса. В отладочный вывод не попадает.
    SessionToken,
    TextRule {
        emptiness: Emptiness::Forbidden,
        min_bytes: MIN_SESSION_TOKEN_BYTES,
        max_bytes: MAX_SESSION_TOKEN_BYTES,
        allowed_char: is_allowed_opaque_char,
    }
);

impl fmt::Debug for SessionToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Токен — секрет сеанса: даже в журнале отладки только факт наличия.
        formatter.write_str("SessionToken(<redacted>)")
    }
}

impl SessionToken {
    /// Сравнивает предъявленный пропуск с выданным за постоянное время.
    ///
    /// Обычное `==` выходит на первом несовпавшем байте, и по времени ответа
    /// можно угадывать токен по символу. Здесь время зависит только от длины.
    #[must_use]
    pub fn matches(&self, presented: &Self) -> bool {
        let expected = self.0.as_bytes();
        let candidate = presented.0.as_bytes();
        if expected.len() != candidate.len() {
            return false;
        }
        // Накопление различий без раннего выхода.
        let difference = expected
            .iter()
            .zip(candidate)
            .fold(0_u8, |accumulated, (left, right)| {
                accumulated | (left ^ right)
            });
        difference == 0
    }
}

#[cfg(test)]
mod tests;
