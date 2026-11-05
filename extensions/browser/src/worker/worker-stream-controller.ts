// Контроллер хука внутри выделенного воркера: что воркер сообщает MAIN-миру о своих MediaSource
// и как отдаёт их поток по команде (сессия 09b, пакет C).
//
// Владеет:
//  - картой «ключ учёта → запись MediaSource» (слабые ссылки: журнал живёт, пока жив MediaSource);
//  - тем, в каком сообщении `postMessage` ушёл handle каждого MediaSource (до его первого
//    SourceBuffer ключа ещё нет — номер сообщения ждёт его здесь);
//  - открытыми потоками «ключ → номер потока MAIN-мира».
//
// Инварианты:
//  - `handle_sent` с ключом уходит раньше `media_source_started` того же MediaSource: к мигу, когда
//    MAIN-мир узнаёт «у него SourceBuffer», связь «handle → ключ» у него уже есть;
//  - поток открывается только командой MAIN-мира: повтор журнала (снимок + хвост, «память без
//    копий», решение 176), затем `stream_replayed`, затем живые элементы — всё синхронно, без
//    пропусков и вклиниваний (одна задача воркера);
//  - пока поток не открыт, в порт не уходит ни одного байта видео;
//  - сбой отправки потока закрывает его честно (`stream_unavailable`, а отказ счётчика копий —
//    `stream_flooded`), а не оставляет дыру.

import type { LocalMediaSourceKey } from "../shared/handoff-contract.ts";
import type { WorkerCommand, WorkerStreamId } from "../shared/worker-channel-contract.ts";
import type { BinaryIntrinsics } from "../page/binary-view.ts";
import type { ForwardByteMeter } from "../page/forward-byte-meter.ts";
import type { ReplayItem } from "../page/journal/media-journal.ts";
import type { MediaSourceRecord, MseListener } from "../page/mse-tracker.ts";
import type { OddityReporter } from "../page/prototype-wrap.ts";
import { notifyTap, type ChunkTap, type StreamOrigin } from "../page/handoff-tap.ts";
import { sendStreamItem, StreamBytesRefused } from "../page/stream-sender.ts";
import type { HandleTransferObserver } from "./handle-transfers.ts";
import type { ReportSink } from "./report-link.ts";

/** Что нужно контроллеру снаружи. */
export interface WorkerStreamEnvironment {
  readonly link: ReportSink;
  readonly binary: BinaryIntrinsics;
  /** Предел копий в сеанс (решение 200): у воркера свой, он копирует в своём потоке. */
  readonly meter: ForwardByteMeter;
  /** `readyState` MediaSource оригинальным геттером (`null` — прочитать не вышло). */
  readonly readyStateOf: (mediaSource: object) => string | null;
  /** Подписаться на `sourceclose` MediaSource оригинальным `addEventListener`. */
  readonly onSourceClose: (mediaSource: object, listener: () => void) => void;
  readonly reportOddity: OddityReporter;
  /** Подслушивание уходящих кусков (только отладочная сборка записи потока); без него — ничего. */
  readonly tap?: ChunkTap;
}

/** Сколько записей копится до уборки умерших (MediaSource за жизнь воркера — единицы). */
const SWEEP_THRESHOLD = 64;

export class WorkerStreamController implements MseListener, HandleTransferObserver {
  readonly #environment: WorkerStreamEnvironment;
  readonly #records = new Map<LocalMediaSourceKey, WeakRef<MediaSourceRecord>>();
  /** MediaSource, чей handle уже ушёл, но SourceBuffer ещё нет → номер сообщения. */
  readonly #sentInMessage = new WeakMap<object, number>();
  /** MediaSource, у которого уже есть ключ учёта (handle может уйти и после — так не бывает, но честно). */
  readonly #keyOfStarted = new WeakMap<object, LocalMediaSourceKey>();
  readonly #streams = new Map<LocalMediaSourceKey, WorkerStreamId>();

  constructor(environment: WorkerStreamEnvironment) {
    this.#environment = environment;
  }

  // ------------------------------------------------------------ учёт MSE (MseListener)

