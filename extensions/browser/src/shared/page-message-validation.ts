// Строгая проверка формы сообщений MAIN-мира (участок 1) по договору handoff-contract.ts.
//
// MAIN-мир делит мир со страницей: страница может подделать ЛЮБОЕ сообщение по нашему порту
// (подменить прототипы до/после нас, вызвать наши функции). Поэтому изолированный мир пропускает
// дальше только сообщения ровно договорной формы: известный `kind`, ровно нужные поля, верные типы
// и диапазоны. Всё остальное — мусор: отбрасывается и считается (см. src/content/relay.ts).
// Тот же разбор фон повторяет над тем, что пришло из изолированного мира (защита в глубину):
// это дёшево, байты при этом не копируются.

import type {
  CandidateMedia,
  CandidatesReply,
  HandoffEndCause,
  HandoffStartOutcome,
  LocalMseEvent,
  PageEvent,
  UnsupportedMediaForm,
  VideoCandidate,
} from "./handoff-contract.ts";
import {
  MAX_JSON_SAFE_INTEGER,
  MAX_MIME_TYPE_BYTES,
  MAX_PLAYBACK_RATE_PERMILLE,
  MAX_SOURCE_BUFFERS_PER_GENERATION,
  MIN_PLAYBACK_RATE_PERMILLE,
  type MediaDuration,
  type PagePlaybackRequest,
  type RemovalEnd,
} from "../protocol/index.ts";

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

/**
 * Сколько видеоэлементов один документ может описать в ответе `candidates`.
 * Настоящие страницы держат единицы видео (лента превью — десятки); предел не даёт враждебной
 * странице заставить фон перебирать миллионы записей.
 */
export const MAX_VIDEO_CANDIDATES_PER_DOCUMENT = 256;

/** Наибольший номер вида u32 (номера запросов, `seek_id`). */
const MAX_U32 = 0xffff_ffff;

/**
 * Локальный ключ MediaSource: «ms-» и номер без ведущих нулей; у MediaSource выделенного воркера
 * (сессия 09b) впереди ещё «w<номер воркера>/» — тоже без ведущих нулей, ровно один уровень.
 * 15 цифр — заведомо больше любого счётчика за жизнь документа и меньше 2^53.
 */
const LOCAL_MEDIA_SOURCE_KEY_PATTERN = /^(?:w[1-9][0-9]{0,14}\/)?ms-[1-9][0-9]{0,14}$/;

/** Ключ, который выдаёт один учёт MSE (главного потока или одного воркера): только «ms-N». */
const TRACKER_MEDIA_SOURCE_KEY_PATTERN = /^ms-[1-9][0-9]{0,14}$/;

/**
 * Метка документа (`DocumentTag`): 1…64 символа base64url (сюда входит и hex). MAIN-мир выбирает её
 * случайно на старте документа; узкий алфавит и длина не дают странице протащить в фон что угодно.
 */
const DOCUMENT_TAG_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;

/** Печатный ASCII с пробелом — правило MIME-типа протокола (text.rs / text.ts). */
const PRINTABLE_ASCII_PATTERN = /^[\x20-\x7e]+$/;

/**
 * Наибольшая длина адреса файла в событии `file_source`, символов. Подписанные адреса CDN бывают
 * в единицы КиБ; 16 Ки символов покрывают их с запасом и не дают странице раздуть память фона
 * одним «адресом».
 */
export const MAX_FILE_SOURCE_URL_LENGTH = 16 * 1024;

/** Схемы адреса файла, который фон умеет качать с `Range`. */
const FILE_SOURCE_PROTOCOLS: readonly string[] = ["http:", "https:"];

const HANDOFF_START_OUTCOMES: readonly HandoffStartOutcome[] = ["started", "element_not_found", "no_media"];
const UNSUPPORTED_MEDIA_FORMS: readonly UnsupportedMediaForm[] = [
  "native_hls",
  "blob_file",
  "data_url",
  "media_stream",
  "other_scheme",
  "untracked_worker_media_source",
];
const HANDOFF_END_CAUSES: readonly HandoffEndCause[] = ["returned", "element_released"];

type PlainRecord = Readonly<Record<string, unknown>>;

// ------------------------------------------------------------------ примитивы

function isPlainRecord(value: unknown): value is PlainRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value) && !ArrayBuffer.isView(value);
}

