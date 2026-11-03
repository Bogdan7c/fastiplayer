// Реестр MediaSource выделенных воркеров документа (сессия 09b, пакет C) — для классификатора
// источника элемента и для контроллера передачи.
//
// Владеет:
//  - картой «handle в главном потоке → (воркер, номер сообщения)» (слабо: handle собирается со
//    своим элементом). Ключ MediaSource по ней получается ЛЕНИВО, в миг вопроса: донесение хука по
//    личному порту и сообщение сайта с handle идут разными путями, и любое может прийти первым;
//  - номерами потоков документа и открытыми потоками (обработчики контроллера, срок повтора).
//
// Правило «живой MediaSource воркера» то же, что в главном потоке: есть SourceBuffer и не закрыт.
// Handle, который мы не видели ни в одном сообщении обёрнутого воркера (обёртку не пустил CSP или
// Trusted Types, handle прошёл через чужой MessagePort, воркер вложенный), — честное «не умеем».

import type { LocalMediaSourceKey } from "../../shared/handoff-contract.ts";
import { MAX_PAGE_BYTES_EVENT_BYTES } from "../../shared/page-message-validation.ts";
import type { WorkerReport, WorkerStreamId, WorkerStreamItem } from "../../shared/worker-channel-contract.ts";
import { copyForTransfer, type BinaryIntrinsics } from "../binary-view.ts";
import type { OddityReporter } from "../prototype-wrap.ts";
import type { TimeoutScheduler } from "../shadow/file-return.ts";
import type { StreamMessage } from "../stream-sender.ts";
import { WorkerChannel, type WorkerChannelEnvironment, type WorkerChannelExtension, type WorkerChannelListener } from "./worker-channel.ts";

/**
 * Сколько ждём конца повтора журнала от воркера, мс. Повтор 64 МиБ — это копия и отправка
 * (~0,1 с); занятый тяжёлым кодом воркер отвечает за доли секунды. 3 с — с запасом, и намного
 * меньше допуска тишины тени у фона (`SHADOW_SILENCE_LIMIT_MS`, 10 с).
 */
export const WORKER_STREAM_REPLAY_TIMEOUT_MS = 3000;

/** MediaSource воркера, который можно передать. Снаружи смотрят только `key`. */
export interface WorkerMediaSourceRef {
  /** Ключ договора участка 1: «w<номер воркера>/ms-N». */
  readonly key: LocalMediaSourceKey;
  /** Свои поля реестра: канал воркера и ключ учёта внутри него. */
  readonly channel: WorkerChannel;
  readonly localKey: LocalMediaSourceKey;
}

/** Что стоит за handle в `srcObject`. */
export type WorkerHandleResolution =
  | { readonly kind: "live"; readonly mediaSource: WorkerMediaSourceRef }
  /** Наш воркер, но MediaSource ещё без SourceBuffer, уже закрыт или донесения ещё нет: пока нечего. */
  | { readonly kind: "not_live" }
  /** Не сопоставить с MediaSource обёрнутого воркера: честное «не поддерживается». */
  | { readonly kind: "untracked" };

/** Обработчики одного открытого потока (их даёт контроллер передачи). */
export interface WorkerStreamHandlers {
  /** Сообщение потока уже с ключом договора; `transfer` — буфер, переданный дальше без копии. */
  deliver(message: StreamMessage, transfer: readonly Transferable[]): void;
  /** Повтор журнала окончен (дальше — только живые элементы). */
  replayed(): void;
  /** Поток не открыть или он оборвался (воркер не ответил в срок, MediaSource ушёл, сбой). */
  unavailable(): void;
}

/** Кто узнаёт о новых живых MediaSource воркеров (контроллер передачи). */
export interface WorkerMediaSourceListener {
  onWorkerMediaSourceStarted(mediaSource: WorkerMediaSourceRef): void;
}

/** Окружение реестра. */
export interface WorkerMediaSourcesEnvironment {
  /** Объект — `MediaSourceHandle` этого окна (прототип снят на document_start). */
  readonly isMediaSourceHandle: (candidate: unknown) => boolean;
  readonly binary: BinaryIntrinsics;
  readonly scheduleTimeout: TimeoutScheduler;
  readonly reportOddity: OddityReporter;
  /** Расширение каналов воркеров (только отладочная сборка записи потока); без него — ничего. */
  readonly channelExtension?: WorkerChannelExtension;
}

