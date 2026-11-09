// Закачка файла `<track src>` фоном (сессия 13b2, решения 272, 275).
//
// Адрес дорожки назвала страница, а качает фон - с куками пользователя и правами экстеншена. Поэтому
// закачка идёт той же сетью, что и файл `<video src>` (заслон локальных адресов внутри, Referer
// правилом снаружи; композицию собирает `session-source-feeds.ts`) и по тем же правилам допуска:
// адрес проверяется до запроса и ещё раз после перенаправлений. Образец - hls/resource-fetcher.ts.
//
// Одна закачка = один запрос целого тела:
// - тело не больше 8 МиБ: `Content-Length` больше - отказ без чтения, иначе счёт байт при чтении;
// - срок `TRACK_DOWNLOAD_TIMEOUT_MS` по часам сеанса (не голый setTimeout);
// - отмена по `AbortSignal` владельца (конец сеанса, смена документа);
// - не больше `MAX_CONCURRENT_TRACK_DOWNLOADS` одновременно, остальные ждут своей очереди
//   (не больше `MAX_WAITING_TRACK_DOWNLOADS`; лишние отвергаются: страница не должна копить очередь);
// - содержимое опознаётся `sniffSubtitleDocument`: кусочек HLS и не-субтитры - отказ с причиной.
// Исход - закрытый список, а не bool: журналу и тестам важно, ПОЧЕМУ закачка не удалась.

import { MAX_PAGE_SUBTITLE_DOCUMENT_BYTES } from "../../../protocol/limits.ts";
import type { SubtitleFormatHint } from "../../../protocol/session/page-subtitles.ts";
import { sniffSubtitleDocument } from "../../../shared/subtitle-document-sniff.ts";
import type { Scheduler } from "../../host-link/native-port.ts";
import type { RangeFetch, RangeFetchResponse } from "../range-fetcher.ts";

/** Срок одной закачки, мс (с момента, когда она заняла слот). */
export const TRACK_DOWNLOAD_TIMEOUT_MS = 30_000;

/** Сколько закачек дорожек идёт одновременно. */
export const MAX_CONCURRENT_TRACK_DOWNLOADS = 3;

/** Сколько закачек ждёт слота; сверх этого - отказ `queue_full`. */
export const MAX_WAITING_TRACK_DOWNLOADS = 64;

/** Исход закачки дорожки. */
export type TrackDownloadOutcome =
  /** Документ скачан и опознан. */
  | { readonly kind: "ready"; readonly bytes: Uint8Array; readonly formatHint: Exclude<SubtitleFormatHint, "unknown"> }
  /** Адрес (названный страницей или конечный после перенаправлений) запрещён правилом; тело не читалось. */
  | { readonly kind: "address_refused" }
  /** Сеть не дала ответ (или заслон браузера остановил запрос). */
  | { readonly kind: "network_error" }
  /** Не 2xx. */
  | { readonly kind: "http_error"; readonly status: number }
  /** Тело больше 8 МиБ. */
  | { readonly kind: "too_large" }
  /** Содержимое не субтитры (в том числе пустое). */
  | { readonly kind: "not_subtitles" }
  /** Кусочек WebVTT плейлиста HLS: это не дорожка, её соберёт hls.js. */
  | { readonly kind: "hls_segment" }
  /** Не уложились в срок. */
  | { readonly kind: "timed_out" }
  /** Владелец отменил закачку (конец сеанса, смена документа). */
  | { readonly kind: "cancelled" }
  /** Очередь ожидания полна. */
  | { readonly kind: "queue_full" };

/** Что качать и чем. */
export interface TrackDownloadRequest {
  readonly url: string;
  /** Сеть с заслоном и Referer под эту дорожку. */
  readonly network: RangeFetch;
  /** Можно ли ходить по адресу (правило сеанса; зовётся и для конечного адреса). */
  readonly allowsAddress: (url: string) => boolean;
  readonly signal: AbortSignal;
}

export interface TrackDownloader {
  download(request: TrackDownloadRequest): Promise<TrackDownloadOutcome>;
}

/** Отметка ожидающей закачки в очереди слотов. */
interface WaitingSlot {
  readonly start: () => void;
}

export function createTrackDownloader(scheduler: Scheduler): TrackDownloader {
  let runningCount = 0;
  const waiting: WaitingSlot[] = [];

  /** Занять слот: сразу или дождавшись очереди; очередь полна - `queue_full`, отмена из очереди - `cancelled`. */
  async function takeSlot(signal: AbortSignal): Promise<"taken" | "cancelled" | "queue_full"> {
    if (runningCount < MAX_CONCURRENT_TRACK_DOWNLOADS) {
      runningCount += 1;
      return "taken";
    }
    if (waiting.length >= MAX_WAITING_TRACK_DOWNLOADS) return "queue_full";
    return new Promise((resolve) => {
      const slot: WaitingSlot = {
        start: () => {
          signal.removeEventListener("abort", leaveQueue);
          runningCount += 1;
          resolve("taken");
        },
      };
      const leaveQueue = (): void => {
        const index = waiting.indexOf(slot);
        if (index !== -1) waiting.splice(index, 1);
        resolve("cancelled");
      };
      signal.addEventListener("abort", leaveQueue, { once: true });
      waiting.push(slot);
    });
  }

  /** Освободить слот и пустить следующего. */
  function releaseSlot(): void {
    runningCount -= 1;
    waiting.shift()?.start();
  }

  return {
    async download(request) {
      if (request.signal.aborted) return { kind: "cancelled" };
      // Запрещённый адрес отказывает ДО очереди: в сеть не уходит ничего.
      if (!request.allowsAddress(request.url)) return { kind: "address_refused" };
      const slot = await takeSlot(request.signal);
      if (slot === "queue_full") return { kind: "queue_full" };
      if (slot === "cancelled") return { kind: "cancelled" };
      try {
        return await downloadWithDeadline(request, scheduler);
      } finally {
        releaseSlot();
      }
    },
  };
}

