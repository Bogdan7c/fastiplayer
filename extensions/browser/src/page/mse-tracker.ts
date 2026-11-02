// Учёт MediaSource и SourceBuffer документа: локальные ключи, индексы, перевод вызовов MSE в
// события договора (`LocalMseEvent`) и байты. Хук (`mse-hook.ts`) зовёт этот модуль ТОЛЬКО после
// успешного вызова оригинала.
//
// Инварианты:
//  - локальный ключ MediaSource — «ms-N» по счётчику документа; выдаётся при первом
//    `addSourceBuffer` (раньше MediaSource не к чему отслеживать: SourceBuffer у него нет);
//  - индекс SourceBuffer — 0..7 по порядку `addSourceBuffer` в своём MediaSource; SourceBuffer
//    девятый и далее или с MIME, который протокол не примет, не отслеживается;
//  - `duration`, выставленный до первого SourceBuffer, запоминается и выдаётся сразу после него;
//  - `endOfStream("network" | "decode")` — сбой, не конец потока: не пересказываем;
//  - каждое событие и каждый кусок попадает в журнал MediaSource («память без копий») и
//    слушателю (живой поток во время передачи) — в одном и том же порядке.

import type { LocalMediaSourceKey, LocalMseEvent } from "../shared/handoff-contract.ts";
import { MAX_SOURCE_BUFFERS_PER_GENERATION } from "../protocol/limits.ts";
import type { MediaDuration } from "../protocol/time.ts";
import type { DocumentMemoryBudget } from "./journal/memory-budget.ts";
import type { MediaJournal, ReplayItem } from "./journal/media-journal.ts";
import {
  asWebIdlDouble,
  asWebIdlString,
  durationOf,
  isProtocolMimeType,
  offsetMicros,
  positionMicros,
  removalEndOf,
  sameDuration,
} from "./mse-values.ts";

/** Что хук сообщает об успешных вызовах MSE страницы. */
export interface MseObserver {
  sourceBufferAdded(mediaSource: object, sourceBuffer: object, mimeType: unknown): void;
  bytesAppended(sourceBuffer: object, bytes: Uint8Array): void;
  sourceBufferTypeChanged(sourceBuffer: object, mimeType: unknown): void;
  timestampOffsetChanged(sourceBuffer: object, offsetSeconds: number): void;
  rangeRemoved(sourceBuffer: object, startSeconds: unknown, endSeconds: unknown): void;
  sourceBufferAborted(sourceBuffer: object): void;
  durationChanged(mediaSource: object, durationSeconds: number): void;
  /** `durationAfterSeconds` — `duration` сразу после успешного `endOfStream()` (браузер мог её сменить). */
  streamEnded(mediaSource: object, errorArgument: unknown, durationAfterSeconds: number | null): void;
}

/** Отслеживаемый MediaSource документа. */
export interface MediaSourceRecord {
  readonly key: LocalMediaSourceKey;
  readonly journal: MediaJournal;
  /**
   * Blob-адрес MediaSource на момент его первого `addSourceBuffer` — по нему решается, к какому
   * элементу он подключён («src элемента = этот адрес»). `null` — адрес не создавался через
   * `URL.createObjectURL` (например, `srcObject`, 09b).
   */
  readonly objectUrl: string | null;
  /** Слабая ссылка на сам MediaSource (чтение `readyState`). */
  readonly mediaSource: WeakRef<object>;
}

/** Кто слушает поток событий (контроллер передачи). */
export interface MseListener {
  /** У MediaSource появился первый SourceBuffer — до его `source_buffer_added`. */
  onMediaSourceStarted(record: MediaSourceRecord): void;
  /** Очередное событие или кусок байтов (вид без копии) — в порядке вызовов страницы. */
  onItem(record: MediaSourceRecord, item: ReplayItem): void;
}

/** Что трекеру нужно снаружи. */
export interface MseTrackerEnvironment {
  readonly budget: DocumentMemoryBudget;
  /** `readyState` MediaSource оригинальным геттером (`null` — прочитать не вышло). */
  readonly readyStateOf: (mediaSource: object) => string | null;
  /** Последний blob-адрес, созданный для этого MediaSource. */
  readonly objectUrlOf: (mediaSource: object) => string | null;
  /** Единичная отладочная запись о необычном (без спама). */
  readonly reportOddity: (what: string) => void;
}

