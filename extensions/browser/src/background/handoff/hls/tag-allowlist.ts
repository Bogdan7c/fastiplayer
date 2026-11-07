// Белый список тегов HLS (сессия 10a2, решение 228). Корень приватности переписчика: плееру уходит
// только то, что здесь ПРОВЕРЕНО и ПЕРЕСОБРАНО из разобранных значений. Всё незнакомое - выбрасывается.
// (Первая версия держала «чёрный список»: оставляла всё, что не узнала как утечку, - и пропускала
// адреса в необычных тегах, регистрах и разделителях.)
//
// Три вида тегов:
// - без значения (`#EXT-X-ENDLIST`): остаются, только если вся строка - ровно имя тега;
// - со скаляром (`#EXT-X-TARGETDURATION:6`): значение обязано подойти под строгое выражение, строка
//   собирается заново; `#EXTINF` теряет название (оно свободный текст);
// - со списком атрибутов (`#EXT-X-KEY:...`): имена только [A-Z0-9-]; `URI` (ровно это имя, в
//   кавычках) отдаётся переписчику; прочие значения - безопасные скаляры или строки в кавычках без
//   `/ \ ? {$ ://`. Нарушение - строка выбрасывается целиком.

import { formatAttributeList, parseAttributeList, type Attribute } from "./attribute-list.ts";

/** Имя тега целиком: `#EXT` и заглавные буквы, цифры, дефис. Пробелов и регистра нет. */
export const TAG_NAME = /^#EXT[A-Z0-9-]*$/;

/** Имя атрибута. */
const ATTRIBUTE_NAME = /^[A-Z0-9-]+$/;

/** Единственное имя атрибута с адресом. */
export const URI_ATTRIBUTE = "URI";

/** Теги без значения: остаются, если строка равна имени. `#EXTM3U` в списке нет: его ставит переписчик. */
const NO_VALUE_TAGS: ReadonlySet<string> = new Set([
  "#EXT-X-ENDLIST",
  "#EXT-X-DISCONTINUITY",
  "#EXT-X-INDEPENDENT-SEGMENTS",
  "#EXT-X-I-FRAMES-ONLY",
  "#EXT-X-GAP",
]);

const DECIMAL_INTEGER = /^\d{1,20}$/;
const BYTE_RANGE = /^\d{1,20}(?:@\d{1,20})?$/;
const PLAYLIST_TYPE = /^(?:VOD|EVENT)$/;
const DATE_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}(?::?\d{2})?)$/;
const DECIMAL_DURATION = /^\d{1,10}(?:\.\d{1,10})?$/;

/** Проверка значения скалярного тега: подходит ли оно (значение собирается обратно как есть). */
const SCALAR_TAGS: ReadonlyMap<string, RegExp> = new Map([
  ["#EXT-X-VERSION", DECIMAL_INTEGER],
  ["#EXT-X-TARGETDURATION", DECIMAL_INTEGER],
  ["#EXT-X-MEDIA-SEQUENCE", DECIMAL_INTEGER],
  ["#EXT-X-DISCONTINUITY-SEQUENCE", DECIMAL_INTEGER],
  ["#EXT-X-BITRATE", DECIMAL_INTEGER],
  ["#EXT-X-PLAYLIST-TYPE", PLAYLIST_TYPE],
  ["#EXT-X-BYTERANGE", BYTE_RANGE],
  ["#EXT-X-PROGRAM-DATE-TIME", DATE_TIME],
]);

/** Название сегмента отделяется запятой; плееру от тега нужна только длительность. */
const SEGMENT_INFO_TAG = "#EXTINF";
const SEGMENT_INFO_TITLE_SEPARATOR = ",";

/** Теги со списком атрибутов. */
export const ATTRIBUTE_TAGS: ReadonlySet<string> = new Set([
  "#EXT-X-KEY",
  "#EXT-X-SESSION-KEY",
  "#EXT-X-MAP",
  "#EXT-X-MEDIA",
  "#EXT-X-STREAM-INF",
  "#EXT-X-I-FRAME-STREAM-INF",
  "#EXT-X-PART",
  "#EXT-X-PART-INF",
  "#EXT-X-PRELOAD-HINT",
  "#EXT-X-RENDITION-REPORT",
  "#EXT-X-SERVER-CONTROL",
  "#EXT-X-START",
  "#EXT-X-SKIP",
]);

