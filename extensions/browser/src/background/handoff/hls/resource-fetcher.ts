// Качальщик ресурсов HLS (сессия 10a2, решение 228): на каждый `fetch_resource` плеера фон по
// номерку находит адрес в стойке, качает его сетью браузера и отдаёт тело рамками вида 2, затем
// `resource_fetch_finished`. Аналог range-fetcher.ts для файла, но запросы идут ПАРАЛЛЕЛЬНО
// (до MAX_CONCURRENT_RESOURCE_FETCHES) - плеер сам решает, что и когда ему нужно.
//
// Инварианты договора с приёмником плеера:
// - КАЖДЫЙ принятый запрос кончается ровно одним `resource_fetch_finished` (отменять запросы плеер
//   не умеет, а брошенный держит свой слот, пока мы не ответим). Исключение - `dispose`: поколение
//   уже запечатано плеером, его запоздалые сообщения он выбрасывает;
// - рамки запроса строго подряд, `offset` считается от 0 ОТДАННОГО тела (не от смещения куска);
//   `completed{size}` равен числу отданных байт, иначе плеер закроет сеанс кодом 4003;
// - рамки идут по мере прихода тела (плеер ждёт ответ ~15 с), кроме плейлистов: те переписываются
//   целиком;
// - обратное давление: пока очередь сеанса больше RANGE_FEED_HIGH_WATER_BYTES, тело не читается.
//
// Приватность и безопасность: качается только то, что выдала стойка (нет в стойке - `refused` без
// сети); конечный адрес после перенаправлений проверяется `allowsAddress`; в плейлистах настоящие
// адреса заменены номерками (playlist-rewriter.ts).
//
// Вид ответа:
// - `Range` слайса: ждём 206. Сервер, проигнорировавший Range и приславший 200, - не ошибка:
//   пропускаем первые `offset` байт потока и отдаём `length` следующих;
// - тело длиннее `max_bytes` - `too_large` (по Content-Length сразу, иначе по потоку);
// - тело короче запрошенного слайса - `completed` с настоящим размером.

import { MAX_CONCURRENT_RESOURCE_FETCHES, MAX_DATA_FRAME_PAYLOAD_BYTES, type ResourceByteRange, type ResourceFetchOutcome, type ResourceHandle } from "../../../protocol/index.ts";
import type { Scheduler } from "../../host-link/native-port.ts";
import type { RangeFeedSink, RangeFetch, RangeFetchInit, RangeFetchResponse } from "../range-fetcher.ts";
import { RANGE_FEED_DRAIN_POLL_MS, RANGE_FEED_HIGH_WATER_BYTES } from "../range-fetcher.ts";
import type { DeskEntry, HandleDesk, ResourceKind } from "./handle-desk.ts";
import { MAX_PLAYLIST_LEADING_WHITESPACE_BYTES, PLAYLIST_SIGNATURE, PLAYLIST_SNIFF_BYTES, UTF8_BOM_BYTES } from "./limits.ts";
import { rewritePlaylist, type IssueResource } from "./playlist-rewriter.ts";

/** Запрос плеера `fetch_resource` (уже строго разобранный протоколом). */
export interface ResourceFetchRequest {
  readonly request_id: number;
  readonly resource: ResourceHandle;
  readonly byte_range: ResourceByteRange;
  readonly max_bytes: number;
}

export interface HlsResourceFetcherParameters {
  /** Стойка сеанса: нужен только поиск по номерку. */
  readonly desk: Pick<HandleDesk, "lookup">;
  /** Сеть браузера, уже обёрнутая заслоном локальной сети и правилом Referer. */
  readonly fetch: RangeFetch;
  /** Можно ли ходить на конечный адрес после перенаправлений. */
  readonly allowsAddress: (finalUrl: string) => boolean;
  /** Выдача номерков для адресов из переписываемого плейлиста (допуск + запись в стойку). */
  readonly issueFromPlaylist: IssueResource;
  readonly sink: RangeFeedSink;
  readonly scheduler: Scheduler;
}

export interface HlsResourceFetcher {
  /** Новый `fetch_resource` плеера. */
  request(request: ResourceFetchRequest): void;
  /** Конец поколения или сеанса: оборвать все загрузки, больше ничего не слать. Повтор - ничего. */
  dispose(): void;
}

/** Чем кончилось чтение тела: итог или «оборвали» (`dispose`, сообщений не будет). */
type BodyOutcome = ResourceFetchOutcome | undefined;

