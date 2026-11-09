// Фильтр ответа сети ДО чтения тела (сессия 13b2): чистая функция «стоит ли читать этот ответ».
//
// Зачем так строго. `Response.clone()` при чтении только одной ветки копит всё тело в памяти, а XHR уже
// держит тело целиком. Поэтому читаем лишь то, что по адресу, статусу, типу и размеру похоже на субтитры, —
// и никогда не трогаем видео, аудио, картинки, шрифты, манифесты HLS/DASH и прочее двоичное. Любой JSON
// и XML сайта читать тоже нельзя (приватность и цена): их читаем только по адресу, который явно «про субтитры».
//
// Правило решения, по порядку:
//  1. ответ не успешный (не 2xx, 206 — это кусок, а не весь документ), непрозрачный (`opaque`) или пустой
//     по `Content-Length: 0` — не читаем;
//  2. `Content-Length` больше потолка документа — не читаем (неизвестная длина проходит: потолок
//     соблюдает читатель потока и отменяет его на превышении);
//  3. адрес манифеста (`.m3u8`, `.mpd`) или тип «точно не субтитры» (видео, аудио, картинка, шрифт,
//     манифесты HLS/DASH, архивы) — не читаем никогда;
//  4. тип «точно субтитры» (`text/vtt`, `application/x-subrip`, `application/ttml+xml`, …) — читаем;
//  5. двусмысленный текстовый тип (`text/plain`, `application/json`, `text/xml`, `application/xml`,
//     `application/octet-stream`, его отсутствие) — читаем только если АДРЕС явно субтитровый:
//     расширение `.vtt/.webvtt/.srt/.ttml/.dfxp`, путь с `timedtext`/`caption`/`subtitle`, или
//     `fmt=json3|srv3|vtt|ttml`;
//  6. всё остальное (`text/html`, `text/css`, скрипты, PDF, …) — не читаем.
//
// Окончательно «это субтитры» решает проверка содержимого (`sniffSubtitleDocument`), здесь только допуск.

import { parseAddressParts, queryValueOf, type AddressParts } from "../../adapters/capabilities/subtitle-naming/address-parts.ts";
import { MAX_PAGE_SUBTITLE_DOCUMENT_BYTES } from "../../protocol/limits.ts";

/** Потолок размера документа: больше протокол не пропустит, поэтому и читать смысла нет. */
export const MAX_SUBTITLE_RESPONSE_BYTES = MAX_PAGE_SUBTITLE_DOCUMENT_BYTES;

/** Что известно об ответе до чтения тела. Все значения — уже прочитанные оригинальными геттерами. */
export interface SubtitleResponseCandidate {
  /** Итоговый адрес ответа (`Response.url`, `XMLHttpRequest.responseURL`). */
  readonly url: string;
  readonly status: number;
  /** `Response.type` (`basic`, `cors`, `opaque`, …); у XHR — `null` (непрозрачных XHR не бывает). */
  readonly responseType: string | null;
  /** Заголовок `Content-Type` как есть; `null` — нет или не виден (CORS). */
  readonly contentType: string | null;
  /** Заголовок `Content-Length` как есть; `null` — нет или не виден. */
  readonly contentLength: string | null;
}

/** Почему ответ не читается (для тестов и диагностики). */
export type SubtitleResponseSkipReason =
  | "bad_address"
  | "bad_status"
  | "opaque_response"
  | "empty_body"
  | "too_large"
  | "never_readable"
  | "ambiguous_type_and_plain_address"
  | "foreign_type";

export type SubtitleResponseVerdict = { readonly read: true } | { readonly read: false; readonly reason: SubtitleResponseSkipReason };

const READ: SubtitleResponseVerdict = { read: true };
const skip = (reason: SubtitleResponseSkipReason): SubtitleResponseVerdict => ({ read: false, reason });

/** Расширения файлов субтитров: по ним читаем даже двусмысленный тип. */
const SUBTITLE_EXTENSIONS: readonly string[] = [".vtt", ".webvtt", ".srt", ".ttml", ".dfxp"];

/** Расширения манифестов: HLS и DASH читать нельзя, под каким бы типом их ни отдали. */
const MANIFEST_EXTENSIONS: readonly string[] = [".m3u8", ".mpd"];

/** Подстроки пути, по которым адрес «про субтитры» (`/api/timedtext`, `/captions/en.json`, `/subtitles/1.xml`). */
const SUBTITLE_PATH_WORDS: readonly string[] = ["timedtext", "caption", "subtitle"];

/** Значения `fmt`, которыми сервисы просят документ субтитров (YouTube: json3, srv3, vtt, ttml). */
const SUBTITLE_FORMAT_VALUES: readonly string[] = ["json3", "srv3", "vtt", "ttml"];

