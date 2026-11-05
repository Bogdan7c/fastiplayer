// Личный канал «MAIN-мир ⇄ хук внутри выделенного воркера» (сессия 09b, решение 63).
//
// Как он появляется: обёртка `Worker` в MAIN-мире (src/page/worker-capture) создаёт воркер из
// blob-адреса «наш хук + исходный скрипт сайта», а сразу после конструктора шлёт ему ПЕРВЫМ
// сообщением метку `WORKER_CHANNEL_HANDSHAKE` и один MessagePort (transfer). Хук в воркере
// (src/worker) ставит слушатель `message` в фазе захвата раньше кода сайта, забирает это сообщение
// и гасит его (`stopImmediatePropagation`) — сайт его не видит. Дальше обмен идёт только по порту.
//
// Ключи MediaSource по этому каналу — ключи учёта воркера «ms-N» (без номера воркера): номер
// воркера знает только MAIN-мир, он и превращает ключ в «w<номер>/ms-N» договора участка 1.
//
// Кто кому что говорит:
//  - MAIN → воркер (`WorkerCommand`): открыть поток MediaSource (повтор «памяти без копий» и затем
//    живые события), закрыть все потоки;
//  - воркер → MAIN (`WorkerReport`): хук встал; исходный скрипт сайта загружен; MediaSource получил первый SourceBuffer или закрылся;
//    в каком по счёту сообщении `postMessage` воркера ушёл handle какого MediaSource; элементы
//    открытого потока (события MSE и куски байтов с transfer); повтор закончен; поток не открыть.
// Код сайта в воркере до порта не дотягивается (он в замыкании хука), но форма всё равно
// проверяется строго: в MAIN-мир не должно пройти ничего, что сломало бы учёт контроллера.

import type { LocalMediaSourceKey, LocalMseEvent } from "./handoff-contract.ts";
import { isTrackerMediaSourceKey, parseLocalMseEvent } from "./page-message-validation.ts";
import { MAX_SOURCE_BUFFERS_PER_GENERATION } from "../protocol/limits.ts";

/** Поле-метка рукопожатия канала воркера. Значение — версия этого договора. */
export const WORKER_CHANNEL_MARKER_FIELD = "fastiplayerBridgeWorkerChannel";

/** Версия договора канала воркера (меняется вместе с этим файлом). */
export const WORKER_CHANNEL_VERSION = 1;

/** Сообщение рукопожатия (порт идёт отдельно, в списке transfer). */
export const WORKER_CHANNEL_HANDSHAKE: Readonly<Record<string, number>> = Object.freeze({
  [WORKER_CHANNEL_MARKER_FIELD]: WORKER_CHANNEL_VERSION,
});

/**
 * Номер потока: его выбирает MAIN-мир при каждом открытии (1, 2, …), и все элементы потока несут
 * его. Так запоздавшие куски закрытого потока не смешиваются с повтором нового (тот же MediaSource
 * можно открыть снова в следующей передаче).
 */
export type WorkerStreamId = number;

/** Наибольший номер потока и сообщения (u32). */
const MAX_U32 = 0xffff_ffff;

/** MAIN-мир → воркер. */
export type WorkerCommand =
  /** Повторить журнал MediaSource и дальше слать его живые события под номером `stream`. */
  | { readonly kind: "open_stream"; readonly stream: WorkerStreamId; readonly media_source: LocalMediaSourceKey }
  /** Передача кончилась: больше ничего не слать. */
  | { readonly kind: "close_streams" };

/** Элемент потока: событие MSE или кусок байтов (плотная копия, передаётся transfer). */
export type WorkerStreamItem =
  | { readonly kind: "mse"; readonly event: LocalMseEvent }
  | { readonly kind: "bytes"; readonly media_source: LocalMediaSourceKey; readonly source_buffer: number; readonly bytes: Uint8Array };

