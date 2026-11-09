// Общие проверки значений, пришедших из MAIN-мира (участок 1).
//
// MAIN-мир делит мир со страницей, поэтому любое его сообщение — возможная подделка. Здесь лежат
// простые кирпичики строгой проверки: «обычная запись ровно с такими полями», «целое в диапазоне»,
// «одно из», «настоящие байты этого мира», «адрес файла http(s)», «факты Referer». Ими пользуются
// пограничник событий страницы (`page-message-validation.ts`) и пограничник субтитров страницы
// (`page-subtitle-validation.ts`, сессия 13b2). Вынесены из `page-message-validation.ts` без
// изменения поведения: тот файл упёрся в предел размера, а второму пограничнику нужны те же правила.

import { REFERRER_POLICIES, type ReferrerPolicyName } from "./referrer-policy.ts";
import { MAX_JSON_SAFE_INTEGER } from "../protocol/index.ts";

/**
 * Наибольший груз одного события `bytes`, байт.
 *
 * Это один `appendBuffer` страницы (или его часть). Реальные куски эталонных сайтов — до единиц МиБ
 * (YouTube 0,1–2 МиБ, сегмент hls.js до ~5 МиБ); 4K-сегмент в 4 с на 40 Мбит/с — около 20 МиБ.
 * 32 МиБ покрывает их с запасом и при этом не даёт странице одним сообщением раздуть память фона
 * (очередь сеанса тоже ограничена, см. background/handoff/constants.ts). Больший кусок MAIN-мир
 * обязан прислать частями по этому пределу.
 */
export const MAX_PAGE_BYTES_EVENT_BYTES = 32 * 1024 * 1024;

/** Наибольший номер вида u32 (номера запросов, `seek_id`). */
export const MAX_U32 = 0xffff_ffff;

/**
 * Наибольшая длина адреса файла в событии `file_source`, символов. Подписанные адреса CDN бывают
 * в единицы КиБ; 16 Ки символов покрывают их с запасом и не дают странице раздуть память фона
 * одним «адресом».
 */
export const MAX_FILE_SOURCE_URL_LENGTH = 16 * 1024;

/** Наибольшая длина `referrer_origin`: схема, порт и имя хоста (не длиннее имени хоста DNS). */
export const MAX_REFERRER_ORIGIN_LENGTH = 300;

/** Схемы адреса файла, который фон умеет качать. */
const FILE_SOURCE_PROTOCOLS: readonly string[] = ["http:", "https:"];

/** Обычная запись (не массив и не вид на байты) с неизвестными пока полями. */
export type PlainRecord = Readonly<Record<string, unknown>>;

/** Факты Referer источника (решение 202): сайт-источник или `null` и известная политика. */
export interface ReferrerFacts {
  readonly origin: string | null;
  readonly policy: ReferrerPolicyName;
}

export function isPlainRecord(value: unknown): value is PlainRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value) && !ArrayBuffer.isView(value);
}

/** Ровно эти собственные поля: лишнее или недостающее поле — отказ. */
export function hasExactKeys(record: PlainRecord, keys: readonly string[]): boolean {
  const ownKeys = Object.keys(record);
  return ownKeys.length === keys.length && keys.every((key) => Object.hasOwn(record, key));
}

export function isIntegerInRange(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= min && value <= max;
}

export const isMediaTime = (value: unknown): value is number => isIntegerInRange(value, 0, MAX_JSON_SAFE_INTEGER);
export const isU32 = (value: unknown): value is number => isIntegerInRange(value, 0, MAX_U32);

export function isOneOf<T extends string>(value: unknown, allowed: readonly T[]): value is T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value);
}

/**
 * Байты события: настоящий `Uint8Array` этого мира на обычном (не разделяемом) буфере,
 * 1 байт…`maxBytes`. `SharedArrayBuffer` отвергаем: страница могла бы менять байты уже после
 * проверки, а Chrome такой буфер всё равно не передаёт фону.
 */
export function isPageBytes(value: unknown, maxBytes: number = MAX_PAGE_BYTES_EVENT_BYTES): value is Uint8Array {
  return value instanceof Uint8Array && value.buffer instanceof ArrayBuffer && value.byteLength >= 1 && value.byteLength <= maxBytes;
}

/**
 * Адрес файла для фона: абсолютный http(s), без логина/пароля в адресе (`fetch` такой адрес
 * отвергает), уже в каноническом виде (`href` совпадает со строкой — так его отдаёт `currentSrc`),
 * не длиннее MAX_FILE_SOURCE_URL_LENGTH. Всё прочее — мусор страницы.
 */
export function isFileSourceUrl(value: unknown): value is string {
  if (typeof value !== "string" || value.length === 0 || value.length > MAX_FILE_SOURCE_URL_LENGTH) return false;
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return false;
  }
  return FILE_SOURCE_PROTOCOLS.includes(parsed.protocol) && parsed.username === "" && parsed.password === "" && parsed.href === value;
}

/** Сайт-источник для Referer: ровно происхождение http(s), без пути, запроса и учётных данных. */
export function isReferrerOrigin(value: unknown): value is string {
  if (typeof value !== "string" || value.length === 0 || value.length > MAX_REFERRER_ORIGIN_LENGTH) return false;
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return false;
  }
  return FILE_SOURCE_PROTOCOLS.includes(parsed.protocol) && parsed.origin === value;
}

/** Факты Referer источника (решение 202): сайт-источник или `null` и известная политика; иначе `undefined`. */
export function referrerFactsOf(origin: unknown, policy: unknown): ReferrerFacts | undefined {
  if (origin !== null && !isReferrerOrigin(origin)) return undefined;
  return isOneOf(policy, REFERRER_POLICIES) ? { origin, policy } : undefined;
}