/** Типы, которые сами говорят «субтитры». */
const SUBTITLE_MEDIA_TYPES: readonly string[] = [
  "text/vtt",
  "text/srt",
  "text/x-srt",
  "text/x-subrip",
  "application/x-subrip",
  "application/x-srt",
  "application/ttml+xml",
  "application/ttaf+xml",
  "application/x-ttml+xml",
];

/** Двусмысленные типы: текст/данные, внутри которых бывают субтитры, но чаще нет. */
const AMBIGUOUS_MEDIA_TYPES: readonly string[] = [
  "text/plain",
  "text/xml",
  "application/xml",
  "application/json",
  "text/json",
  "application/octet-stream",
  "binary/octet-stream",
];

/** Типы-манифесты и прочее «точно не субтитры», которое нельзя путать с двусмысленным `application/*`. */
const NEVER_READABLE_MEDIA_TYPES: readonly string[] = [
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/mpegurl",
  "audio/x-mpegurl",
  "application/dash+xml",
  "application/vnd.ms-sstr+xml",
  "application/zip",
  "application/pdf",
  "application/gzip",
];

/** Крупные семейства, которые не читаем никогда. */
const NEVER_READABLE_FAMILIES: readonly string[] = ["video/", "audio/", "image/", "font/"];

/** Тип без параметров (`charset`), строчными; пустая строка — типа нет. */
function mediaTypeOf(contentType: string | null): string {
  if (contentType === null) return "";
  const semicolon = contentType.indexOf(";");
  return (semicolon === -1 ? contentType : contentType.slice(0, semicolon)).trim().toLowerCase();
}

/** Последний сегмент пути строчными: по нему берём расширение. */
function lastPathSegment(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1).toLowerCase();
}

function hasExtension(parts: AddressParts, extensions: readonly string[]): boolean {
  const segment = lastPathSegment(parts.path);
  return extensions.some((extension) => segment.endsWith(extension));
}

/** Адрес с расширением файла субтитров. */
export function hasSubtitleFileExtension(parts: AddressParts): boolean {
  return hasExtension(parts, SUBTITLE_EXTENSIONS);
}

/**
 * Адрес явно «про субтитры»: путь с `timedtext`/`caption`/`subtitle` или `fmt` из списка. Одного
 * расширения `.json`/`.xml` мало: так читалась бы вся программная часть сайта.
 */
export function hasSubtitleApiMarkers(parts: AddressParts): boolean {
  const path = parts.path.toLowerCase();
  if (SUBTITLE_PATH_WORDS.some((word) => path.includes(word))) return true;
  const format = queryValueOf(parts, "fmt");
  return format !== null && SUBTITLE_FORMAT_VALUES.includes(format.toLowerCase());
}

/** Адрес разрешает читать двусмысленный ответ: файл субтитров или явный субтитровый адрес. */
export function addressLooksLikeSubtitles(address: string): boolean {
  const parts = parseAddressParts(address);
  return parts !== null && (hasSubtitleFileExtension(parts) || hasSubtitleApiMarkers(parts));
}

/** Длина из `Content-Length`; `null` — заголовка нет или он не число (тогда доверяем потолку читателя). */
function declaredLengthOf(contentLength: string | null): number | null {
  if (contentLength === null || !/^\s*\d{1,15}\s*$/.test(contentLength)) return null;
  return Number(contentLength);
}

/** Решение по ответу. Не бросает; входные значения — чужие данные, но уже строки/числа. */
export function decideSubtitleResponse(candidate: SubtitleResponseCandidate): SubtitleResponseVerdict {
  const parts = parseAddressParts(candidate.url);
  if (parts === null) return skip("bad_address");
  if (!Number.isInteger(candidate.status) || candidate.status < 200 || candidate.status > 299 || candidate.status === 206) return skip("bad_status");
  if (candidate.responseType === "opaque" || candidate.responseType === "opaqueredirect" || candidate.responseType === "error") return skip("opaque_response");
  const declaredLength = declaredLengthOf(candidate.contentLength);
  if (declaredLength === 0) return skip("empty_body");
  if (declaredLength !== null && declaredLength > MAX_SUBTITLE_RESPONSE_BYTES) return skip("too_large");

  const mediaType = mediaTypeOf(candidate.contentType);
  if (hasExtension(parts, MANIFEST_EXTENSIONS)) return skip("never_readable");
  if (NEVER_READABLE_MEDIA_TYPES.includes(mediaType) || NEVER_READABLE_FAMILIES.some((family) => mediaType.startsWith(family))) return skip("never_readable");

  if (SUBTITLE_MEDIA_TYPES.includes(mediaType)) return READ;
  const ambiguous = mediaType === "" || AMBIGUOUS_MEDIA_TYPES.includes(mediaType);
  if (!ambiguous) return skip("foreign_type");
  return hasSubtitleFileExtension(parts) || hasSubtitleApiMarkers(parts) ? READ : skip("ambiguous_type_and_plain_address");
}