/** Ровно эти собственные поля: лишнее или недостающее поле — отказ. */
function hasExactKeys(record: PlainRecord, keys: readonly string[]): boolean {
  const ownKeys = Object.keys(record);
  return ownKeys.length === keys.length && keys.every((key) => Object.hasOwn(record, key));
}

function isIntegerInRange(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= min && value <= max;
}

const isMediaTime = (value: unknown): value is number => isIntegerInRange(value, 0, MAX_JSON_SAFE_INTEGER);
const isU32 = (value: unknown): value is number => isIntegerInRange(value, 0, MAX_U32);
const isSourceBufferIndex = (value: unknown): value is number =>
  isIntegerInRange(value, 0, MAX_SOURCE_BUFFERS_PER_GENERATION - 1);
const isPlaybackRate = (value: unknown): value is number =>
  isIntegerInRange(value, MIN_PLAYBACK_RATE_PERMILLE, MAX_PLAYBACK_RATE_PERMILLE);

function isLocalMediaSourceKey(value: unknown): value is string {
  return typeof value === "string" && LOCAL_MEDIA_SOURCE_KEY_PATTERN.test(value);
}

/**
 * Ключ одного учёта MSE без номера воркера («ms-N»). Так говорит хук внутри воркера по своему
 * личному каналу: номер воркера ставит MAIN-мир (src/page/worker-capture).
 */
export function isTrackerMediaSourceKey(value: unknown): value is string {
  return typeof value === "string" && TRACKER_MEDIA_SOURCE_KEY_PATTERN.test(value);
}

/** MIME-тип, который пройдёт строгую проверку протокола: 1…256 байт печатного ASCII. */
function isProtocolMimeType(value: unknown): value is string {
  return typeof value === "string" && value.length <= MAX_MIME_TYPE_BYTES && PRINTABLE_ASCII_PATTERN.test(value);
}

/**
 * Байты события: настоящий `Uint8Array` этого мира на обычном (не разделяемом) буфере,
 * 1 байт…MAX_PAGE_BYTES_EVENT_BYTES. `SharedArrayBuffer` отвергаем: страница могла бы менять
 * байты уже после проверки, а Chrome такой буфер всё равно не передаёт фону.
 */
function isPageBytes(value: unknown): value is Uint8Array {
  return (
    value instanceof Uint8Array &&
    value.buffer instanceof ArrayBuffer &&
    value.byteLength >= 1 &&
    value.byteLength <= MAX_PAGE_BYTES_EVENT_BYTES
  );
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

function isOneOf<T extends string>(value: unknown, allowed: readonly T[]): value is T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value);
}

// ------------------------------------------------------------------ составные поля

function parseRemovalEnd(value: unknown, startUs: number): RemovalEnd | undefined {
  if (!isPlainRecord(value)) return undefined;
  if (value["kind"] === "end_of_timeline" && hasExactKeys(value, ["kind"])) return { kind: "end_of_timeline" };
  // Пустой промежуток протокол отвергает (validatePageSession): отсекаем его здесь же.
  if (value["kind"] === "at" && hasExactKeys(value, ["kind", "time_us"]) && isMediaTime(value["time_us"])) {
    return value["time_us"] > startUs ? { kind: "at", time_us: value["time_us"] } : undefined;
  }
  return undefined;
}

function parseMediaDuration(value: unknown): MediaDuration | undefined {
  if (!isPlainRecord(value)) return undefined;
  switch (value["kind"]) {
    case "unknown":
      return hasExactKeys(value, ["kind"]) ? { kind: "unknown" } : undefined;
    case "live":
      return hasExactKeys(value, ["kind"]) ? { kind: "live" } : undefined;
    case "finite":
      return hasExactKeys(value, ["kind", "duration_us"]) && isMediaTime(value["duration_us"])
        ? { kind: "finite", duration_us: value["duration_us"] }
        : undefined;
    default:
      return undefined;
  }
}

function parsePlaybackRequest(value: unknown): PagePlaybackRequest | undefined {
  if (!isPlainRecord(value)) return undefined;
  switch (value["kind"]) {
    case "play":
      return hasExactKeys(value, ["kind"]) ? { kind: "play" } : undefined;
    case "pause":
      return hasExactKeys(value, ["kind"]) ? { kind: "pause" } : undefined;
    case "set_rate":
      return hasExactKeys(value, ["kind", "rate"]) && isPlaybackRate(value["rate"])
        ? { kind: "set_rate", rate: value["rate"] }
        : undefined;
    default:
      return undefined;
  }
}