/** Воркер → MAIN-мир. */
export type WorkerReport =
  /** Хук встал и принял порт: blob-адреса обёртки больше не нужны. */
  | { readonly kind: "hook_ready" }
  /**
   * Исходный скрипт сайта загружен (решение 192): классический — `importScripts` вернулся (или
   * бросил), модульный — модуль сайта выполнен. С этого мига blob-адрес сайта воркеру больше не
   * нужен, и отложенный `URL.revokeObjectURL` страницы можно исполнить (blob-revoke-deferral.ts).
   */
  | { readonly kind: "site_script_loaded" }
  /** У MediaSource появился первый SourceBuffer (с этого мига он «живой», как в главном потоке). */
  | { readonly kind: "media_source_started"; readonly media_source: LocalMediaSourceKey }
  /** MediaSource отсоединён от элемента (`sourceclose`): передавать его уже нечего. */
  | { readonly kind: "media_source_closed"; readonly media_source: LocalMediaSourceKey }
  /**
   * Handle ушёл в главный поток в `message`-м по счёту успешном `postMessage` воркера (с 1).
   * `media_source` — чей это handle; `null` — не нашего учёта (вложенный воркер, несколько handle
   * в одном сообщении — тогда сопоставление неоднозначно, и мы честно его не делаем).
   */
  | { readonly kind: "handle_sent"; readonly message: number; readonly media_source: LocalMediaSourceKey | null }
  | { readonly kind: "stream_item"; readonly stream: WorkerStreamId; readonly item: WorkerStreamItem }
  /** Повтор журнала потока окончен; дальше идут только живые элементы. */
  | { readonly kind: "stream_replayed"; readonly stream: WorkerStreamId }
  /** Поток не открыть: MediaSource неизвестен, собран или закрыт, отправка сорвалась. */
  | { readonly kind: "stream_unavailable"; readonly stream: WorkerStreamId }
  /** Счётчик копий воркера отказал (решение 200): страница льёт байты сверх предела, поток закрыт. */
  | { readonly kind: "stream_flooded"; readonly stream: WorkerStreamId };

type PlainRecord = Readonly<Record<string, unknown>>;

function isPlainRecord(value: unknown): value is PlainRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value) && !ArrayBuffer.isView(value);
}

/** Ровно эти собственные поля: лишнее или недостающее — отказ. */
function hasExactKeys(record: PlainRecord, keys: readonly string[]): boolean {
  const ownKeys = Object.keys(record);
  return ownKeys.length === keys.length && keys.every((key) => Object.hasOwn(record, key));
}

function isPositiveU32(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= MAX_U32;
}

function isSourceBufferIndex(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value < MAX_SOURCE_BUFFERS_PER_GENERATION;
}

/** Это сообщение рукопожатия канала воркера: ровно одно поле-метка нужной версии. */
export function isWorkerChannelHandshake(message: unknown): boolean {
  if (!isPlainRecord(message)) return false;
  return hasExactKeys(message, [WORKER_CHANNEL_MARKER_FIELD]) && message[WORKER_CHANNEL_MARKER_FIELD] === WORKER_CHANNEL_VERSION;
}

/** Разбор команды MAIN-мира (сторона воркера); `undefined` — мусор. */
export function parseWorkerCommand(value: unknown): WorkerCommand | undefined {
  if (!isPlainRecord(value)) return undefined;
  switch (value["kind"]) {
    case "open_stream": {
      const stream = value["stream"];
      const mediaSource = value["media_source"];
      return hasExactKeys(value, ["kind", "stream", "media_source"]) && isPositiveU32(stream) && isTrackerMediaSourceKey(mediaSource)
        ? { kind: "open_stream", stream, media_source: mediaSource }
        : undefined;
    }
    case "close_streams":
      return hasExactKeys(value, ["kind"]) ? { kind: "close_streams" } : undefined;
    default:
      return undefined;
  }
}

/**
 * Проверка байтов на стороне MAIN-мира: настоящий `Uint8Array` этого мира, 1…предел байт. Её даёт
 * вызывающий (снятые на document_start двоичные геттеры), чтобы разбор не зависел от глобалов,
 * которые страница могла подменить.
 */
export type OwnBytesCheck = (value: unknown) => value is Uint8Array;

