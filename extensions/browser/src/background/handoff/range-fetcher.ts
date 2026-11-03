// Загрузчик диапазонов файла `<video src>` (сессия 09b, решения 100–104 и 188): на каждый
// `fetch_range` плеера фон сам качает кусок файла с `Range` через сеть браузера и отдаёт его
// рамками вида 2, затем `range_fetch_finished`.
//
// Почему фон, а не страница: чужой CDN без CORS из страницы и из контент-скрипта не отдаётся
// (замер 00c, archive.org), а фону права на сайты снимают CORS. Куки сайта идут с запросом
// (`credentials: "include"`) — так отдаются файлы за входом.
//
// Один загрузчик — один файл (одно поколение-файл сеанса). Он владеет:
// - адресом файла (адрес плееру не уходит — протокол адресов не несёт);
// - «паспортом» файла по первому ответу (ETag, Last-Modified, полный размер): сменился — отказ
//   `resource_changed`;
// - видом размера из первого `completed` (известен/неизвестен — не переключается);
// - очередью запросов и одним текущим `fetch` (строго по одному).
//
// Инварианты договора с приёмником плеера (crates/pushed-media-source/src/range_file/state.rs):
// - рамки запроса — строго подряд от его смещения, не дальше `offset + length` и известного
//   размера; после `range_fetch_finished` запроса — ни одной его рамки;
// - короткое чтение — только у конца файла (иначе честный `network_error`, плеер повторит с
//   первого недостающего байта);
// - КАЖДЫЙ начатый запрос честно кончается: отменять запросы плеер не умеет. Плеер держит одно
//   активное чтение: новый `fetch_range` значит, что прежние им брошены (перемотка). Поэтому новый
//   запрос «вытесняет» старые: текущий `fetch` обрывается, а они кончаются `network_error` — плеер
//   молча выбросит их как устаревшие, а перемотка не ждёт докачки ненужного куска;
// - рамки и итог идут в ту же очередь сеанса, что и всё прочее, — итог не обгоняет свои байты;
// - очередь сеанса ограничена (128 МиБ): пока в ней больше RANGE_FEED_HIGH_WATER_BYTES, тело
//   дальше не читается (обратное давление до сети);
// - после `dispose` (конец сеанса или новое поколение — MSE или другой файл) — ни рамки, ни
//   сообщения: текущий `fetch` обрывается. Итог брошенного запроса плееру не нужен: его поколение
//   устарело, и такие сообщения его сторож поколений выбрасывает.
//
// Известное ограничение безопасности (находка 11 ревью 09b; разбор атак — сессия 09c). Адрес
// файла выбирает СТРАНИЦА (`<video src>`), а качает его ФОН — с куками пользователя
// (`credentials: "include"`) и с правами экстеншена на сайты: без CORS и без вопроса браузера о
// доступе к локальной сети (Local Network Access). Враждебная страница может так заставить фон
// сходить по адресу, куда сама дотянуться не может: на другой сайт с куками пользователя, в
// локальную сеть или на `localhost` (роутер, принтер, панель разработчика). Байты уходят только
// плееру (не странице), а запросы — только GET с `Range`, но сам факт запроса и его побочные
// эффекты (GET с действием на старом оборудовании, чтение по таймингу) остаются. Поведение пока
// не меняется: ограничения адреса (схема, частные сети, совпадение с сайтом вкладки, вопрос
// пользователю) решает 09c.

import { MAX_DATA_FRAME_PAYLOAD_BYTES, type RangeFetchFailure, type RangeFetchOutcome, type ResourceSize } from "../../protocol/index.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import type { OutgoingItem } from "./page-event-translator.ts";
import {
  identityOf,
  planRangeResponse,
  reportedSize,
  sameResource,
  type ResourceIdentity,
  type ResponseHeaders,
} from "./range-response.ts";