// ------------------------------------------------------------------ события MSE

/** Поля «MediaSource + индекс SourceBuffer», общие для событий SourceBuffer. */
function sourceBufferAddress(value: PlainRecord): { media_source: string; source_buffer: number } | undefined {
  const mediaSource = value["media_source"];
  const sourceBuffer = value["source_buffer"];
  return isLocalMediaSourceKey(mediaSource) && isSourceBufferIndex(sourceBuffer)
    ? { media_source: mediaSource, source_buffer: sourceBuffer }
    : undefined;
}

/** Разбор `LocalMseEvent`; `undefined` — форма не договорная. */
export function parseLocalMseEvent(value: unknown): LocalMseEvent | undefined {
  if (!isPlainRecord(value)) return undefined;
  const kind = value["kind"];
  switch (kind) {
    case "source_buffer_added":
    case "source_buffer_type_changed": {
      const address = sourceBufferAddress(value);
      const mimeType = value["mime_type"];
      if (!hasExactKeys(value, ["kind", "media_source", "source_buffer", "mime_type"])) return undefined;
      return address !== undefined && isProtocolMimeType(mimeType) ? { kind, ...address, mime_type: mimeType } : undefined;
    }
    case "timestamp_offset_changed": {
      const address = sourceBufferAddress(value);
      const offset = value["offset_us"];
      if (!hasExactKeys(value, ["kind", "media_source", "source_buffer", "offset_us"])) return undefined;
      const validOffset = isIntegerInRange(offset, -MAX_JSON_SAFE_INTEGER, MAX_JSON_SAFE_INTEGER);
      return address !== undefined && validOffset ? { kind, ...address, offset_us: offset } : undefined;
    }
    case "source_buffer_removed": {
      const address = sourceBufferAddress(value);
      const start = value["start_us"];
      if (!hasExactKeys(value, ["kind", "media_source", "source_buffer", "start_us", "end"])) return undefined;
      if (address === undefined || !isMediaTime(start)) return undefined;
      const end = parseRemovalEnd(value["end"], start);
      return end !== undefined ? { kind, ...address, start_us: start, end } : undefined;
    }
    case "source_buffer_aborted": {
      const address = sourceBufferAddress(value);
      return hasExactKeys(value, ["kind", "media_source", "source_buffer"]) && address !== undefined
        ? { kind, ...address }
        : undefined;
    }
    case "duration_changed": {
      const mediaSource = value["media_source"];
      const duration = parseMediaDuration(value["duration"]);
      return hasExactKeys(value, ["kind", "media_source", "duration"]) && isLocalMediaSourceKey(mediaSource) && duration !== undefined
        ? { kind, media_source: mediaSource, duration }
        : undefined;
    }
    case "end_of_stream": {
      const mediaSource = value["media_source"];
      return hasExactKeys(value, ["kind", "media_source"]) && isLocalMediaSourceKey(mediaSource)
        ? { kind, media_source: mediaSource }
        : undefined;
    }
    default:
      return undefined;
  }
}

// ------------------------------------------------------------------ события страницы