function parseStreamItem(value: unknown, isOwnBytes: OwnBytesCheck): WorkerStreamItem | undefined {
  if (!isPlainRecord(value)) return undefined;
  switch (value["kind"]) {
    case "mse": {
      const event = parseLocalMseEvent(value["event"]);
      // У воркера ключ — только «ms-N»: номер воркера ставит MAIN-мир.
      return hasExactKeys(value, ["kind", "event"]) && event !== undefined && isTrackerMediaSourceKey(event.media_source)
        ? { kind: "mse", event }
        : undefined;
    }
    case "bytes": {
      const mediaSource = value["media_source"];
      const sourceBuffer = value["source_buffer"];
      const bytes = value["bytes"];
      return hasExactKeys(value, ["kind", "media_source", "source_buffer", "bytes"]) &&
        isTrackerMediaSourceKey(mediaSource) &&
        isSourceBufferIndex(sourceBuffer) &&
        isOwnBytes(bytes)
        ? { kind: "bytes", media_source: mediaSource, source_buffer: sourceBuffer, bytes }
        : undefined;
    }
    default:
      return undefined;
  }
}

/** Ключ донесения о жизни MediaSource (`{kind, media_source}`); `undefined` — форма не та. */
function mediaSourceOfLifecycleReport(value: PlainRecord): LocalMediaSourceKey | undefined {
  const mediaSource = value["media_source"];
  return hasExactKeys(value, ["kind", "media_source"]) && isTrackerMediaSourceKey(mediaSource) ? mediaSource : undefined;
}

/** Номер потока донесения о потоке (`{kind, stream}`); `undefined` — форма не та. */
function streamOfStreamReport(value: PlainRecord): WorkerStreamId | undefined {
  const stream = value["stream"];
  return hasExactKeys(value, ["kind", "stream"]) && isPositiveU32(stream) ? stream : undefined;
}

/** Разбор донесения воркера (сторона MAIN-мира); `undefined` — мусор. */
export function parseWorkerReport(value: unknown, isOwnBytes: OwnBytesCheck): WorkerReport | undefined {
  if (!isPlainRecord(value)) return undefined;
  switch (value["kind"]) {
    case "hook_ready":
      return hasExactKeys(value, ["kind"]) ? { kind: "hook_ready" } : undefined;
    case "site_script_loaded":
      return hasExactKeys(value, ["kind"]) ? { kind: "site_script_loaded" } : undefined;
    case "media_source_started": {
      const mediaSource = mediaSourceOfLifecycleReport(value);
      return mediaSource === undefined ? undefined : { kind: "media_source_started", media_source: mediaSource };
    }
    case "media_source_closed": {
      const mediaSource = mediaSourceOfLifecycleReport(value);
      return mediaSource === undefined ? undefined : { kind: "media_source_closed", media_source: mediaSource };
    }
    case "handle_sent": {
      const message = value["message"];
      const mediaSource = value["media_source"];
      return hasExactKeys(value, ["kind", "message", "media_source"]) && isPositiveU32(message) && (mediaSource === null || isTrackerMediaSourceKey(mediaSource))
        ? { kind: "handle_sent", message, media_source: mediaSource }
        : undefined;
    }
    case "stream_item": {
      const stream = value["stream"];
      const item = parseStreamItem(value["item"], isOwnBytes);
      return hasExactKeys(value, ["kind", "stream", "item"]) && isPositiveU32(stream) && item !== undefined ? { kind: "stream_item", stream, item } : undefined;
    }
    case "stream_replayed": {
      const stream = streamOfStreamReport(value);
      return stream === undefined ? undefined : { kind: "stream_replayed", stream };
    }
    case "stream_unavailable": {
      const stream = streamOfStreamReport(value);
      return stream === undefined ? undefined : { kind: "stream_unavailable", stream };
    }
    case "stream_flooded": {
      const stream = streamOfStreamReport(value);
      return stream === undefined ? undefined : { kind: "stream_flooded", stream };
    }
    default:
      return undefined;
  }
}
