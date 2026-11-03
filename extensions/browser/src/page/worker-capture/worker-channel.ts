// MAIN-сторона одного обёрнутого воркера (сессия 09b, пакет C): что мы знаем о его MediaSource и
// как говорим с его хуком по личному порту.
//
// Владеет фактами одного воркера:
//  - его номером в документе (`w1`, `w2`, …) — из него и ключа учёта воркера («ms-N») получается
//    ключ договора «w1/ms-N»;
//  - счётом доверенных сообщений `message`/`messageerror`, пришедших на объект Worker (n-е
//    сообщение здесь = n-й успешный `postMessage` воркера, см. src/worker/handle-transfers.ts);
//  - донесениями хука: «в сообщении n ушёл handle MediaSource ms-K» (или «не наш»), какие
//    MediaSource начаты и какие закрыты.
// Потоки (номера, обработчики, сроки) держит реестр документа (`worker-media-sources.ts`): он
// раздаёт номера потоков на весь документ. Канал лишь шлёт команды и отдаёт ему донесения о потоках.

import type { LocalMediaSourceKey } from "../../shared/handoff-contract.ts";
import { parseWorkerReport, type OwnBytesCheck, type WorkerCommand, type WorkerReport, type WorkerStreamId } from "../../shared/worker-channel-contract.ts";
import type { ChannelOriginals } from "../page-channel.ts";
import type { OddityReporter } from "../prototype-wrap.ts";

/** Что известно о handle, ушедшем в сообщении воркера. */
export type HandleOrigin =
  /** Handle MediaSource этого воркера (ключ учёта воркера). */
  | { readonly kind: "own"; readonly localKey: LocalMediaSourceKey }
  /** Хук сказал «не наш» (вложенный воркер, несколько handle в сообщении): честно не сопоставить. */
  | { readonly kind: "foreign" }
  /** Донесения ещё нет (порт и сообщения сайта идут разными путями) или MediaSource ещё без SourceBuffer. */
  | { readonly kind: "unknown_yet" };

/** Кто слушает канал (реестр документа). */
export interface WorkerChannelListener {
  /** У MediaSource воркера появился первый SourceBuffer. */
  onMediaSourceStarted(channel: WorkerChannel, localKey: LocalMediaSourceKey): void;
  /** Донесение о потоке (элемент, конец повтора, отказ). */
  onStreamReport(channel: WorkerChannel, report: Extract<WorkerReport, { kind: "stream_item" | "stream_replayed" | "stream_unavailable" }>): void;
}

/**
 * Расширение канала (нейтральный шов): видит каждый новый канал и может принять донесение, которого
 * договор не знает. В продукте расширения нет; отладочная сборка записи потока страницы подставляет
 * своё и получает так события воркера (`stream-recorder/`).
 */
export interface WorkerChannelExtension {
  /** Появился канал нового воркера. */
  channelCreated(channel: WorkerChannel): void;
  /** Донесение не разобрано договором: `true` — расширение приняло его (иначе это мусор). */
  acceptReport(channel: WorkerChannel, data: unknown): boolean;
}

/** Окружение канала. */
export interface WorkerChannelEnvironment {
  readonly originals: ChannelOriginals;
  readonly isOwnBytes: OwnBytesCheck;
  /** Хук встал: blob-адреса обёртки можно отпустить. */
  readonly onHookReady: () => void;
  /** Исходный скрипт сайта загружен: отложенный revoke его blob-адреса можно исполнить (решение 192). */
  readonly onSiteScriptLoaded: () => void;
  readonly listener: WorkerChannelListener;
  readonly extension?: WorkerChannelExtension;
  readonly reportOddity: OddityReporter;
}

/**
 * Сколько донесений «handle в сообщении n» помним на воркер. Handle — по одному на MediaSource,
 * за жизнь воркера их единицы-десятки; предел не даёт враждебному воркеру раздуть память.
 */
const MAX_REMEMBERED_HANDLE_MESSAGES = 1024;

/** Префикс номера воркера в ключе договора («w1/ms-2»). */
const WORKER_KEY_PREFIX = "w";

export class WorkerChannel {
  /** Номер воркера в документе (с 1). */
  readonly ordinal: number;
  readonly #environment: WorkerChannelEnvironment;
  #port: object | null = null;
  #messagesSeen = 0;
  readonly #handleOrigins = new Map<number, LocalMediaSourceKey | null>();
  readonly #started = new Set<LocalMediaSourceKey>();
  readonly #closed = new Set<LocalMediaSourceKey>();