interface OpenStream {
  readonly channel: WorkerChannel;
  readonly mediaSource: WorkerMediaSourceRef;
  readonly handlers: WorkerStreamHandlers;
  /** Снять срок повтора (после `stream_replayed` — пустышка). */
  cancelReplayTimeout: () => void;
}

/** Наибольший номер потока (u32 договора канала). */
const MAX_STREAM_ID = 0xffff_ffff;

export class WorkerMediaSources implements WorkerChannelListener {
  readonly #environment: WorkerMediaSourcesEnvironment;
  readonly #handleOrigins = new WeakMap<object, { readonly channel: WorkerChannel; readonly messageOrdinal: number }>();
  readonly #channels: WorkerChannel[] = [];
  readonly #streams = new Map<WorkerStreamId, OpenStream>();
  #lastStreamId = 0;
  #listener: WorkerMediaSourceListener | null = null;

  constructor(environment: WorkerMediaSourcesEnvironment) {
    this.#environment = environment;
  }

  /** Подключить слушателя (контроллер передачи). Один на документ. */
  setListener(listener: WorkerMediaSourceListener): void {
    this.#listener = listener;
  }

  /** Новый канал обёрнутого воркера (номер — следующий по документу). */
  createChannel(environment: Omit<WorkerChannelEnvironment, "listener" | "isOwnBytes">): WorkerChannel {
    const extension = this.#environment.channelExtension;
    const channel = new WorkerChannel(this.#channels.length + 1, {
      ...environment,
      listener: this,
      isOwnBytes: (value) => this.#isOwnBytes(value),
      ...(extension === undefined ? {} : { extension }),
    });
    this.#channels.push(channel);
    extension?.channelCreated(channel);
    return channel;
  }

  /** В сообщении `messageOrdinal` воркера пришёл (ровно один) handle. */
  noteHandleArrived(handle: object, channel: WorkerChannel, messageOrdinal: number): void {
    this.#handleOrigins.set(handle, { channel, messageOrdinal });
  }

  /** Что стоит за объектом из `srcObject`; `null` — это не `MediaSourceHandle`. */
  resolveHandle(candidate: unknown): WorkerHandleResolution | null {
    if (!this.#environment.isMediaSourceHandle(candidate) || typeof candidate !== "object" || candidate === null) return null;
    const origin = this.#handleOrigins.get(candidate);
    if (origin === undefined) return { kind: "untracked" };
    const handleOrigin = origin.channel.originOfMessage(origin.messageOrdinal);
    switch (handleOrigin.kind) {
      case "foreign":
        return { kind: "untracked" };
      case "unknown_yet":
        return { kind: "not_live" };
      case "own":
        if (!origin.channel.isLive(handleOrigin.localKey)) return { kind: "not_live" };
        return { kind: "live", mediaSource: this.#refOf(origin.channel, handleOrigin.localKey) };
    }
  }

  /**
   * Открыть поток MediaSource воркера: хук повторит журнал и дальше шлёт живые элементы. Не
   * бросает: сбой команды — сразу `unavailable`.
   */
  openStream(mediaSource: WorkerMediaSourceRef, handlers: WorkerStreamHandlers): void {
    if (this.#lastStreamId >= MAX_STREAM_ID) {
      this.#environment.reportOddity("worker stream numbers are exhausted");
      handlers.unavailable();
      return;
    }
    this.#lastStreamId += 1;
    const stream = this.#lastStreamId;
    const entry: OpenStream = { channel: mediaSource.channel, mediaSource, handlers, cancelReplayTimeout: () => {} };
    this.#streams.set(stream, entry);
    entry.cancelReplayTimeout = this.#environment.scheduleTimeout(() => {
      this.#environment.reportOddity("worker did not replay its journal in time");
      this.#dropStream(stream)?.handlers.unavailable();
    }, WORKER_STREAM_REPLAY_TIMEOUT_MS);
    try {
      mediaSource.channel.openStream(stream, mediaSource.localKey);
    } catch {
      this.#environment.reportOddity("worker stream could not be requested");
      this.#dropStream(stream)?.handlers.unavailable();
    }
  }

  /** Передача кончилась: забыть все потоки и попросить воркеры замолчать. */
  closeAllStreams(): void {
    const channels = new Set<WorkerChannel>();
    for (const [stream, entry] of this.#streams) {
      channels.add(entry.channel);
      this.#dropStream(stream);
    }
    for (const channel of channels) {
      try {
        channel.closeStreams();
      } catch {
        this.#environment.reportOddity("worker streams could not be closed");
      }
    }
  }

  // ------------------------------------------------------------ донесения каналов

  onMediaSourceStarted(channel: WorkerChannel, localKey: LocalMediaSourceKey): void {
    this.#listener?.onWorkerMediaSourceStarted(this.#refOf(channel, localKey));
  }

  onStreamReport(channel: WorkerChannel, report: Extract<WorkerReport, { kind: "stream_item" | "stream_replayed" | "stream_unavailable" }>): void {
    const entry = this.#streams.get(report.stream);
    // Чужой или уже закрытый поток (запоздавшие куски прошлой передачи) — молча мимо: так и задумано.
    if (entry === undefined || entry.channel !== channel) return;
    switch (report.kind) {
      case "stream_replayed":
        entry.cancelReplayTimeout();
        entry.cancelReplayTimeout = () => {};
        entry.handlers.replayed();
        return;
      case "stream_unavailable":
        this.#dropStream(report.stream);
        entry.handlers.unavailable();
        return;
      case "stream_item":
        this.#deliver(report.stream, entry, report.item);
        return;
    }
  }

  #deliver(stream: WorkerStreamId, entry: OpenStream, item: WorkerStreamItem): void {
    const itemKey = item.kind === "mse" ? item.event.media_source : item.media_source;
    if (itemKey !== entry.mediaSource.localKey) {
      // Элемент чужого MediaSource под номером этого потока: дальше поток не надёжен.
      this.#environment.reportOddity("worker stream item belongs to another media source");
      this.#dropStream(stream);
      entry.handlers.unavailable();
      return;
    }
    const key = entry.mediaSource.key;
    if (item.kind === "mse") {
      entry.handlers.deliver({ kind: "mse", event: { ...item.event, media_source: key } }, []);
      return;
    }
    const bytes = this.#transferableBytes(item.bytes);
    entry.handlers.deliver({ kind: "bytes", media_source: key, source_buffer: item.source_buffer, bytes }, [bytes.buffer as ArrayBuffer]);
  }

  #dropStream(stream: WorkerStreamId): OpenStream | undefined {
    const entry = this.#streams.get(stream);
    if (entry === undefined) return undefined;
    this.#streams.delete(stream);
    entry.cancelReplayTimeout();
    return entry;
  }

  #refOf(channel: WorkerChannel, localKey: LocalMediaSourceKey): WorkerMediaSourceRef {
    return { key: channel.contractKeyOf(localKey), channel, localKey };
  }

  /** Настоящий `Uint8Array` этого мира на обычном буфере, 1…предел байт (снятыми геттерами). */
  #isOwnBytes(value: unknown): value is Uint8Array {
    const { binary } = this.#environment;
    if (typeof value !== "object" || value === null || Object.getPrototypeOf(value) !== binary.Uint8Array.prototype) return false;
    try {
      const length = binary.typedArrayByteLength(value);
      // Геттер `byteLength` ArrayBuffer бросает на SharedArrayBuffer: такой буфер не наш.
      binary.arrayBufferByteLength(binary.typedArrayBuffer(value));
      return length >= 1 && length <= MAX_PAGE_BYTES_EVENT_BYTES;
    } catch {
      return false;
    }
  }

  /**
   * Байты пришли из воркера плотной копией (её буфер передан нам). Если вид занимает буфер
   * целиком — передаём дальше тот же буфер, иначе (так хук не шлёт) — своя плотная копия.
   */
  #transferableBytes(bytes: Uint8Array): Uint8Array {
    const { binary } = this.#environment;
    const buffer = binary.typedArrayBuffer(bytes);
    const dense = binary.typedArrayByteOffset(bytes) === 0 && binary.typedArrayByteLength(bytes) === binary.arrayBufferByteLength(buffer);
    return dense ? bytes : copyForTransfer(binary, bytes);
  }
}
