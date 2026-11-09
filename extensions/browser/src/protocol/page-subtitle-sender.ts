// Отправитель документа субтитров страницы (протокол 1.5, сессия 13b1, решения 270-274).
//
// Договор (`session/page-subtitles.ts`): сначала заголовок `subtitle_track_offered` с точной
// длиной документа, затем байты документа рамками вида 3 - подряд с 0, ровно `byte_length`
// байт. Здесь только нарезка: документ режется на куски не больше груза рамки (1 МиБ) БЕЗ
// копий - куски остаются подмассивами того же буфера (как `splitAppendPayload`). Рамку из
// заголовка и куска собирает `encodeDataFrame` (там груз копируется ровно один раз).
// Отправлять ли субтитры вообще (общая версия сеанса не ниже 1.5) решает вызывающий:
// `versionIncludes(agreed, dataFrameIntroducedIn("page_subtitle_bytes"))`.

import type { PageSubtitleBytesHeader } from "./data-frame.ts";
import { type EncodeResult, reject, succeed } from "./errors.ts";
import { MAX_DATA_FRAME_PAYLOAD_BYTES, MAX_PAGE_SUBTITLE_DOCUMENT_BYTES } from "./limits.ts";
import { splitAppendPayload } from "./sender.ts";
import type { PageSessionMessage } from "./session/page.ts";
import type { PageSubtitleOffer } from "./session/page-subtitles.ts";

const MAX_U32 = 0xffff_ffff;

/** Поля заголовка, которые знает вызывающий: длину документа помощник считает сам. */
export type PageSubtitleOfferFields = Omit<PageSubtitleOffer, "byte_length">;

/** Один кусок документа: заголовок рамки вида 3 и груз - подмассив документа без копии. */
export interface PageSubtitleDocumentPiece {
  readonly header: PageSubtitleBytesHeader;
  readonly payload: Uint8Array;
}

/** Документ, готовый к отправке: сначала `offer`, затем `pieces` строго по порядку. */
export interface PageSubtitleDocumentSending {
  readonly offer: Extract<PageSessionMessage, { readonly type: "subtitle_track_offered" }>;
  readonly pieces: readonly PageSubtitleDocumentPiece[];
}

/**
 * Готовит документ дорожки к отправке: заголовок с `byte_length` = длине документа и куски по
 * `maxChunkBytes` (по умолчанию 1 МиБ - предел груза рамки) со смещениями подряд с 0.
 *
 * Отказ - `number_out_of_range`: пустой документ или длиннее 8 МиБ (`byte_length`), номер
 * дорожки не 1..2^32-1 (`track`). Неверный `maxChunkBytes` - ошибка вызывающего (`RangeError`,
 * как у `splitAppendPayload`).
 */
export function splitPageSubtitleDocument(
  fields: PageSubtitleOfferFields,
  document: Uint8Array | ArrayBuffer | ArrayBufferView,
  maxChunkBytes: number = MAX_DATA_FRAME_PAYLOAD_BYTES,
): EncodeResult<PageSubtitleDocumentSending> {
  if (!Number.isInteger(fields.track) || fields.track < 1 || fields.track > MAX_U32) {
    return reject("number_out_of_range", "track");
  }
  const chunks = splitAppendPayload(document, maxChunkBytes);
  const byteLength = chunks.reduce((total, chunk) => total + chunk.length, 0);
  if (byteLength === 0 || byteLength > MAX_PAGE_SUBTITLE_DOCUMENT_BYTES) {
    return reject("number_out_of_range", "byte_length");
  }
  const pieces: PageSubtitleDocumentPiece[] = [];
  let offset = 0;
  for (const payload of chunks) {
    pieces.push({ header: { kind: "page_subtitle_bytes", track: fields.track, offset }, payload });
    offset += payload.length;
  }
  return succeed({ offer: { type: "subtitle_track_offered", ...fields, byte_length: byteLength }, pieces });
}