interface SourceBufferRecord {
  readonly mediaSource: MediaSourceRecord;
  readonly index: number;
}

/** Префикс локального ключа MediaSource (договор: «ms-1», «ms-2», …). */
const MEDIA_SOURCE_KEY_PREFIX = "ms-";

export class MseTracker implements MseObserver {
  readonly #environment: MseTrackerEnvironment;
  readonly #mediaSources = new WeakMap<object, MediaSourceRecord>();
  readonly #sourceBufferCounts = new WeakMap<MediaSourceRecord, number>();
  readonly #sourceBuffers = new WeakMap<object, SourceBufferRecord>();
  /** Длительность, выставленная до первого SourceBuffer. */
  readonly #pendingDurations = new WeakMap<object, MediaDuration>();
  /** Последняя пересказанная длительность MediaSource (чтобы заметить смену внутри endOfStream). */
  readonly #lastDurations = new WeakMap<MediaSourceRecord, MediaDuration>();
  #lastKeyNumber = 0;
  #listener: MseListener | null = null;

  constructor(environment: MseTrackerEnvironment) {
    this.#environment = environment;
  }

  /** Подключить слушателя (контроллер передачи). Один на документ. */
  setListener(listener: MseListener): void {
    this.#listener = listener;
  }

  /** Запись отслеживаемого MediaSource (только если у него уже был SourceBuffer). */
  recordOf(mediaSource: object): MediaSourceRecord | null {
    return this.#mediaSources.get(mediaSource) ?? null;
  }

  /** Сколько SourceBuffer отслеживается у MediaSource. */
  sourceBufferCountOf(record: MediaSourceRecord): number {
    return this.#sourceBufferCounts.get(record) ?? 0;
  }

  sourceBufferAdded(mediaSource: object, sourceBuffer: object, mimeArgument: unknown): void {
    const mimeType = asWebIdlString(mimeArgument);
    const record = this.#mediaSources.get(mediaSource) ?? this.#startMediaSource(mediaSource);
    const index = this.sourceBufferCountOf(record);
    if (index >= MAX_SOURCE_BUFFERS_PER_GENERATION) {
      this.#environment.reportOddity("more than 8 source buffers in one media source");
      return;
    }
    if (!isProtocolMimeType(mimeType)) {
      this.#environment.reportOddity("source buffer mime type is not representable in the protocol");
      return;
    }
    this.#sourceBufferCounts.set(record, index + 1);
    this.#sourceBuffers.set(sourceBuffer, { mediaSource: record, index });
    this.#emitEvent(record, { kind: "source_buffer_added", media_source: record.key, source_buffer: index, mime_type: mimeType });
    const pendingDuration = this.#pendingDurations.get(mediaSource);
    if (index === 0 && pendingDuration !== undefined) {
      this.#pendingDurations.delete(mediaSource);
      this.#emitEvent(record, { kind: "duration_changed", media_source: record.key, duration: pendingDuration });
    }
  }

  bytesAppended(sourceBuffer: object, bytes: Uint8Array): void {
    const tracked = this.#sourceBuffers.get(sourceBuffer);
    // Пустой груз протокол не принимает — и смысла в нём нет.
    if (tracked === undefined || bytes.length === 0) return;
    const record = tracked.mediaSource;
    record.journal.recordBytes(tracked.index, bytes);
    this.#environment.budget.enforce();
    this.#listener?.onItem(record, { kind: "bytes", sourceBuffer: tracked.index, view: bytes });
  }

