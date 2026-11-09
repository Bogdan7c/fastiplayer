// Общие помощники тестов субтитров страницы: документы, события, разбор провода.
import assert from "node:assert/strict";
import { decodeDataFrame, type PageSessionMessage } from "../../../src/protocol/index.ts";
import type { PageSubtitleDocumentEvent, PageSubtitleNaming, PageSubtitleTrackSourceEvent } from "../../../src/shared/page-subtitle-contract.ts";
import type { RangeFetch, RangeFetchInit, RangeFetchResponse } from "../../../src/background/handoff/range-fetcher.ts";
import type { FakeWebSocket } from "../support.ts";

export const VTT_TEXT = "WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n";
export const NAMING_EN: PageSubtitleNaming = { language: "en", label: "English", auto_generated: false };
export const TRACK_URL = "https://cdn.example/subs/en.vtt";

export const bytesOf = (text: string): Uint8Array => new TextEncoder().encode(text);

/** Событие готового документа страницы. */
export function documentEvent(overrides: Partial<PageSubtitleDocumentEvent> = {}): PageSubtitleDocumentEvent {
  return {
    kind: "subtitle_document",
    key: 1,
    naming: NAMING_EN,
    format_hint: "web_vtt",
    origin: "page_download",
    display: "showing",
    bytes: bytesOf(VTT_TEXT),
    ...overrides,
  };
}

/** Событие объявленного `<track src>`. */
export function trackSourceEvent(overrides: Partial<PageSubtitleTrackSourceEvent> = {}): PageSubtitleTrackSourceEvent {
  return {
    kind: "subtitle_track_source",
    key: 1,
    naming: NAMING_EN,
    display: "not_showing",
    url: TRACK_URL,
    referrer_origin: null,
    referrer_policy: "no-referrer",
    ...overrides,
  };
}

/** Документ, как его принял плеер: заголовок и собранные из рамок вида 3 байты. */
export interface WireDocument {
  readonly offer: Extract<PageSessionMessage, { readonly type: "subtitle_track_offered" }>;
  readonly bytes: Uint8Array;
}

/** Разобрать провод: каждый `subtitle_track_offered` и рамки вида 3 его дорожки (смещения подряд с 0). */
export function subtitleDocumentsOnWire(socket: FakeWebSocket): WireDocument[] {
  const documents: { offer: WireDocument["offer"]; pieces: Uint8Array[]; next: number }[] = [];
  const messages = socket.sentMessages();
  let messageIndex = 0;
  for (const payload of socket.sent) {
    if (typeof payload === "string") {
      const message = messages[messageIndex++];
      if (message?.type === "subtitle_track_offered") documents.push({ offer: message, pieces: [], next: 0 });
      continue;
    }
    const frame = decodeDataFrame(payload);
    assert.ok(frame.ok);
    if (frame.value.header.kind !== "page_subtitle_bytes") continue;
    const header = frame.value.header;
    const open = documents.findLast((document) => document.offer.track === header.track);
    assert.ok(open !== undefined, "рамка для непредложенной дорожки");
    assert.equal(header.offset, open.next, "смещения идут подряд с 0");
    open.pieces.push(frame.value.payload);
    open.next += frame.value.payload.length;
  }
  return documents.map(({ offer, pieces, next }) => {
    assert.equal(next, offer.byte_length, "рамки покрывают документ ровно");
    const bytes = new Uint8Array(next);
    let position = 0;
    for (const piece of pieces) {
      bytes.set(piece, position);
      position += piece.length;
    }
    return { offer, bytes };
  });
}

/** Ответ сети: тело, статус, конечный адрес, заголовки. */
export function networkResponse(body: Uint8Array | ReadableStream<Uint8Array> | null, options: { status?: number; url?: string; headers?: Record<string, string> } = {}): RangeFetchResponse {
  const response = new Response(body as BodyInit | null, { status: options.status ?? 200, headers: options.headers ?? {} });
  if (options.url !== undefined) Object.defineProperty(response, "url", { value: options.url });
  return response;
}

/** Фейк сети: запоминает вызовы, отвечает как скажет `answer`. */
export function recordingNetwork(answer: (url: string, init: RangeFetchInit) => Promise<RangeFetchResponse>) {
  const calls: { readonly url: string; readonly init: RangeFetchInit }[] = [];
  const fetch: RangeFetch = (url, init) => {
    calls.push({ url, init });
    return answer(url, init);
  };
  return { fetch, calls };
}

/** Тело, которое зависает до отмены запроса. */
export function hangingBody(signal: AbortSignal): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(controller) {
      signal.addEventListener("abort", () => controller.error(new DOMException("aborted", "AbortError")));
    },
  });
}
