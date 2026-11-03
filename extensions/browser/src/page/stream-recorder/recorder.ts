// Ядро записи потока страницы: события формата `*.fprec`, склад байтов, идентификаторы объектов.
//
// Владеет: списком событий, складом байтов, нумерацией объектов (`ms-N`, `sb-N`, `el-N` — сквозная в
// пределах одного контекста) и связью «ключ учёта моста → идентификаторы записи». НЕ знает, откуда
// берутся события (обёртки MSE, элемента, мост): обёртки зовут его `record*`-методами.
//
// Один экземпляр — один контекст (`main` или воркер). Событие воркера пишется с «сырым» временем
// воркера и ctx `worker`; главный поток при приёме пачки (`absorbWorkerBatch`) пересчитывает время
// и подставляет `worker-<N>`.

import { ByteStore } from "./byte-store.ts";
import {
  DEFAULT_STORE_CEILING_BYTES,
  MAX_RECORDED_EVENTS,
  fnv1a32Hex,
  type JsonValue,
  type RecordedEvent,
} from "./format.ts";

/** Поля операции (всё, кроме `t_ms`, `ctx`, `op`). */
export type EventFields = { readonly [field: string]: JsonValue };

/** Параметры записи одного контекста. */
export interface RecorderOptions {
  /** Имя контекста в событиях: `main` или (для воркера до подстановки номера) `worker`. */
  readonly ctx: string;
  /** Часы контекста (`performance.now()`), миллисекунды. */
  readonly now: () => number;
  /** Показание часов в момент старта записи: `t_ms` считается от него. */
  readonly startMs: number;
  readonly storeCeilingBytes?: number;
  readonly maxEvents?: number;
}

/** Идентификатор объекта и признак «выдан впервые». */
export interface IssuedId {
  readonly id: string;
  readonly isNew: boolean;
}

/** Идентификаторы записи для куска, отданного мостом (ключ учёта + индекс SourceBuffer). */
export interface RecordedBuffer {
  readonly ms: string;
  readonly sb: string;
}

/** Пачка событий воркера для главного потока. */
export interface WorkerBatch {
  /** `performance.timeOrigin` воркера. */
  readonly timeOrigin: number;
  readonly events: readonly RecordedEvent[];
  /** Склад пачки одним буфером (смещения `append` в событиях — внутри него); `null` — байтов нет. */
  readonly bytes: Uint8Array | null;
  /** Воркер упёрся в свой потолок: часть байтов не записана. */
  readonly truncated: boolean;
}

/** Сводка для отладочного глобала. */
export interface RecordingSummary {
  readonly recording: true;
  readonly events: number;
  readonly appended_bytes: number;
  readonly store_bytes: number;
  readonly truncated: boolean;
  readonly sent_chunks: number;
  readonly last_event_t_ms: number;
}

export class StreamRecorder {
  readonly #options: RecorderOptions;
  readonly #store: ByteStore;
  readonly #maxEvents: number;
  #events: RecordedEvent[] = [];
  #eventsTruncated = false;
  #appendedBytes = 0;
  #sentChunks = 0;
  readonly #mediaSourceIds = new WeakMap<object, string>();
  readonly #sourceBufferIds = new WeakMap<object, RecordedBuffer>();
  readonly #elementIds = new WeakMap<object, string>();
  readonly #bufferByTrackerKey = new Map<string, RecordedBuffer>();
  readonly #mediaSourceByObjectUrl = new Map<string, string>();
  #lastMediaSource = 0;
  #lastSourceBuffer = 0;
  #lastElement = 0;

  constructor(options: RecorderOptions) {
    this.#options = options;
    this.#store = new ByteStore(options.storeCeilingBytes ?? DEFAULT_STORE_CEILING_BYTES);
    this.#maxEvents = options.maxEvents ?? MAX_RECORDED_EVENTS;
  }

  // ------------------------------------------------------------ идентификаторы

  mediaSourceIdOf(mediaSource: object): IssuedId {
    const known = this.#mediaSourceIds.get(mediaSource);
    if (known !== undefined) return { id: known, isNew: false };
    this.#lastMediaSource += 1;
    const id = `ms-${this.#lastMediaSource}`;
    this.#mediaSourceIds.set(mediaSource, id);
    return { id, isNew: true };
  }

  /** Идентификатор MediaSource, только если он уже выдан. */
  knownMediaSourceId(mediaSource: object): string | undefined {
    return this.#mediaSourceIds.get(mediaSource);
  }

  /** Идентификаторы SourceBuffer; первый раз выдаются, если известен его MediaSource. */
  sourceBufferIdOf(sourceBuffer: object, owner: string | null): { readonly buffer: RecordedBuffer; readonly isNew: boolean } | undefined {
    const known = this.#sourceBufferIds.get(sourceBuffer);
    if (known !== undefined) return { buffer: known, isNew: false };
    if (owner === null) return undefined;
    this.#lastSourceBuffer += 1;
    const buffer = { ms: owner, sb: `sb-${this.#lastSourceBuffer}` };
    this.#sourceBufferIds.set(sourceBuffer, buffer);
    return { buffer, isNew: true };
  }

  knownSourceBuffer(sourceBuffer: object): RecordedBuffer | undefined {
    return this.#sourceBufferIds.get(sourceBuffer);
  }

  elementIdOf(element: object): string {
    const known = this.#elementIds.get(element);
    if (known !== undefined) return known;
    this.#lastElement += 1;
    const id = `el-${this.#lastElement}`;
    this.#elementIds.set(element, id);
    return id;
  }

  /** Связать ключ учёта моста («ms-3», индекс 0) с идентификаторами записи — по нему событие `sent` находит `sb`. */
  bindTrackerBuffer(trackerKey: string, index: number, buffer: RecordedBuffer): void {
    this.#bufferByTrackerKey.set(`${trackerKey}#${index}`, buffer);
  }

