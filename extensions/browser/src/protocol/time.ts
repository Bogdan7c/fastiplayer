// Время, скорость и байтовые смещения протокола. Зеркало time.rs.
//
// Время - целые микросекунды (не дробные секунды): сравнение однозначно, байты
// фикстур совпадают на Rust и TypeScript. Скорость - тысячные доли (1000 = 1x).
// Все целые не больше 2^53 - 1, поэтому в TypeScript это обычный `number`.

import {
  MAX_JSON_SAFE_INTEGER,
  MAX_PLAYBACK_RATE_PERMILLE,
  MAX_RANGE_FETCH_BYTES,
  MIN_PLAYBACK_RATE_PERMILLE,
} from "./limits.ts";
import { safeInteger } from "./numbers.ts";
import { type Codec, taggedUnion } from "./schema.ts";

/** Позиция или длительность в микросекундах, 0..2^53-1. */
export type MediaTimeMicros = number;

/** Смещение меток времени (`timestampOffset`) в микросекундах, может быть отрицательным. */
export type MediaTimeOffsetMicros = number;

/** Скорость воспроизведения в тысячных долях, 63..16000 (1000 = обычная). */
export type PlaybackRatePermille = number;

/** Смещение или длина в байтах, 0..2^53-1. */
export type ByteCount = number;

/** Обычная скорость воспроизведения. */
export const NORMAL_PLAYBACK_RATE_PERMILLE: PlaybackRatePermille = 1000;

export const mediaTimeCodec: Codec<MediaTimeMicros> = safeInteger({ primitive: "u64", min: 0, max: MAX_JSON_SAFE_INTEGER });

export const mediaTimeOffsetCodec: Codec<MediaTimeOffsetMicros> = safeInteger({
  primitive: "i64",
  min: -MAX_JSON_SAFE_INTEGER,
  max: MAX_JSON_SAFE_INTEGER,
});

export const playbackRateCodec: Codec<PlaybackRatePermille> = safeInteger({
  primitive: "u32",
  min: MIN_PLAYBACK_RATE_PERMILLE,
  max: MAX_PLAYBACK_RATE_PERMILLE,
});

export const byteOffsetCodec: Codec<ByteCount> = safeInteger({ primitive: "u64", min: 0, max: MAX_JSON_SAFE_INTEGER });

/** Длина запроса байтов файла: 1..16 МиБ (ноль - бессмысленный запрос). */
export const rangeFetchLengthCodec: Codec<ByteCount> = safeInteger({ primitive: "u64", min: 1, max: MAX_RANGE_FETCH_BYTES });

/** Длина ролика, как её знает плеер страницы. */
export type MediaDuration =
  | { readonly kind: "unknown" }
  | { readonly kind: "finite"; readonly duration_us: MediaTimeMicros }
  | { readonly kind: "live" };

export const mediaDurationCodec: Codec<MediaDuration> = taggedUnion<MediaDuration, "kind">("kind", {
  unknown: {},
  finite: { duration_us: mediaTimeCodec },
  live: {},
});
