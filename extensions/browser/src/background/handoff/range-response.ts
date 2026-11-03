// Разбор ответа сервера на запрос диапазона файла `<video src>` (сессия 09b). Чистые функции:
// что ответ значит для запроса `fetch_range` — без сети, очереди и таймеров (их держит
// range-fetcher.ts). Каждое правило проверяется модульным тестом.
//
// Договор приёмника плеера (crates/pushed-media-source/src/range_file/state.rs) задаёт, что
// здесь важно:
// - размер файла между запросами не меняется, «известен» ↔ «неизвестен» не переключается;
// - файл, сменившийся между запросами (другие ETag / Last-Modified / полный размер), — окончательный
//   отказ `resource_changed`;
// - сервер, не умеющий `Range` (200 вместо 206), годится только для запроса с начала файла.

import type { RangeFetchFailure, ResourceSize } from "../../protocol/index.ts";

/** Заголовки ответа — та часть `Headers`, которой мы пользуемся (в тестах — настоящий `Headers`). */
export interface ResponseHeaders {
  get(name: string): string | null;
}

/** `Content-Range: bytes first-last/total` (total `*` — сервер размера не знает). */
export interface ContentRange {
  readonly first: number;
  readonly last: number;
  readonly total: number | "unknown";
}

/**
 * Разобрать `Content-Range` ответа 206. `undefined` — заголовка нет или он не в форме
 * «bytes a-b/c» (единственная форма для одиночного диапазона, RFC 9110 §14.4).
 */
export function parseContentRange(header: string | null): ContentRange | undefined {
  if (header === null) return undefined;
  const match = /^\s*bytes\s+(\d{1,16})-(\d{1,16})\/(\d{1,16}|\*)\s*$/i.exec(header);
  if (match === null) return undefined;
  const first = Number(match[1]);
  const last = Number(match[2]);
  const total = match[3] === "*" ? "unknown" : Number(match[3]);
  if (!Number.isSafeInteger(first) || !Number.isSafeInteger(last) || last < first) return undefined;
  if (total !== "unknown" && (!Number.isSafeInteger(total) || last >= total)) return undefined;
  return { first, last, total };
}

/**
 * Длина тела по `Content-Length` (только без `Content-Encoding`: `fetch` распаковывает сжатое
 * тело, и тогда `Content-Length` — длина сжатого, а не файла). `undefined` — длина неизвестна.
 */
export function plainContentLength(headers: ResponseHeaders): number | undefined {
  const encoding = headers.get("content-encoding");
  if (encoding !== null && encoding.trim().toLowerCase() !== "identity") return undefined;
  const header = headers.get("content-length");
  if (header === null || !/^\s*\d{1,16}\s*$/.test(header)) return undefined;
  const length = Number(header);
  return Number.isSafeInteger(length) ? length : undefined;
}

/** «Паспорт» файла по первому ответу: по нему узнаём, что файл на сервере сменился. */
export interface ResourceIdentity {
  readonly etag: string | null;
  readonly lastModified: string | null;
  /** Полный размер из ответа (`undefined` — ответ его не сообщил). */
  readonly totalSize: number | undefined;
}

export function identityOf(headers: ResponseHeaders, totalSize: number | undefined): ResourceIdentity {
  return { etag: headers.get("etag"), lastModified: headers.get("last-modified"), totalSize };
}

/**
 * Тот ли это файл. Сравниваем только то, что сообщили ОБА ответа: сервер, не приславший ETag
 * во втором ответе, не считается сменившим файл.
 */
export function sameResource(first: ResourceIdentity, later: ResourceIdentity): boolean {
  if (first.etag !== null && later.etag !== null && first.etag !== later.etag) return false;
  if (first.lastModified !== null && later.lastModified !== null && first.lastModified !== later.lastModified) return false;
  if (first.totalSize !== undefined && later.totalSize !== undefined && first.totalSize !== later.totalSize) return false;
  return true;
}

/** Что делать с ответом на (под)запрос, начинающийся со смещения `offset`. */
export type RangeResponsePlan =
  /** Читать тело: первый байт тела = `offset`; `bodyBytes` — сколько обещано (если известно). */
  | {
      readonly kind: "read_body";
      readonly bodyBytes: number | undefined;
      /** Полный размер файла по этому ответу (`undefined` — неизвестен). */
      readonly totalSize: number | undefined;
    }
  /** 416: от этого смещения файла нет (за концом). */
  | { readonly kind: "out_of_range" }
  /** Ответ не годится для запроса (не тот диапазон, 200 со смещения > 0, ошибка HTTP). */
  | { readonly kind: "failed"; readonly failure: Extract<RangeFetchFailure, "http_error"> };

const HTTP_OK = 200;
const HTTP_PARTIAL_CONTENT = 206;
const HTTP_RANGE_NOT_SATISFIABLE = 416;

/** Разобрать статус и заголовки ответа на `Range: bytes=offset-…`. */
export function planRangeResponse(status: number, headers: ResponseHeaders, offset: number): RangeResponsePlan {
  if (status === HTTP_RANGE_NOT_SATISFIABLE) return { kind: "out_of_range" };
  if (status === HTTP_PARTIAL_CONTENT) {
    const range = parseContentRange(headers.get("content-range"));
    // Сервер отдал не тот кусок (или не сказал, какой): байты по смещению ставить нельзя.
    if (range === undefined || range.first !== offset) return { kind: "failed", failure: "http_error" };
    return {
      kind: "read_body",
      bodyBytes: range.last - range.first + 1,
      totalSize: range.total === "unknown" ? undefined : range.total,
    };
  }
  if (status === HTTP_OK) {
    // Сервер не понял `Range` и шлёт файл целиком с начала: годится только для запроса с 0.
    if (offset !== 0) return { kind: "failed", failure: "http_error" };
    const length = plainContentLength(headers);
    return { kind: "read_body", bodyBytes: length, totalSize: length };
  }
  return { kind: "failed", failure: "http_error" };
}

/** Размер для `range_fetch_finished{completed}` с учётом правила «не переключается». */
export function reportedSize(firstReport: ResourceSize | undefined, totalSize: number | undefined): ResourceSize {
  // Первый отчёт задаёт вид навсегда: «неизвестен» остаётся неизвестным, даже если сервер потом
  // размер сообщил; «известен» — тем же числом (смену числа ловит `sameResource`).
  if (firstReport !== undefined) return firstReport;
  return totalSize === undefined ? { kind: "unknown" } : { kind: "known", size_bytes: totalSize };
}
