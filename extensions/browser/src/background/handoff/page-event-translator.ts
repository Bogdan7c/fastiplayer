// Перевод событий страницы (локальные ключи MAIN-мира) в сообщения и рамки протокола сеанса.
//
// Один переводчик — на один документ (одно подключение порта участка 2). Он:
// - подставляет номер поколения вместо локального ключа MediaSource (stream-numbering.ts);
//   первое упоминание ключа открывает поколение — сначала уходит `generation_started{N}`;
// - помнит, какие SourceBuffer объявлены (`source_buffer_added`): событие или байты для
//   необъявленного SourceBuffer — нарушение порядка страницы. Плеер закрыл бы такой сеанс 4003,
//   поэтому мы не шлём заведомо битый поток, а сообщаем проблему владельцу сеанса;
// - режет байты на рамки ≤ 1 МиБ (`splitAppendPayload`, без копий) и нумерует их по
//   (поколение, SourceBuffer) с 0 подряд;
// - перемотки и play/pause страницы переводит в `seek_started` / `page_playback`.
// Порядок выдачи — ровно порядок событий страницы.

import type { PageEvent } from "../../shared/handoff-contract.ts";
import {
  splitAppendPayload,
  type DataFrameHeader,
  type PageSessionMessage,
  type StreamGeneration,
} from "../../protocol/index.ts";
import type { DocumentNumbering, StreamNumbering } from "./stream-numbering.ts";

/** Что уходит в сеанс: текстовое сообщение или двоичная рамка (байты SourceBuffer — вид 1, байты файла — вид 2). */
export type OutgoingItem =
  | { readonly kind: "message"; readonly message: PageSessionMessage }
  | { readonly kind: "frame"; readonly header: DataFrameHeader; readonly payload: Uint8Array };

/** Почему поток страницы нельзя честно переслать плееру. */
export type PageStreamProblem =
  /** Событие или байты SourceBuffer, о котором не было `source_buffer_added`. */
  | "source_buffer_not_announced"
  /** Повторный `source_buffer_added` того же SourceBuffer поколения. */
  | "source_buffer_announced_twice"
  /** Номера поколений u32 исчерпаны. */
  | "generation_numbers_exhausted";

export type TranslationResult =
  | { readonly ok: true; readonly items: readonly OutgoingItem[] }
  | { readonly ok: false; readonly problem: PageStreamProblem };

/**
 * События потока (начало и конец передачи разбирает владелец сеанса, не переводчик). Файл
 * (`file_source`) тоже разбирает владелец: ему, кроме сообщений, нужен номер поколения и адрес
 * для загрузчика диапазонов (`translateFileSource`).
 */
export type StreamPageEvent = Exclude<
  PageEvent,
  { kind: "handoff_began" } | { kind: "handoff_ended" } | { kind: "shadow_alive" } | { kind: "file_source" }
>;

/** Перевод `file_source`: сообщения поколения-файла и его номер (для загрузчика диапазонов). */
export type FileSourceTranslation =
  | { readonly ok: true; readonly generation: StreamGeneration; readonly items: readonly OutgoingItem[] }
  | { readonly ok: false; readonly problem: PageStreamProblem };

export interface DocumentTranslator {
  translate(event: StreamPageEvent): TranslationResult;
  /**
   * Страница передаёт файл `<video src>` (сессия 09b): новое поколение, сразу
   * `generation_started{N}` и `file_source_added{N}` — байты плеер запросит сам (`fetch_range`).
   */
  translateFileSource(): FileSourceTranslation;
}

const success = (items: readonly OutgoingItem[]): TranslationResult => ({ ok: true, items });
const failure = (problem: PageStreamProblem): TranslationResult => ({ ok: false, problem });
const message = (pageMessage: PageSessionMessage): OutgoingItem => ({ kind: "message", message: pageMessage });