/**
 * Теги, чья молчаливая потеря ИЗМЕНИТ смысл: без KEY зашифрованный поток играл бы как открытый, без
 * MAP фрагменты остались бы без init. Нарушение в них - честный отказ плейлисту, а не выброс строки.
 */
export const MEANING_CRITICAL_TAGS: ReadonlySet<string> = new Set(["#EXT-X-KEY", "#EXT-X-SESSION-KEY", "#EXT-X-MAP"]);

/** Значение без кавычек: число (со знаком), 0x-число, разрешение 1280x720 или перечисление. */
const SAFE_UNQUOTED_VALUE = /^(?:-?\d+(?:\.\d+)?|0[xX][0-9A-Fa-f]+|\d+x\d+|[A-Z0-9-]+)$/;

/** Запрещено в значении в кавычках (кроме URI): всё, из чего собирается адрес или путь. */
const FORBIDDEN_IN_QUOTED = /[/\\?]|\{\$|:\/\//;

/**
 * Атрибуты, у которых слэш в кавычках законен по спецификации и несёт не путь: версии формата ключа
 * (`"1/2/5"`) и вторичные кодеки (`"dvh1.05.06/db4h"`).
 */
const QUOTED_VALUE_EXCEPTIONS: ReadonlyMap<string, RegExp> = new Map([
  ["KEYFORMATVERSIONS", /^[0-9/]*$/],
  ["SUPPLEMENTAL-CODECS", /^[A-Za-z0-9.,/-]*$/],
]);

/** Что решено про тег без адресов. */
export type TagVerdict = { readonly kind: "keep"; readonly line: string } | { readonly kind: "drop" };

/** Тег без значения: оставить, если строка - ровно имя. */
export function isNoValueTag(name: string): boolean {
  return NO_VALUE_TAGS.has(name);
}

/** Тег с разбираемым скаляром (в т. ч. EXTINF). */
export function isScalarTag(name: string): boolean {
  return name === SEGMENT_INFO_TAG || SCALAR_TAGS.has(name);
}

/** Пересобрать скалярный тег из проверенного значения; неподходящее значение - выброс строки. */
export function rebuildScalarTag(name: string, value: string): TagVerdict {
  if (name === SEGMENT_INFO_TAG) {
    const separator = value.indexOf(SEGMENT_INFO_TITLE_SEPARATOR);
    const duration = separator === -1 ? value : value.slice(0, separator);
    return DECIMAL_DURATION.test(duration) ? { kind: "keep", line: `${name}:${duration}${SEGMENT_INFO_TITLE_SEPARATOR}` } : { kind: "drop" };
  }
  const pattern = SCALAR_TAGS.get(name);
  return pattern !== undefined && pattern.test(value) ? { kind: "keep", line: `${name}:${value}` } : { kind: "drop" };
}

/** Итог проверки списка атрибутов. */
export type AttributesVerdict =
  | { readonly kind: "valid"; readonly attributes: readonly Attribute[] }
  /** Нарушение: строку выбросить (или, для MEANING_CRITICAL_TAGS, отказать плейлисту). */
  | { readonly kind: "invalid" };

/**
 * Разобрать и проверить атрибуты. `URI` не проверяется по значению (его заменит переписчик), но
 * обязан быть в кавычках.
 */
export function checkAttributes(attributeText: string): AttributesVerdict {
  const parsed = parseAttributeList(attributeText);
  if (!parsed.ok) return { kind: "invalid" };
  for (const attribute of parsed.attributes) {
    if (!ATTRIBUTE_NAME.test(attribute.name)) return { kind: "invalid" };
    if (attribute.name === URI_ATTRIBUTE) {
      if (!attribute.quoted) return { kind: "invalid" };
    } else if (attribute.quoted) {
      const exception = QUOTED_VALUE_EXCEPTIONS.get(attribute.name);
      const safe = exception !== undefined ? exception.test(attribute.value) : !FORBIDDEN_IN_QUOTED.test(attribute.value);
      if (!safe) return { kind: "invalid" };
    } else if (!SAFE_UNQUOTED_VALUE.test(attribute.value)) {
      return { kind: "invalid" };
    }
  }
  return { kind: "valid", attributes: parsed.attributes };
}

/** Собрать тег со списком атрибутов обратно. */
export function formatAttributeTag(name: string, attributes: readonly Attribute[]): string {
  return `${name}:${formatAttributeList(attributes)}`;
}
