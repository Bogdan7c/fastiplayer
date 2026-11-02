// Лимиты протокола моста. Зеркало crates/browser-bridge-protocol/src/limits.rs.
//
// Это часть договора экстеншена и плеера: обе стороны обязаны знать одни и те же
// числа. Менять их можно только вместе с версией протокола и вместе с Rust.

/** Наибольшее JSON-сообщение (управление и события), байт UTF-8. */
export const MAX_JSON_MESSAGE_BYTES = 128 * 1024;

/** Наибольший полезный груз одной двоичной рамки, байт. */
export const MAX_DATA_FRAME_PAYLOAD_BYTES = 1024 * 1024;

/** Размер заголовка двоичной рамки, байт. */
export const DATA_FRAME_HEADER_BYTES = 24;

/** Наибольшее число SourceBuffer в одном поколении (индексы 0..7). */
export const MAX_SOURCE_BUFFERS_PER_GENERATION = 8;

/** Наибольшая длина заголовка страницы, байт UTF-8. */
export const MAX_PAGE_TITLE_BYTES = 1024;

/** Наибольшая длина домена страницы, байт. */
export const MAX_SITE_DOMAIN_BYTES = 253;

/** Наибольшая длина MIME-типа, байт ASCII. */
export const MAX_MIME_TYPE_BYTES = 256;

/** Наибольшая длина непрозрачного идентификатора (ключ сеанса, ID варианта), байт. */
export const MAX_OPAQUE_ID_BYTES = 64;

/** Наименьшая длина одноразового токена сеанса, байт. */
export const MIN_SESSION_TOKEN_BYTES = 32;

/** Наибольшая длина одноразового токена сеанса, байт. */
export const MAX_SESSION_TOKEN_BYTES = 128;

/** Наибольшая длина подписи варианта (озвучка, качество, субтитры), байт UTF-8. */
export const MAX_VARIANT_LABEL_BYTES = 256;

/** Наибольшее число вариантов в одном меню. */
export const MAX_VARIANT_OPTIONS = 256;

/** Наибольшее число меток возможностей в `hello`. */
export const MAX_CAPABILITY_TOKENS = 64;

/** Наибольшая длина одной метки возможности, байт ASCII. */
export const MAX_CAPABILITY_TOKEN_BYTES = 64;

/** Наибольшая длина строки версии программы, байт ASCII. */
export const MAX_SOFTWARE_VERSION_BYTES = 32;

/** Наибольший запрос байтов файла `<video src>` за один раз, байт. */
export const MAX_RANGE_FETCH_BYTES = 16 * 1024 * 1024;

/**
 * Наибольшее целое, которое JavaScript хранит точно (2^53 − 1).
 * Все целые JSON протокола не больше этого числа.
 */
export const MAX_JSON_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;

/** Наименьшая скорость воспроизведения, тысячные доли (0,063×). */
export const MIN_PLAYBACK_RATE_PERMILLE = 63;

/** Наибольшая скорость воспроизведения, тысячные доли (16×). */
export const MAX_PLAYBACK_RATE_PERMILLE = 16_000;