  onMediaSourceStarted(record: MediaSourceRecord): void {
    if (this.#records.size >= SWEEP_THRESHOLD) this.#sweepDeadRecords();
    this.#records.set(record.key, new WeakRef(record));
    const mediaSource = record.mediaSource.deref();
    if (mediaSource === undefined) return;
    this.#keyOfStarted.set(mediaSource, record.key);
    const sentInMessage = this.#sentInMessage.get(mediaSource);
    if (sentInMessage !== undefined) {
      this.#sentInMessage.delete(mediaSource);
      this.#report({ kind: "handle_sent", message: sentInMessage, media_source: record.key });
    }
    this.#report({ kind: "media_source_started", media_source: record.key });
    this.#environment.onSourceClose(mediaSource, () => {
      this.#streams.delete(record.key);
      this.#report({ kind: "media_source_closed", media_source: record.key });
    });
  }

  onItem(record: MediaSourceRecord, item: ReplayItem): void {
    const stream = this.#streams.get(record.key);
    if (stream === undefined) return;
    this.#sendOrClose(stream, record.key, [item], "live");
  }

  // ------------------------------------------------------------ handle (HandleTransferObserver)

  messagePosted(messageOrdinal: number, mediaSourcesOfHandles: readonly (object | null)[]): void {
    const only = mediaSourcesOfHandles.length === 1 ? mediaSourcesOfHandles[0] : null;
    if (only === null || only === undefined) {
      // Чужой handle (вложенный воркер) или несколько в одном сообщении: MAIN-мир не сможет
      // однозначно сопоставить — честно говорим «не наш».
      this.#report({ kind: "handle_sent", message: messageOrdinal, media_source: null });
      return;
    }
    const key = this.#keyOfStarted.get(only);
    if (key !== undefined) this.#report({ kind: "handle_sent", message: messageOrdinal, media_source: key });
    else this.#sentInMessage.set(only, messageOrdinal);
  }

  // ------------------------------------------------------------ команды MAIN-мира

  handle(command: WorkerCommand): void {
    switch (command.kind) {
      case "open_stream":
        this.#openStream(command.stream, command.media_source);
        return;
      case "close_streams":
        this.#streams.clear();
        return;
    }
  }

  #openStream(stream: WorkerStreamId, key: LocalMediaSourceKey): void {
    const record = this.#records.get(key)?.deref();
    const mediaSource = record?.mediaSource.deref();
    if (record === undefined || mediaSource === undefined || this.#environment.readyStateOf(mediaSource) === "closed") {
      this.#report({ kind: "stream_unavailable", stream });
      return;
    }
    // Тот же MediaSource открывается заново (следующая передача) — старый номер забыт.
    this.#streams.set(key, stream);
    if (!this.#sendOrClose(stream, key, record.journal.replay(), "replay")) return;
    this.#report({ kind: "stream_replayed", stream });
  }

  /** Отправить элементы потока; `false` — отправка сорвалась, поток закрыт честно. */
  #sendOrClose(stream: WorkerStreamId, key: LocalMediaSourceKey, items: readonly ReplayItem[], origin: StreamOrigin): boolean {
    try {
      for (const item of items) {
        sendStreamItem(this.#environment, key, item, (message, transfer) => {
          // Подписчик видит кусок ДО отправки: transfer обнуляет буфер (см. handoff-tap.ts).
          const { tap } = this.#environment;
          if (message.kind === "bytes" && tap !== undefined) {
            const chunk = { mediaSource: message.media_source, sourceBuffer: message.source_buffer, bytes: message.bytes, origin };
            notifyTap(this.#environment.reportOddity, () => tap.chunkSent(chunk));
          }
          this.#environment.link.send({ kind: "stream_item", stream, item: message }, transfer);
        });
      }
      return true;
    } catch (error) {
      this.#environment.reportOddity("worker stream send failed");
      this.#streams.delete(key);
      // Флуд — вина страницы, а не сбой: MAIN-мир должен знать разницу, чтобы причина дошла до модели вкладки.
      this.#report({ kind: error instanceof StreamBytesRefused ? "stream_flooded" : "stream_unavailable", stream });
      return false;
    }
  }

  /** Донесение без байтов; сбой порта — след, а не исключение в код сайта. */
  #report(report: Parameters<ReportSink["send"]>[0]): void {
    try {
      this.#environment.link.send(report, []);
    } catch {
      this.#environment.reportOddity(`worker report ${report.kind} could not be sent`);
    }
  }

  #sweepDeadRecords(): void {
    for (const [key, reference] of this.#records) {
      if (reference.deref() === undefined) this.#records.delete(key);
    }
  }
}