const REFUSED: ResourceFetchOutcome = { kind: "failed", failure: { kind: "refused" } };
const NETWORK_ERROR: ResourceFetchOutcome = { kind: "failed", failure: { kind: "network_error" } };
const TOO_LARGE: ResourceFetchOutcome = { kind: "failed", failure: { kind: "too_large" } };

/** Начало `Content-Range` ответа 206: `bytes 100-199/1000`. */
const CONTENT_RANGE_START = /^bytes (\d+)-\d+\//;
/** Байты UTF-8 BOM (U+FEFF). */
const UTF8_BOM: readonly number[] = [0xef, 0xbb, 0xbf];
const HTTP_STATUS_OK_MIN = 200;
const HTTP_STATUS_OK_MAX = 299;
const HTTP_STATUS_PARTIAL = 206;
const HTTP_STATUS_FULL = 200;

export function createHlsResourceFetcher(parameters: HlsResourceFetcherParameters): HlsResourceFetcher {
  const { sink, scheduler } = parameters;
  // Запросы в работе: номер -> способ оборвать.
  const inFlight = new Map<number, AbortController>();
  let disposed = false;

  function finish(request: ResourceFetchRequest, outcome: ResourceFetchOutcome): void {
    if (disposed) return;
    sink.enqueue({ kind: "message", message: { type: "resource_fetch_finished", request_id: request.request_id, outcome } });
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

  /** Отдать кусок рамками ≤ 1 МиБ; `delivered` - сколько байт тела уже отдано. */
  function emitFrames(request: ResourceFetchRequest, delivered: number, chunk: Uint8Array): void {
    for (let start = 0; start < chunk.length; start += MAX_DATA_FRAME_PAYLOAD_BYTES) {
      const payload = chunk.subarray(start, start + MAX_DATA_FRAME_PAYLOAD_BYTES);
      sink.enqueue({ kind: "frame", header: { kind: "range_bytes", request_id: request.request_id, offset: delivered + start }, payload });
    }
  }

  /** Отдать готовый (переписанный) плейлист рамками, соблюдая обратное давление. */
  async function emitBuffered(request: ResourceFetchRequest, body: Uint8Array, signal: AbortSignal): Promise<BodyOutcome> {
    for (let start = 0; start < body.length; start += MAX_DATA_FRAME_PAYLOAD_BYTES) {
      await waitForRoom(signal);
      if (signal.aborted || disposed) return undefined;
      emitFrames(request, start, body.subarray(start, start + MAX_DATA_FRAME_PAYLOAD_BYTES));
    }
    return { kind: "completed", size_bytes: body.length };
  }

  /** Плейлист: переписать целиком и отдать. Не UTF-8 и не переписывается - `refused`. */
  async function serveRewrittenPlaylist(
    request: ResourceFetchRequest,
    original: Uint8Array,
    finalUrl: string,
    signal: AbortSignal,
  ): Promise<BodyOutcome> {
    let text: string;
    try {
      text = new TextDecoder("utf-8", { fatal: true }).decode(original);
    } catch {
      // Не UTF-8: переписать нечем, а отдать как есть - значит отдать настоящие адреса.
      return REFUSED;
    }
    const rewritten = rewritePlaylist(text, finalUrl, parameters.issueFromPlaylist);
    if (!rewritten.ok) return REFUSED;
    const body = new TextEncoder().encode(rewritten.text);
    if (body.length > request.max_bytes) return TOO_LARGE;
    return emitBuffered(request, body, signal);
  }

  /**
   * Прочитать тело ответа. `skip` - сколько первых байт потока выбросить (200 на слайс), `limit` -
   * сколько байт отдать самое большее (длина слайса; для целого - `max_bytes`, превышение -
   * `too_large`).
   */
  async function readBody(
    request: ResourceFetchRequest,
    response: RangeFetchResponse,
    plan: {
      readonly skip: number;
      readonly limit: number;
      readonly mayBePlaylist: boolean;
      /** Номерок выдан как плейлист: тело, не ставшее плейлистом, плееру НЕ отдаётся (`refused`). */
      readonly mustBePlaylist: boolean;
      /** База относительных ссылок плейлиста: конечный адрес ответа, а если он неизвестен ("") - адрес запроса. */
      readonly baseUrl: string;
    },
    signal: AbortSignal,
  ): Promise<BodyOutcome> {
    if (response.body === null) return { kind: "completed", size_bytes: 0 };
    const reader = response.body.getReader();
    const isWhole = request.byte_range.kind === "whole";
    let skipLeft = plan.skip;
    let delivered = 0;
    // Пока тип тела не ясен, байты придерживаются; `held` пуст, когда решение принято.
    let sniffing = plan.mayBePlaylist;
    let isPlaylist = false;
    let held: Uint8Array[] = [];
    let heldBytes = 0;
    // Номерок-плейлист получил не плейлист: сырые байты (с настоящими адресами) плееру не уходят.
    let rejectedAsNotPlaylist = false;

    /** Решение по началу тела принято: плейлист (копим) или нет (отдаём то, что придержали). */
    const decideBySniff = (): void => {
      sniffing = false;
      isPlaylist = startsWithPlaylistSignature(concatenate(held, heldBytes));
      if (!isPlaylist && plan.mustBePlaylist) {
        rejectedAsNotPlaylist = true;
        held = [];
        heldBytes = 0;
      } else if (!isPlaylist) {
        for (const piece of held) {
          emitFrames(request, delivered, piece);
          delivered += piece.length;
        }
        held = [];
        heldBytes = 0;
      }
    };

    try {
      for (;;) {
        await waitForRoom(signal);
        if (signal.aborted || disposed) return undefined;
        const step = await reader.read();
        if (signal.aborted || disposed) return undefined;
        if (step.done) break;
        if (rejectedAsNotPlaylist) return REFUSED;

        let chunk = step.value;
        if (skipLeft > 0) {
          const skipped = Math.min(skipLeft, chunk.length);
          skipLeft -= skipped;
          chunk = chunk.subarray(skipped);
        }
        if (chunk.length === 0) continue;

        const seen = delivered + heldBytes;
        if (seen + chunk.length > plan.limit) {
          // Целое тело больше потолка - `too_large`; слайс просто обрезается по длине.
          if (isWhole) return TOO_LARGE;
          chunk = chunk.subarray(0, plan.limit - seen);
        }

        if (sniffing || isPlaylist) {
          held.push(chunk);
          heldBytes += chunk.length;
          if (sniffing && heldBytes >= PLAYLIST_SNIFF_BYTES) decideBySniff();
          if (rejectedAsNotPlaylist) return REFUSED;
        } else {
          emitFrames(request, delivered, chunk);
          delivered += chunk.length;
        }
        // Слайс набрал свою длину: остальное не нужно.
        if (!isWhole && delivered + heldBytes >= plan.limit) break;
      }

      if (sniffing) decideBySniff();
      if (rejectedAsNotPlaylist) return REFUSED;
      if (isPlaylist) {
        return await serveRewrittenPlaylist(request, concatenate(held, heldBytes), plan.baseUrl, signal);
      }
      return { kind: "completed", size_bytes: delivered };
    } catch {
      // Обрыв сети посреди тела: уже отданные рамки остаются в силе, итог - сбой.
      return signal.aborted ? undefined : NETWORK_ERROR;
    } finally {
      reader.cancel().catch(() => {
        // Поток уже закрыт или упал - освобождать нечего.
      });
    }
  }

  /** Отработать запрос до итога (`undefined` - загрузку оборвали). */
  async function serve(request: ResourceFetchRequest, entry: DeskEntry, abort: AbortController): Promise<BodyOutcome> {
    const { signal } = abort;
    const slice = request.byte_range.kind === "slice" ? request.byte_range : undefined;
    // Плейлист переписывается только целиком: кусок плейлиста отдать нечем, а сырой - нельзя.
    if (slice !== undefined && entry.kind === "playlist") return REFUSED;
    const init: RangeFetchInit = {
      headers: slice === undefined ? {} : { Range: `bytes=${slice.offset}-${slice.offset + slice.length - 1}` },
      credentials: "include",
      cache: cacheFor(entry.kind),
      redirect: "follow",
      signal,
    };
    let response: RangeFetchResponse;
    try {
      response = await parameters.fetch(entry.url, init);
    } catch {
      return signal.aborted ? undefined : NETWORK_ERROR;
    }
    if (signal.aborted || disposed) {
      cancelBody(response);
      return undefined;
    }
    // Вторая линия защиты после заслона браузера: конечный адрес после перенаправлений.
    if (response.url !== "" && response.url !== entry.url && !parameters.allowsAddress(response.url)) {
      cancelBody(response);
      abort.abort();
      return REFUSED;
    }
    if (response.status < HTTP_STATUS_OK_MIN || response.status > HTTP_STATUS_OK_MAX) {
      cancelBody(response);
      return { kind: "failed", failure: { kind: "http_error", status: response.status } };
    }
    if (slice === undefined) {
      const declared = declaredLength(response);
      if (declared !== undefined && declared > request.max_bytes) {
        cancelBody(response);
        return TOO_LARGE;
      }
    }
    if (slice !== undefined && response.status === HTTP_STATUS_PARTIAL && !startsAt(response, slice.offset)) {
      // Сервер отдал другой кусок, чем просили: рамки легли бы не на свои места.
      cancelBody(response);
      return NETWORK_ERROR;
    }
    const plan = {
      // 200 на запрос слайса: сервер проигнорировал Range и шлёт файл с начала.
      skip: slice !== undefined && response.status === HTTP_STATUS_FULL ? slice.offset : 0,
      limit: slice === undefined ? request.max_bytes : slice.length,
      mayBePlaylist: slice === undefined,
      mustBePlaylist: entry.kind === "playlist",
      baseUrl: response.url === "" ? entry.url : response.url,
    };
    return readBody(request, response, plan, signal);
  }

  /** Один запрос от принятия до итога. */
  async function run(request: ResourceFetchRequest, entry: DeskEntry, abort: AbortController): Promise<void> {
    let outcome: BodyOutcome;
    try {
      outcome = await serve(request, entry, abort);
    } catch {
      // Непредвиденный сбой внутри разбора: запрос всё равно обязан кончиться.
      outcome = NETWORK_ERROR;
    } finally {
      inFlight.delete(request.request_id);
    }
    if (outcome !== undefined) finish(request, outcome);
  }

  return {
    request(request) {
      if (disposed) return;
      // Повтор номера запроса в работе - нарушение плеера; второй ответ на тот же номер был бы вреден.
      if (inFlight.has(request.request_id)) return;
      const entry = parameters.desk.lookup(request.resource);
      if (entry === undefined) {
        finish(request, REFUSED);
        return;
      }
      if (inFlight.size >= MAX_CONCURRENT_RESOURCE_FETCHES) {
        finish(request, REFUSED);
        return;
      }
      const abort = new AbortController();
      inFlight.set(request.request_id, abort);
      void run(request, entry, abort);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      for (const abort of inFlight.values()) abort.abort();
      inFlight.clear();
    },
  };
}

/** Кэш браузера по виду ресурса: плейлисты живые - без кэша, сегменты - с кэшем (перемотка назад). */
function cacheFor(kind: ResourceKind): RangeFetchInit["cache"] {
  return kind === "playlist" ? "no-store" : "default";
}

/** `Content-Length`, если он есть и верный. */
function declaredLength(response: RangeFetchResponse): number | undefined {
  const value = response.headers.get("content-length");
  if (value === null || !/^\d+$/.test(value)) return undefined;
  return Number(value);
}

/** Начинается ли 206 ровно с `offset` (по `Content-Range`; нет заголовка - верим серверу). */
function startsAt(response: RangeFetchResponse, offset: number): boolean {
  const match = CONTENT_RANGE_START.exec(response.headers.get("content-range") ?? "");
  return match === null ? true : Number(match[1]) === offset;
}

/** Склеить придержанные куски в один. */
function concatenate(pieces: readonly Uint8Array[], totalBytes: number): Uint8Array {
  const joined = new Uint8Array(totalBytes);
  let position = 0;
  for (const piece of pieces) {
    joined.set(piece, position);
    position += piece.length;
  }
  return joined;
}

/** Пробельные ASCII-байты, допустимые перед меткой: пробел, табуляция, LF, CR. */
const ASCII_WHITESPACE_BYTES: readonly number[] = [0x20, 0x09, 0x0a, 0x0d];

/**
 * Начинается ли тело с `#EXTM3U` после необязательного UTF-8 BOM и не более
 * MAX_PLAYLIST_LEADING_WHITESPACE_BYTES пробельных байт.
 */
function startsWithPlaylistSignature(bytes: Uint8Array): boolean {
  const hasBom = UTF8_BOM.every((expected, index) => bytes[index] === expected);
  let start = hasBom ? UTF8_BOM_BYTES : 0;
  const whitespaceEnd = start + MAX_PLAYLIST_LEADING_WHITESPACE_BYTES;
  while (start < whitespaceEnd && start < bytes.length && ASCII_WHITESPACE_BYTES.includes(bytes[start] ?? -1)) start += 1;
  for (let index = 0; index < PLAYLIST_SIGNATURE.length; index += 1) {
    if (bytes[start + index] !== PLAYLIST_SIGNATURE.charCodeAt(index)) return false;
  }
  return true;
}

/** Не читать тело ответа, который не нужен. */
function cancelBody(response: RangeFetchResponse): void {
  response.body?.cancel().catch(() => {
    // Тело уже закрыто - отменять нечего.
  });
}