/** Закачка под сроком: срок и отмена владельца сходятся в одном `AbortController`. */
async function downloadWithDeadline(request: TrackDownloadRequest, scheduler: Scheduler): Promise<TrackDownloadOutcome> {
  const attempt = new AbortController();
  let timedOut = false;
  const abortFromOwner = (): void => attempt.abort();
  request.signal.addEventListener("abort", abortFromOwner, { once: true });
  const deadline = scheduler.setTimeout(() => {
    timedOut = true;
    attempt.abort();
  }, TRACK_DOWNLOAD_TIMEOUT_MS);
  try {
    const outcome = await fetchWholeBody(request, attempt.signal);
    // Оборванное чтение мы не отличаем от сетевого сбоя, пока не знаем причину обрыва.
    if (timedOut) return { kind: "timed_out" };
    if (request.signal.aborted) return { kind: "cancelled" };
    return outcome;
  } finally {
    scheduler.clearTimeout(deadline);
    request.signal.removeEventListener("abort", abortFromOwner);
  }
}

/** Запрос, проверки ответа и чтение тела с потолком. */
async function fetchWholeBody(request: TrackDownloadRequest, signal: AbortSignal): Promise<TrackDownloadOutcome> {
  let response: RangeFetchResponse;
  try {
    response = await request.network(request.url, { headers: {}, credentials: "include", cache: "default", redirect: "follow", signal });
  } catch {
    // Сеть, заслон браузера или обрыв: причину (срок, отмена) назовёт вызывающий.
    return { kind: "network_error" };
  }
  // Вторая линия защиты: если заслон браузера не встал, конечный адрес проверяем сами.
  if (response.url !== "" && response.url !== request.url && !request.allowsAddress(response.url)) {
    cancelBody(response);
    return { kind: "address_refused" };
  }
  if (response.status < 200 || response.status > 299) {
    cancelBody(response);
    return { kind: "http_error", status: response.status };
  }
  const declared = declaredLength(response);
  if (declared !== undefined && declared > MAX_PAGE_SUBTITLE_DOCUMENT_BYTES) {
    cancelBody(response);
    return { kind: "too_large" };
  }
  const read = await readBodyUpTo(response, MAX_PAGE_SUBTITLE_DOCUMENT_BYTES);
  if (read.kind !== "read") return read.kind === "too_large" ? { kind: "too_large" } : { kind: "network_error" };
  return judgeContent(read.bytes);
}

/** Опознать скачанное по содержимому. */
function judgeContent(bytes: Uint8Array): TrackDownloadOutcome {
  if (bytes.length === 0) return { kind: "not_subtitles" };
  const verdict = sniffSubtitleDocument(bytes);
  if (verdict.kind === "document") return { kind: "ready", bytes, formatHint: verdict.format };
  return { kind: verdict.kind };
}

/** `Content-Length`, если он есть и верный. */
function declaredLength(response: RangeFetchResponse): number | undefined {
  const value = response.headers.get("content-length");
  if (value === null || !/^\d+$/.test(value)) return undefined;
  return Number(value);
}

type BodyRead = { readonly kind: "read"; readonly bytes: Uint8Array } | { readonly kind: "too_large" } | { readonly kind: "failed" };

/** Прочитать тело целиком, считая байты; на превышении потолка чтение отменяется. */
async function readBodyUpTo(response: RangeFetchResponse, limitBytes: number): Promise<BodyRead> {
  if (response.body === null) return { kind: "read", bytes: new Uint8Array(0) };
  const reader = response.body.getReader();
  const pieces: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.length;
      if (total > limitBytes) {
        await reader.cancel().catch(() => {
          // Тело уже закрыто - отменять нечего.
        });
        return { kind: "too_large" };
      }
      pieces.push(value);
    }
  } catch {
    // Обрыв посреди тела: что прочитано, то не документ.
    return { kind: "failed" };
  }
  const joined = new Uint8Array(total);
  let position = 0;
  for (const piece of pieces) {
    joined.set(piece, position);
    position += piece.length;
  }
  return { kind: "read", bytes: joined };
}

/** Не читать тело ответа, который не нужен. */
function cancelBody(response: RangeFetchResponse): void {
  response.body?.cancel().catch(() => {
    // Тело уже закрыто - отменять нечего.
  });
}