  constructor(ordinal: number, environment: WorkerChannelEnvironment) {
    this.ordinal = ordinal;
    this.#environment = environment;
  }

  /** Подключить наш конец личного порта (сразу после создания воркера). */
  connect(port: object): void {
    const { originals } = this.#environment;
    this.#port = port;
    originals.addListener(port, "message", (event) => this.#onPortMessage(event), false);
    originals.startPort(port);
  }

  /** На объект Worker пришло доверенное сообщение (или `messageerror`): его номер по порядку. */
  noteWorkerMessage(): number {
    this.#messagesSeen += 1;
    return this.#messagesSeen;
  }

  /** Ключ договора для ключа учёта этого воркера. */
  contractKeyOf(localKey: LocalMediaSourceKey): LocalMediaSourceKey {
    return `${WORKER_KEY_PREFIX}${this.ordinal}/${localKey}`;
  }

  /** Чей handle ушёл в сообщении `messageOrdinal`. */
  originOfMessage(messageOrdinal: number): HandleOrigin {
    if (!this.#handleOrigins.has(messageOrdinal)) return { kind: "unknown_yet" };
    const localKey = this.#handleOrigins.get(messageOrdinal);
    return localKey === null || localKey === undefined ? { kind: "foreign" } : { kind: "own", localKey };
  }

  /** MediaSource воркера живой: есть SourceBuffer и он не закрыт (то же правило, что в главном потоке). */
  isLive(localKey: LocalMediaSourceKey): boolean {
    return this.#started.has(localKey) && !this.#closed.has(localKey);
  }

  /** Попросить хук открыть поток (повтор, затем живое). */
  openStream(stream: WorkerStreamId, localKey: LocalMediaSourceKey): void {
    this.#send({ kind: "open_stream", stream, media_source: localKey });
  }

  /** Попросить хук закрыть все потоки. */
  closeStreams(): void {
    this.#send({ kind: "close_streams" });
  }

  /** Сообщение расширению хука воркера по тому же личному порту (договор — у расширения). */
  sendToExtension(message: unknown): void {
    if (this.#port === null) throw new Error("worker channel is not connected");
    this.#environment.originals.postToPort(this.#port, message, []);
  }

  #send(command: WorkerCommand): void {
    if (this.#port === null) throw new Error("worker channel is not connected");
    this.#environment.originals.postToPort(this.#port, command, []);
  }

  #onPortMessage(event: unknown): void {
    const { originals, isOwnBytes, reportOddity } = this.#environment;
    try {
      const data = originals.eventData(event);
      const report = parseWorkerReport(data, isOwnBytes);
      if (report === undefined) {
        if (this.#environment.extension?.acceptReport(this, data) === true) return;
        reportOddity("worker report is malformed");
        return;
      }
      this.#apply(report);
    } catch {
      reportOddity("worker report handling failed");
    }
  }

  #apply(report: WorkerReport): void {
    const { listener } = this.#environment;
    switch (report.kind) {
      case "hook_ready":
        this.#environment.onHookReady();
        return;
      case "site_script_loaded":
        this.#environment.onSiteScriptLoaded();
        return;
      case "handle_sent":
        this.#rememberHandleOrigin(report.message, report.media_source);
        return;
      case "media_source_started":
        this.#started.add(report.media_source);
        listener.onMediaSourceStarted(this, report.media_source);
        return;
      case "media_source_closed":
        this.#closed.add(report.media_source);
        return;
      case "stream_item":
      case "stream_replayed":
      case "stream_unavailable":
        listener.onStreamReport(this, report);
        return;
    }
  }

  #rememberHandleOrigin(messageOrdinal: number, localKey: LocalMediaSourceKey | null): void {
    if (this.#handleOrigins.size >= MAX_REMEMBERED_HANDLE_MESSAGES) {
      // Самое старое забываем: его MediaSource давно сменился, а новые важнее.
      const oldest = this.#handleOrigins.keys().next();
      if (oldest.done !== true) this.#handleOrigins.delete(oldest.value);
      this.#environment.reportOddity("too many worker media source handles");
    }
    this.#handleOrigins.set(messageOrdinal, localKey);
  }
}