  rememberObjectUrl(url: string, mediaSourceId: string): void {
    this.#mediaSourceByObjectUrl.set(url, mediaSourceId);
  }

  mediaSourceOfObjectUrl(url: string): string | undefined {
    return this.#mediaSourceByObjectUrl.get(url);
  }

  // ------------------------------------------------------------ запись событий

  /** Записать событие (поля операции — по формату). Переполнение числа событий выставляет `truncated`. */
  record(op: string, fields: EventFields = {}): void {
    if (this.#events.length >= this.#maxEvents) {
      this.#eventsTruncated = true;
      return;
    }
    // Порядок ключей читаемый: t_ms, ctx, op, затем поля операции.
    this.#events.push({ t_ms: this.#options.now() - this.#options.startMs, ctx: this.#options.ctx, op, ...fields });
  }

  /**
   * `append`: байты копируются на склад сейчас. Склад полон — вместо `append` пишется
   * `append_truncated` (тот же `ms/sb/length/hash`, но без `offset`): читатель его пропускает, а
   * `truncated` в манифесте предупреждает, что запись обрезана.
   */
  recordAppend(buffer: RecordedBuffer, bytes: Uint8Array): void {
    const hash = fnv1a32Hex(bytes);
    const stored = this.#store.append(bytes);
    if (stored.kind === "dropped") {
      this.record("append_truncated", { ms: buffer.ms, sb: buffer.sb, length: bytes.length, hash });
      return;
    }
    this.#appendedBytes += bytes.length;
    this.record("append", { ms: buffer.ms, sb: buffer.sb, offset: stored.offset, length: bytes.length, hash });
  }

  /** `sent`: кусок, отданный фону. Не знаем такой буфер — событие не пишется (нечем его назвать). */
  recordSent(trackerKey: string, index: number, bytes: Uint8Array, origin: "replay" | "live"): boolean {
    const buffer = this.#bufferByTrackerKey.get(`${trackerKey}#${index}`);
    if (buffer === undefined) return false;
    this.#sentChunks += 1;
    this.record("sent", { ms: buffer.ms, sb: buffer.sb, length: bytes.length, hash: fnv1a32Hex(bytes), replay: origin === "replay" });
    return true;
  }

  // ------------------------------------------------------------ воркер → главный поток

  /** Воркер: отдать накопленное (события и байты одним буфером) и начать заново. */
  drainBatch(timeOrigin: number): WorkerBatch {
    const events = this.#events;
    this.#events = [];
    const stored = this.#store.drain();
    let bytes: Uint8Array | null = null;
    if (stored.sizeBytes > 0) {
      bytes = new Uint8Array(stored.sizeBytes);
      let position = 0;
      for (const chunk of stored.chunks) {
        bytes.set(chunk, position);
        position += chunk.length;
      }
    }
    return { timeOrigin, events, bytes, truncated: this.#store.truncated || this.#eventsTruncated };
  }

  /**
   * Главный поток: принять пачку воркера. Время пересчитывается на часы главного потока
   * (`performance.timeOrigin` обоих), смещения `append` — на общий склад.
   */
  absorbWorkerBatch(workerOrdinal: number, batch: WorkerBatch, mainTimeOrigin: number): void {
    let base: number | null = null;
    if (batch.bytes !== null) {
      const stored = this.#store.append(batch.bytes);
      if (stored.kind === "stored") base = stored.offset;
    }
    // Часы воркера → часы главного потока → от старта записи.
    const shiftMs = batch.timeOrigin - mainTimeOrigin - this.#options.startMs;
    for (const event of batch.events) {
      if (this.#events.length >= this.#maxEvents) {
        this.#eventsTruncated = true;
        return;
      }
      const absorbed: RecordedEvent = { ...event, ctx: `worker-${workerOrdinal}`, t_ms: event.t_ms + shiftMs };
      if (event.op === "append") {
        if (base === null) {
          // Общий склад полон: байтов нет — событие честно превращается в `append_truncated`.
          absorbed.op = "append_truncated";
          delete absorbed["offset"];
        } else {
          absorbed["offset"] = (event["offset"] as number) + base;
          this.#appendedBytes += event["length"] as number;
        }
      } else if (event.op === "sent") {
        this.#sentChunks += 1;
      }
      this.#events.push(absorbed);
    }
    if (batch.truncated) this.#eventsTruncated = true;
  }

  // ------------------------------------------------------------ итог

  get truncated(): boolean {
    return this.#eventsTruncated || this.#store.truncated;
  }

  /** События по возрастанию `t_ms`; при равенстве — в порядке записи. */
  sortedEvents(): RecordedEvent[] {
    return this.#events
      .map((event, order) => ({ event, order }))
      .sort((left, right) => left.event.t_ms - right.event.t_ms || left.order - right.order)
      .map((entry) => entry.event);
  }

  get storeChunks(): readonly Uint8Array[] {
    return this.#store.chunks;
  }

  /** Сколько пока выставлено событий определённой операции (для отладочной сводки и тестов). */
  get eventCount(): number {
    return this.#events.length;
  }

  get pendingStoreBytes(): number {
    return this.#store.sizeBytes;
  }

  summary(): RecordingSummary {
    const last = this.#events[this.#events.length - 1];
    return {
      recording: true,
      events: this.#events.length,
      appended_bytes: this.#appendedBytes,
      store_bytes: this.#store.sizeBytes,
      truncated: this.truncated,
      sent_chunks: this.#sentChunks,
      last_event_t_ms: last === undefined ? 0 : last.t_ms,
    };
  }
}