  sourceBufferTypeChanged(sourceBuffer: object, mimeArgument: unknown): void {
    const tracked = this.#sourceBuffers.get(sourceBuffer);
    if (tracked === undefined) return;
    const mimeType = asWebIdlString(mimeArgument);
    if (!isProtocolMimeType(mimeType)) {
      this.#environment.reportOddity("changeType mime type is not representable in the protocol");
      return;
    }
    this.#emitEvent(tracked.mediaSource, {
      kind: "source_buffer_type_changed",
      media_source: tracked.mediaSource.key,
      source_buffer: tracked.index,
      mime_type: mimeType,
    });
  }

  timestampOffsetChanged(sourceBuffer: object, offsetSeconds: number): void {
    const tracked = this.#sourceBuffers.get(sourceBuffer);
    const offset = offsetMicros(offsetSeconds);
    if (tracked === undefined || offset === null) return;
    this.#emitEvent(tracked.mediaSource, {
      kind: "timestamp_offset_changed",
      media_source: tracked.mediaSource.key,
      source_buffer: tracked.index,
      offset_us: offset,
    });
  }

  rangeRemoved(sourceBuffer: object, startSeconds: unknown, endSeconds: unknown): void {
    const tracked = this.#sourceBuffers.get(sourceBuffer);
    if (tracked === undefined) return;
    const start = positionMicros(asWebIdlDouble(startSeconds));
    const end = removalEndOf(asWebIdlDouble(endSeconds));
    if (tracked === undefined || start === null || end === null) return;
    // Браузер пускает только end > start, но после округления до микросекунд промежуток может
    // схлопнуться — пустой промежуток протокол отвергает, а удалять в нём нечего.
    if (end.kind === "at" && end.time_us <= start) return;
    this.#emitEvent(tracked.mediaSource, {
      kind: "source_buffer_removed",
      media_source: tracked.mediaSource.key,
      source_buffer: tracked.index,
      start_us: start,
      end,
    });
  }

  sourceBufferAborted(sourceBuffer: object): void {
    const tracked = this.#sourceBuffers.get(sourceBuffer);
    if (tracked === undefined) return;
    this.#emitEvent(tracked.mediaSource, { kind: "source_buffer_aborted", media_source: tracked.mediaSource.key, source_buffer: tracked.index });
  }

  durationChanged(mediaSource: object, durationSeconds: number): void {
    const duration = durationOf(durationSeconds);
    const record = this.#mediaSources.get(mediaSource);
    if (record === undefined || this.sourceBufferCountOf(record) === 0) {
      // SourceBuffer ещё нет: выдать сразу после первого.
      this.#pendingDurations.set(mediaSource, duration);
      return;
    }
    this.#emitEvent(record, { kind: "duration_changed", media_source: record.key, duration });
  }

  streamEnded(mediaSource: object, errorArgument: unknown, durationAfterSeconds: number | null): void {
    // endOfStream("network" | "decode") — страница сообщает о сбое, а не о конце потока.
    if (errorArgument !== undefined) return;
    const record = this.#mediaSources.get(mediaSource);
    if (record === undefined || this.sourceBufferCountOf(record) === 0) return;
    // Без ошибки браузер сам ставит длительность = конец буфера (алгоритм «конец потока» MSE):
    // страница этого не делала, но плееру новая длительность нужна до конца потока.
    if (durationAfterSeconds !== null) {
      const durationAfter = durationOf(durationAfterSeconds);
      const known = this.#lastDurations.get(record);
      if (known === undefined || !sameDuration(known, durationAfter)) {
        this.#emitEvent(record, { kind: "duration_changed", media_source: record.key, duration: durationAfter });
      }
    }
    this.#emitEvent(record, { kind: "end_of_stream", media_source: record.key });
  }

  #startMediaSource(mediaSource: object): MediaSourceRecord {
    this.#lastKeyNumber += 1;
    const key = `${MEDIA_SOURCE_KEY_PREFIX}${this.#lastKeyNumber}`;
    const reference = new WeakRef(mediaSource);
    const journal = this.#environment.budget.createJournal(key, {
      isGone: () => {
        const alive = reference.deref();
        return alive === undefined || this.#environment.readyStateOf(alive) === "closed";
      },
    });
    const record: MediaSourceRecord = { key, journal, objectUrl: this.#environment.objectUrlOf(mediaSource), mediaSource: reference };
    this.#mediaSources.set(mediaSource, record);
    this.#listener?.onMediaSourceStarted(record);
    return record;
  }

  #emitEvent(record: MediaSourceRecord, event: LocalMseEvent): void {
    if (event.kind === "duration_changed") this.#lastDurations.set(record, event.duration);
    record.journal.recordEvent(event);
    this.#listener?.onItem(record, { kind: "mse", event });
  }
}
