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
 * Наибольший ресурс HLS за один `fetch_resource` (протокол 1.1), байт. Тело длиннее
 * `max_bytes` запроса фон не отдаёт, а отвечает `too_large`.
 */
export const MAX_RESOURCE_FETCH_BYTES = 128 * 1024 * 1024;

/**
 * Сколько незавершённых `fetch_resource` плеер держит одновременно (протокол 1.1).
 * На лишний запрос фон вправе ответить `refused`.
 */
export const MAX_CONCURRENT_RESOURCE_FETCHES = 8;

/**
 * Наименьший промежуток между двумя `live_window` одного поколения (протокол 1.2), мс.
 * Договор плеера: частые изменения окна он сводит подряд, последнее окно не теряет,
 * первое окно нового поколения шлёт сразу. Экстеншен частоту не проверяет.
 */
export const MIN_LIVE_WINDOW_INTERVAL_MS = 1_000;

/**
 * Наибольший документ субтитров страницы (протокол 1.5), байт. Документ длиннее экстеншен
 * не предлагает: плеер держит недокачанный документ целиком.
 */
export const MAX_PAGE_SUBTITLE_DOCUMENT_BYTES = 8 * 1024 * 1024;

/**
 * Сколько разных дорожек субтитров страница может предложить за сеанс (протокол 1.5).
 * Повторное предложение того же номера - замена документа и места не занимает.
 */
export const MAX_PAGE_SUBTITLE_TRACKS = 64;

/**
 * Сколько байт документов субтитров страницы плеер держит за сеанс (протокол 1.5), всего:
 * последний документ каждой дорожки плюс недокачанные; замена считает новый размер вместо
 * прежнего. Предложение сверх этого плеер отвергает одно - экстеншен такие не предлагает.
 */
export const MAX_PAGE_SUBTITLE_SESSION_BYTES = 32 * 1024 * 1024;

/** Наибольшая подпись дорожки субтитров страницы, байт UTF-8 (протокол 1.5). */
export const MAX_SUBTITLE_LABEL_BYTES = 256;

/** Наибольший тег языка BCP-47 дорожки субтитров, байт (RFC 5646, раздел 4.4.1; протокол 1.5). */
export const MAX_SUBTITLE_LANGUAGE_TAG_BYTES = 35;

/**
 * Наибольшее целое, которое JavaScript хранит точно (2^53 − 1).
 * Все целые JSON протокола не больше этого числа.
 */
export const MAX_JSON_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;

/** Наименьшая скорость воспроизведения, тысячные доли (0,063×). */
export const MIN_PLAYBACK_RATE_PERMILLE = 63;

/** Наибольшая скорость воспроизведения, тысячные доли (16×). */
export const MAX_PLAYBACK_RATE_PERMILLE = 16_000;