/** Запрос плеера `fetch_range` (уже строго разобранный протоколом). */
export interface RangeRequest {
  readonly request_id: number;
  readonly offset: number;
  readonly length: number;
}

/** Ответ `fetch` — та часть `Response`, которой мы пользуемся (в тестах — настоящий `Response`). */
export interface RangeFetchResponse {
  readonly status: number;
  readonly headers: ResponseHeaders;
  readonly body: ReadableStream<Uint8Array> | null;
}

/** Параметры запроса — та часть `RequestInit`, которую мы задаём. */
export interface RangeFetchInit {
  readonly headers: Readonly<Record<string, string>>;
  readonly credentials: "include";
  readonly cache: "no-store";
  readonly redirect: "follow";
  readonly signal: AbortSignal;
}

/** Сеть браузера (в продукте — глобальный `fetch` фона, в тестах — фейк). */
export type RangeFetch = (url: string, init: RangeFetchInit) => Promise<RangeFetchResponse>;

/** Куда уходят рамки и итоги: очередь сеанса (session-socket.ts). */
export interface RangeFeedSink {
  enqueue(item: OutgoingItem): void;
  /** Байт в очереди сеанса, ещё не отданных сокету. */
  queuedBytes(): number;
}

export interface RangeFetcherParameters {
  readonly url: string;
  readonly fetch: RangeFetch;
  readonly sink: RangeFeedSink;
  readonly scheduler: Scheduler;
}

export interface RangeFetcher {
  /** Новый `fetch_range` плеера: вытесняет все прежние (они честно кончаются `network_error`). */
  request(request: RangeRequest): void;
  /** Конец сеанса: оборвать текущий `fetch`, больше ничего не слать. Повтор — ничего. */
  dispose(): void;
}

/**
 * Сколько байт может лежать в очереди сеанса, прежде чем загрузчик перестанет читать тело.
 * Один запрос плеера — до 16 МиБ; 32 МиБ дают запас на два запроса и сообщения, а до потолка
 * очереди (128 МиБ) остаётся место для всплеска страницы.
 */
export const RANGE_FEED_HIGH_WATER_BYTES = 32 * 1024 * 1024;

/** Как часто проверяем, освободилась ли очередь сеанса (как SOCKET_DRAIN_POLL_MS у сокета). */
export const RANGE_FEED_DRAIN_POLL_MS = 20;

/**
 * Сколько подзапросов допускается на один запрос плеера. Сервер вправе отдать меньше, чем
 * попросили (CDN режут диапазоны по 1–10 МиБ), тогда докачиваем остаток новым подзапросом с того
 * же места — рамки остаются подряд. 32 подзапроса покрывают 16 МиБ даже порциями по 512 КиБ;
 * больше — сервер ведёт себя странно, честнее отказать `network_error` (плеер повторит).
 */
export const MAX_SUBREQUESTS_PER_RANGE = 32;

/** Запрос в работе или в очереди. */
interface QueuedRequest {
  readonly request: RangeRequest;
  /** Пришёл более новый запрос — этот плеер уже бросил. */
  superseded: boolean;
}

/** Как кончилось чтение одного ответа. */
type BodyReadResult =
  | { readonly kind: "read"; readonly bytes: number; readonly streamEnded: boolean }
  | { readonly kind: "failed" }
  | { readonly kind: "stopped" };