/** Разбор `PageEvent` (MAIN → изолированный мир → фон); `undefined` — мусор. */
export function parsePageEvent(value: unknown): PageEvent | undefined {
  if (!isPlainRecord(value)) return undefined;
  switch (value["kind"]) {
    case "handoff_began": {
      const requestId = value["request_id"];
      const outcome = value["outcome"];
      return hasExactKeys(value, ["kind", "request_id", "outcome"]) && isU32(requestId) && isOneOf(outcome, HANDOFF_START_OUTCOMES)
        ? { kind: "handoff_began", request_id: requestId, outcome }
        : undefined;
    }
    case "mse": {
      const event = parseLocalMseEvent(value["event"]);
      return hasExactKeys(value, ["kind", "event"]) && event !== undefined ? { kind: "mse", event } : undefined;
    }
    case "bytes": {
      const address = sourceBufferAddress(value);
      const bytes = value["bytes"];
      return hasExactKeys(value, ["kind", "media_source", "source_buffer", "bytes"]) && address !== undefined && isPageBytes(bytes)
        ? { kind: "bytes", ...address, bytes }
        : undefined;
    }
    case "page_seek": {
      const target = value["target_us"];
      return hasExactKeys(value, ["kind", "target_us"]) && isMediaTime(target) ? { kind: "page_seek", target_us: target } : undefined;
    }
    case "player_seek_applied": {
      const seekId = value["seek_id"];
      const target = value["target_us"];
      return hasExactKeys(value, ["kind", "seek_id", "target_us"]) && isU32(seekId) && isMediaTime(target)
        ? { kind: "player_seek_applied", seek_id: seekId, target_us: target }
        : undefined;
    }
    case "page_playback": {
      const request = parsePlaybackRequest(value["request"]);
      return hasExactKeys(value, ["kind", "request"]) && request !== undefined ? { kind: "page_playback", request } : undefined;
    }
    case "file_source": {
      const url = value["url"];
      return hasExactKeys(value, ["kind", "url"]) && isFileSourceUrl(url) ? { kind: "file_source", url } : undefined;
    }
    case "shadow_alive":
      return hasExactKeys(value, ["kind"]) ? { kind: "shadow_alive" } : undefined;
    case "handoff_ended": {
      const cause = value["cause"];
      return hasExactKeys(value, ["kind", "cause"]) && isOneOf(cause, HANDOFF_END_CAUSES) ? { kind: "handoff_ended", cause } : undefined;
    }
    default:
      return undefined;
  }
}

// ------------------------------------------------------------------ кандидаты видео

/** Метка документа по правилу выше. */
export function isDocumentTag(value: unknown): value is string {
  return typeof value === "string" && DOCUMENT_TAG_PATTERN.test(value);
}

function parseCandidateMedia(value: unknown): CandidateMedia | undefined {
  if (!isPlainRecord(value)) return undefined;
  switch (value["kind"]) {
    case "media_source":
      return hasExactKeys(value, ["kind"]) ? { kind: "media_source" } : undefined;
    case "file":
      return hasExactKeys(value, ["kind"]) ? { kind: "file" } : undefined;
    case "none":
      return hasExactKeys(value, ["kind"]) ? { kind: "none" } : undefined;
    case "unsupported": {
      const form = value["form"];
      return hasExactKeys(value, ["kind", "form"]) && isOneOf(form, UNSUPPORTED_MEDIA_FORMS) ? { kind: "unsupported", form } : undefined;
    }
    default:
      return undefined;
  }
}

function parseVideoCandidate(value: unknown): VideoCandidate | undefined {
  const keys = ["document", "element", "media", "playing", "visible_area_px"];
  if (!isPlainRecord(value) || !hasExactKeys(value, keys)) return undefined;
  const document = value["document"];
  const element = value["element"];
  const media = parseCandidateMedia(value["media"]);
  const playing = value["playing"];
  const visibleArea = value["visible_area_px"];
  const validArea = typeof visibleArea === "number" && Number.isFinite(visibleArea) && visibleArea >= 0;
  if (!isDocumentTag(document) || !isMediaTime(element)) return undefined;
  if (media === undefined || typeof playing !== "boolean" || !validArea) return undefined;
  return { document, element, media, playing, visible_area_px: visibleArea };
}

/** Разбор списка кандидатов (не длиннее предела документа); один кривой элемент — отказ всего списка. */
export function parseVideoCandidates(value: unknown): VideoCandidate[] | undefined {
  if (!Array.isArray(value) || value.length > MAX_VIDEO_CANDIDATES_PER_DOCUMENT) return undefined;
  const candidates: VideoCandidate[] = [];
  for (const item of value) {
    const candidate = parseVideoCandidate(item);
    if (candidate === undefined) return undefined;
    candidates.push(candidate);
  }
  return candidates;
}

/** Разбор ответа MAIN-мира `candidates`. */
export function parseCandidatesReply(value: unknown): CandidatesReply | undefined {
  if (!isPlainRecord(value) || value["kind"] !== "candidates") return undefined;
  if (!hasExactKeys(value, ["kind", "request_id", "candidates"])) return undefined;
  const requestId = value["request_id"];
  const candidates = parseVideoCandidates(value["candidates"]);
  return isU32(requestId) && candidates !== undefined ? { kind: "candidates", request_id: requestId, candidates } : undefined;
}
