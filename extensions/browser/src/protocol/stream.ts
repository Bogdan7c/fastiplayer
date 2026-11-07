// Номера потока: поколение, SourceBuffer, номера запросов. Зеркало stream.rs.
//
// Поколение - новый `MediaSource` на элементе (с 1, растёт на 1). SourceBuffer -
// индекс в поколении (0..7). Перемотка номеров не меняет (она - событие `seek_started`).

import { MAX_SOURCE_BUFFERS_PER_GENERATION } from "./limits.ts";
import { plainInteger, safeInteger } from "./numbers.ts";
import type { Codec } from "./schema.ts";

/** Номер поколения потока, 1..2^32-1. */
export type StreamGeneration = number;

/** Индекс SourceBuffer внутри поколения, 0..7. */
export type SourceBufferIndex = number;

export const streamGenerationCodec: Codec<StreamGeneration> = safeInteger({ primitive: "u32", min: 1 });

export const sourceBufferIndexCodec: Codec<SourceBufferIndex> = safeInteger({
  primitive: "u16",
  min: 0,
  max: MAX_SOURCE_BUFFERS_PER_GENERATION - 1,
});

/** Номер перемотки по команде плеера (`seek_id`), u32. */
export const seekIdCodec: Codec<number> = plainInteger("u32");

/**
 * Номер запроса байтов (`fetch_range` и `fetch_resource`), u32. Пространство номеров
 * у двух запросов общее: плеер ведёт один счётчик на сеанс.
 */
export const rangeRequestIdCodec: Codec<number> = plainInteger("u32");

/** Номерок ресурса HLS (протокол 1.1), весь диапазон u32. */
export type ResourceHandle = number;

/** Номерок ресурса HLS: его выдал фон, переписывая плейлист. */
export const resourceHandleCodec: Codec<ResourceHandle> = plainInteger("u32");

/** Номер запроса на участке «вход», u32. */
export const controlRequestIdCodec: Codec<number> = plainInteger("u32");

/** Порядковый номер тика часов, u32. */
export const clockSequenceCodec: Codec<number> = plainInteger("u32");