export function createRangeFetcher(parameters: RangeFetcherParameters): RangeFetcher {
  const { url, sink, scheduler } = parameters;
  const queue: QueuedRequest[] = [];
  let current: { readonly entry: QueuedRequest; readonly abort: AbortController } | undefined;
  let running = false;
  let disposed = false;
  // Паспорт файла по первому годному ответу.
  let identity: ResourceIdentity | undefined;
  // Вид размера из первого `completed`: дальше он не меняется.
  let firstSizeReport: ResourceSize | undefined;

  function finish(request: RangeRequest, outcome: RangeFetchOutcome): void {
    if (disposed) return;
    sink.enqueue({ kind: "message", message: { type: "range_fetch_finished", request_id: request.request_id, outcome } });
  }

  const failed = (failure: RangeFetchFailure): RangeFetchOutcome => ({ kind: "failed", failure });

  /** Известный размер файла (из первого `completed` или паспорта). */
  function knownSize(): number | undefined {
    if (firstSizeReport?.kind === "known") return firstSizeReport.size_bytes;
    return identity?.totalSize;
  }

  function completed(totalSize: number | undefined): RangeFetchOutcome {
    const size = reportedSize(firstSizeReport, totalSize ?? knownSize());
    firstSizeReport = size;
    return { kind: "completed", resource_size: size };
  }

  /** Подождать, пока очередь сеанса не опустится ниже порога (или загрузку не оборвут). */
  function waitForRoom(signal: AbortSignal): Promise<void> {
    return new Promise((resolve) => {
      const check = (): void => {
        if (signal.aborted || sink.queuedBytes() <= RANGE_FEED_HIGH_WATER_BYTES) {
          resolve();
          return;
        }
        scheduler.setTimeout(check, RANGE_FEED_DRAIN_POLL_MS);
      };
      check();
    });
  }

  /** Отдать кусок тела рамками ≤ 1 МиБ, начиная со смещения файла `fileOffset`. */
  function emitFrames(request: RangeRequest, fileOffset: number, chunk: Uint8Array): void {
    for (let start = 0; start < chunk.length; start += MAX_DATA_FRAME_PAYLOAD_BYTES) {
      const payload = chunk.subarray(start, start + MAX_DATA_FRAME_PAYLOAD_BYTES);
      sink.enqueue({ kind: "frame", header: { kind: "range_bytes", request_id: request.request_id, offset: fileOffset + start }, payload });
    }
  }

  /**
   * Прочитать тело ответа, отдавая не больше `limit` байт от смещения `fileOffset`. Лишнее
   * (сервер прислал больше, 200 вместо 206) не читается: поток отменяется.
   */
  async function readBody(
    request: RangeRequest,
    body: ReadableStream<Uint8Array>,
    fileOffset: number,
    limit: number,
    signal: AbortSignal,
  ): Promise<BodyReadResult> {
    const reader = body.getReader();
    let bytes = 0;
    try {
      while (bytes < limit) {
        await waitForRoom(signal);
        if (signal.aborted) return { kind: "stopped" };
        const step = await reader.read();
        if (signal.aborted || disposed) return { kind: "stopped" };
        if (step.done) return { kind: "read", bytes, streamEnded: true };
        const chunk = step.value.subarray(0, limit - bytes);
        emitFrames(request, fileOffset + bytes, chunk);
        bytes += chunk.length;
      }
      return { kind: "read", bytes, streamEnded: false };
    } catch {
      // Обрыв сети посреди тела: уже отданные байты остаются в силе, итог — `network_error`.
      return signal.aborted ? { kind: "stopped" } : { kind: "failed" };
    } finally {
      cancelQuietly(reader);
    }
  }

  /** Отработать один запрос плеера до его итога (`undefined` — загрузку оборвали). */
  async function serve(request: RangeRequest, signal: AbortSignal): Promise<RangeFetchOutcome | undefined> {
    const requestEnd = request.offset + request.length;
    let delivered = 0;
    for (let subrequest = 0; ; subrequest += 1) {
      const position = request.offset + delivered;
      if (delivered === request.length) return completed(undefined);
      const size = knownSize();
      // Размер известен и мы у конца: короткое чтение у конца файла честно; запрос целиком за
      // концом — `out_of_range` (сеть не нужна).
      if (size !== undefined && position >= size) return delivered > 0 ? completed(size) : failed("out_of_range");
      if (subrequest >= MAX_SUBREQUESTS_PER_RANGE) return failed("network_error");

      let response: RangeFetchResponse;
      try {
        response = await parameters.fetch(url, {
          headers: { Range: `bytes=${position}-${requestEnd - 1}` },
          credentials: "include",
          // Без кэша браузера: частичные ответы из кэша смешивали бы версии файла.
          cache: "no-store",
          redirect: "follow",
          signal,
        });
      } catch {
        return signal.aborted ? undefined : failed("network_error");
      }
      if (signal.aborted || disposed) {
        cancelBody(response);
        return undefined;
      }

      const plan = planRangeResponse(response.status, response.headers, position);
      if (plan.kind === "out_of_range") {
        cancelBody(response);
        // Продолжение упёрлось в конец файла неизвестного размера: это и есть конец.
        return delivered > 0 ? completed(undefined) : failed("out_of_range");
      }
      if (plan.kind === "failed") {
        cancelBody(response);
        return failed(plan.failure);
      }
      const responseIdentity = identityOf(response.headers, plan.totalSize);
      if (identity === undefined) identity = responseIdentity;
      else if (!sameResource(identity, responseIdentity)) {
        cancelBody(response);
        return failed("resource_changed");
      }
      if (response.body === null) return failed("network_error");

      const remaining = request.length - delivered;
      const limit = plan.bodyBytes === undefined ? remaining : Math.min(remaining, plan.bodyBytes);
      const read = await readBody(request, response.body, position, limit, signal);
      if (read.kind === "stopped") return undefined;
      if (read.kind === "failed") return failed("network_error");
      delivered += read.bytes;
      const reachedEnd = position + read.bytes;
      const totalSize = plan.totalSize ?? knownSize();
      if (totalSize !== undefined && reachedEnd >= totalSize) return completed(totalSize);
      // Тело кончилось раньше обещанного — обрыв, а не конец файла.
      if (read.streamEnded && plan.bodyBytes !== undefined && read.bytes < Math.min(remaining, plan.bodyBytes)) {
        return failed("network_error");
      }
      // 200 без длины кончился — это конец файла неизвестного размера.
      if (read.streamEnded && plan.bodyBytes === undefined) return completed(undefined);
      // Иначе сервер отдал меньше, чем просили (режет диапазоны), — докачиваем с того же места.
      if (read.bytes === 0) return failed("network_error");
    }
  }

  async function pump(): Promise<void> {
    if (running) return;
    running = true;
    try {
      for (let entry = queue.shift(); entry !== undefined && !disposed; entry = queue.shift()) {
        if (entry.superseded) {
          // Плеер этот запрос уже бросил: честный итог без сети (он его выбросит как устаревший).
          finish(entry.request, failed("network_error"));
          continue;
        }
        const abort = new AbortController();
        current = { entry, abort };
        const outcome = await serve(entry.request, abort.signal);
        current = undefined;
        if (disposed) return;
        // Оборванный вытеснением запрос тоже кончается честно: прежние рамки в силе, итог — сбой.
        finish(entry.request, outcome ?? failed("network_error"));
      }
    } finally {
      running = false;
    }
  }

  return {
    request(request) {
      if (disposed) return;
      for (const queued of queue) queued.superseded = true;
      if (current !== undefined) {
        current.entry.superseded = true;
        current.abort.abort();
      }
      queue.push({ request, superseded: false });
      void pump();
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      queue.length = 0;
      current?.abort.abort();
      current = undefined;
    },
  };
}

/** Отменить чтение тела; ошибка отмены уже ничего не меняет (тело нам больше не нужно). */
function cancelQuietly(reader: ReadableStreamDefaultReader<Uint8Array>): void {
  reader.cancel().catch(() => {
    // Поток уже закрыт или упал — освобождать нечего.
  });
}

/** Не читать тело ответа, который не нужен (иначе соединение держалось бы до конца файла). */
function cancelBody(response: RangeFetchResponse): void {
  response.body?.cancel().catch(() => {
    // Тело уже закрыто — отменять нечего.
  });
}
