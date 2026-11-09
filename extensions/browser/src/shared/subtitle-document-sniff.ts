// Опознание документа субтитров по содержимому (сессия 13b2).
//
// Нужен в двух местах:
// - MAIN-мир: ответ `fetch`/XHR прошёл фильтр по адресу и типу, но оставить его можно, только если
//   внутри действительно субтитры (иначе на склад попадёт любой JSON или XML сайта);
// - фон: скачанный `<track src>` — подсказка формата для плеера.
// Плеер всё равно верит содержимому и разбирает документ сам (13b1): здесь только «похоже ли».
//
// Смотрим на первые байты как на ASCII: признаки всех форматов — латиница в начале документа,
// поэтому кодировка (UTF-8, cp1251 у `.srt`) на опознание не влияет. UTF-16 не опознаём:
// документ с таким началом считается «не субтитрами» (известное ограничение).

import type { SubtitleFormatHint } from "../protocol/session/page-subtitles.ts";

/** Сколько первых байт смотрим: заголовок WebVTT, корень XML и первая реплика SRT туда влезают. */
export const SUBTITLE_SNIFF_PREVIEW_BYTES = 4096;

/**
 * Итог опознания:
 * - `document` — целый документ субтитров этого формата;
 * - `hls_segment` — кусочек WebVTT плейлиста HLS (`X-TIMESTAMP-MAP` в заголовке): это не дорожка,
 *   его собирает hls.js в `TextTrack`, а оттуда документ придёт репликами;
 * - `not_subtitles` — что-то другое.
 */
export type SubtitleSniffVerdict =
  | { readonly kind: "document"; readonly format: Exclude<SubtitleFormatHint, "unknown"> }
  | { readonly kind: "hls_segment" }
  | { readonly kind: "not_subtitles" };

const NOT_SUBTITLES: SubtitleSniffVerdict = { kind: "not_subtitles" };

/** Первая реплика SRT: номер, затем время «чч:мм:сс,ммм --> …» (запятая или точка). */
const SRT_OPENING_PATTERN = /^\d{1,9}[ \t]*\r?\n[ \t]*\d{1,3}:\d{2}:\d{2}[,.]\d{1,3}[ \t]*-->/;

/** WEBVTT, за которым конец документа, пробел, табуляция или перевод строки (спецификация WebVTT). */
const WEBVTT_SIGNATURE_PATTERN = /^WEBVTT(?:$|[ \t\r\n])/;

/** Признак кусочка HLS в заголовке WebVTT (RFC 8216 §3.5). */
const HLS_TIMESTAMP_MAP = "X-TIMESTAMP-MAP";

/** Корень XML после пролога: имя элемента с необязательным префиксом пространства имён. */
const XML_ROOT_PATTERN = /^<(?:[A-Za-z_][\w.-]*:)?([A-Za-z_][\w.-]*)/;

/**
 * Начало документа как строка ASCII: байты старше 127 заменяются на `?` (на признаки не влияют).
 * BOM UTF-8 и ведущие пробельные символы отбрасываются.
 */
function asciiPreview(bytes: Uint8Array): string {
  let start = 0;
  if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) start = 3;
  const end = Math.min(bytes.length, start + SUBTITLE_SNIFF_PREVIEW_BYTES);
  let preview = "";
  for (let index = start; index < end; index += 1) {
    const byte = bytes[index] ?? 0;
    preview += byte < 0x80 ? String.fromCharCode(byte) : "?";
  }
  return preview.replace(/^[\s]+/, "");
}

/** Заголовок WebVTT — всё до первой пустой строки. */
function webVttHeaderOf(preview: string): string {
  const blankLine = preview.search(/\r?\n[ \t]*\r?\n/);
  return blankLine === -1 ? preview : preview.slice(0, blankLine);
}

/** Пропустить пролог XML: объявление, комментарии, инструкции обработки, DOCTYPE и пробелы. */
function skipXmlProlog(preview: string): string {
  let rest = preview;
  for (;;) {
    rest = rest.replace(/^\s+/, "");
    const prolog = /^(?:<\?[\s\S]*?\?>|<!--[\s\S]*?-->|<!DOCTYPE[^>]*>)/i.exec(rest);
    if (prolog === null) return rest;
    rest = rest.slice(prolog[0].length);
  }
}

function sniffXml(preview: string): SubtitleSniffVerdict {
  const root = XML_ROOT_PATTERN.exec(skipXmlProlog(preview));
  switch (root?.[1]) {
    case "tt":
      return { kind: "document", format: "ttml" };
    case "timedtext":
      return { kind: "document", format: "srv3" };
    default:
      return NOT_SUBTITLES;
  }
}

/** json3 YouTube: объект с `wireMagic` или массивом `events` в начале документа. */
function sniffJson(preview: string): SubtitleSniffVerdict {
  return /"(?:wireMagic|events)"\s*:/.test(preview) ? { kind: "document", format: "json3" } : NOT_SUBTITLES;
}

/** Опознать документ субтитров по первым байтам (см. `SubtitleSniffVerdict`). */
export function sniffSubtitleDocument(bytes: Uint8Array): SubtitleSniffVerdict {
  const preview = asciiPreview(bytes);
  if (WEBVTT_SIGNATURE_PATTERN.test(preview)) {
    return webVttHeaderOf(preview).includes(HLS_TIMESTAMP_MAP) ? { kind: "hls_segment" } : { kind: "document", format: "web_vtt" };
  }
  if (preview.startsWith("<")) return sniffXml(preview);
  if (preview.startsWith("{")) return sniffJson(preview);
  if (SRT_OPENING_PATTERN.test(preview)) return { kind: "document", format: "srt" };
  return NOT_SUBTITLES;
}