/** Переводчик для нового документа сеанса. */
export function createDocumentTranslator(numbering: StreamNumbering): DocumentTranslator {
  const document: DocumentNumbering = numbering.openDocument();
  // Объявленные SourceBuffer: «поколение:индекс».
  const announcedSourceBuffers = new Set<string>();

  /** Поколение ключа; при первом взгляде — с сообщением `generation_started` впереди. */
  function resolveGeneration(mediaSource: string): { generation: StreamGeneration; prefix: OutgoingItem[] } | undefined {
    const lookup = document.generationOf(mediaSource);
    if (lookup.kind === "exhausted") return undefined;
    const prefix = lookup.kind === "started" ? [message({ type: "generation_started", generation: lookup.generation })] : [];
    return { generation: lookup.generation, prefix };
  }

  function translateMse(event: Extract<StreamPageEvent, { kind: "mse" }>["event"]): TranslationResult {
    const resolved = resolveGeneration(event.media_source);
    if (resolved === undefined) return failure("generation_numbers_exhausted");
    const { generation, prefix } = resolved;
    switch (event.kind) {
      case "source_buffer_added": {
        const key = `${generation}:${event.source_buffer}`;
        if (announcedSourceBuffers.has(key)) return failure("source_buffer_announced_twice");
        announcedSourceBuffers.add(key);
        return success([
          ...prefix,
          message({ type: "source_buffer_added", generation, source_buffer: event.source_buffer, mime_type: event.mime_type }),
        ]);
      }
      case "duration_changed":
        return success([...prefix, message({ type: "duration_changed", generation, duration: event.duration })]);
      case "end_of_stream":
        return success([...prefix, message({ type: "end_of_stream", generation })]);
      default:
        break;
    }
    // Остальные события относятся к конкретному SourceBuffer — он обязан быть объявлен.
    if (!announcedSourceBuffers.has(`${generation}:${event.source_buffer}`)) return failure("source_buffer_not_announced");
    const address = { generation, source_buffer: event.source_buffer };
    switch (event.kind) {
      case "source_buffer_type_changed":
        return success([message({ type: "source_buffer_type_changed", ...address, mime_type: event.mime_type })]);
      case "timestamp_offset_changed":
        return success([message({ type: "timestamp_offset_changed", ...address, offset_us: event.offset_us })]);
      case "source_buffer_removed":
        return success([message({ type: "source_buffer_removed", ...address, start_us: event.start_us, end: event.end })]);
      case "source_buffer_aborted":
        return success([message({ type: "source_buffer_aborted", ...address })]);
    }
  }

  function translateBytes(event: Extract<StreamPageEvent, { kind: "bytes" }>): TranslationResult {
    const lookup = document.generationOf(event.media_source);
    if (lookup.kind === "exhausted") return failure("generation_numbers_exhausted");
    // Байты без объявленного SourceBuffer (в том числе нового MediaSource) — нарушение порядка.
    if (lookup.kind === "started" || !announcedSourceBuffers.has(`${lookup.generation}:${event.source_buffer}`)) {
      return failure("source_buffer_not_announced");
    }
    const frames = splitAppendPayload(event.bytes).map(
      (payload): OutgoingItem => ({
        kind: "frame",
        header: {
          kind: "source_buffer_bytes",
          generation: lookup.generation,
          source_buffer: event.source_buffer,
          sequence: numbering.nextFrameSequence(lookup.generation, event.source_buffer),
        },
        payload,
      }),
    );
    return success(frames);
  }

  return {
    translateFileSource() {
      const lookup = document.startFileGeneration();
      if (lookup.kind === "exhausted") return { ok: false, problem: "generation_numbers_exhausted" };
      const { generation } = lookup;
      return {
        ok: true,
        generation,
        items: [message({ type: "generation_started", generation }), message({ type: "file_source_added", generation })],
      };
    },
    translate(event) {
      switch (event.kind) {
        case "mse":
          return translateMse(event.event);
        case "bytes":
          return translateBytes(event);
        case "page_seek":
          return success([message({ type: "seek_started", target_us: event.target_us, cause: { kind: "page_seek" } })]);
        case "player_seek_applied":
          return success([
            message({ type: "seek_started", target_us: event.target_us, cause: { kind: "player_seek", seek_id: event.seek_id } }),
          ]);
        case "page_playback":
          return success([message({ type: "page_playback", request: event.request })]);
      }
    },
  };
}
