// Ресурсы HLS по номеркам (протокол 1.1, решение 228). Зеркало resource.rs.
//
// Плеер ведёт HLS сам и просит каждый ресурс (плейлист, init, сегмент, ключ) командой
// `fetch_resource`. Фон качает его сетью браузера и отдаёт тело рамками вида 2
// (`range_bytes{request_id, offset}`), где `offset` - смещение ВНУТРИ тела ответа с 0,
// затем `resource_fetch_finished{request_id, outcome}`.

import { MAX_RESOURCE_FETCH_BYTES } from "./limits.ts";
import { safeInteger } from "./numbers.ts";
import { type Codec, taggedUnion } from "./schema.ts";
import { type ByteCount, byteOffsetCodec } from "./time.ts";

/** Наименьший код ответа HTTP (`100 Continue`). */
const MIN_HTTP_STATUS = 100;
/** Наибольший код ответа HTTP по RFC 9110 (класс 5xx). */
const MAX_HTTP_STATUS = 599;

/** Длина куска и потолок ответа: 1..128 МиБ (ноль - бессмысленный запрос). */
export const resourceFetchLengthCodec: Codec<ByteCount> = safeInteger({ primitive: "u64", min: 1, max: MAX_RESOURCE_FETCH_BYTES });

/** Сколько байт тела отдано: 0..128 МиБ (пустое тело бывает). */
export const resourceBodySizeCodec: Codec<ByteCount> = safeInteger({ primitive: "u64", min: 0, max: MAX_RESOURCE_FETCH_BYTES });

/** Код ответа HTTP, 100..599. */
export const httpStatusCodec: Codec<number> = safeInteger({ primitive: "u16", min: MIN_HTTP_STATUS, max: MAX_HTTP_STATUS });

/**
 * Какую часть ресурса просит плеер. Не необязательное поле, а метка: пропуск поля -
 * `missing_field`, `null` - `wrong_value_type`, как в Rust.
 */
export type ResourceByteRange =
  | { readonly kind: "whole" }
  | { readonly kind: "slice"; readonly offset: ByteCount; readonly length: ByteCount };

/** Почему ресурс не получен. */
export type ResourceFetchFailure =
  | { readonly kind: "network_error" }
  | { readonly kind: "http_error"; readonly status: number }
  | { readonly kind: "refused" }
  | { readonly kind: "too_large" };

/** Чем кончился запрос ресурса. */
export type ResourceFetchOutcome =
  | { readonly kind: "completed"; readonly size_bytes: ByteCount }
  | { readonly kind: "failed"; readonly failure: ResourceFetchFailure };

export const resourceByteRangeCodec: Codec<ResourceByteRange> = taggedUnion<ResourceByteRange, "kind">("kind", {
  whole: {},
  slice: { offset: byteOffsetCodec, length: resourceFetchLengthCodec },
});

const resourceFetchFailureCodec: Codec<ResourceFetchFailure> = taggedUnion<ResourceFetchFailure, "kind">("kind", {
  network_error: {},
  http_error: { status: httpStatusCodec },
  refused: {},
  too_large: {},
});

export const resourceFetchOutcomeCodec: Codec<ResourceFetchOutcome> = taggedUnion<ResourceFetchOutcome, "kind">("kind", {
  completed: { size_bytes: resourceBodySizeCodec },
  failed: { failure: resourceFetchFailureCodec },
});
