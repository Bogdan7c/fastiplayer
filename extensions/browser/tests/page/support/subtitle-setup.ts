// Общие шаги тестов субтитров MAIN-мира (13b2): страница с окружением субтитров, скачивание документов
// через `fetch`/XHR, передача видео и разбор того, что пришло изолированному миру.
import assert from "node:assert/strict";
import type { PageEvent } from "../../../src/shared/handoff-contract.ts";
import type { PageSubtitleDocumentEvent, PageSubtitleEvent, PageSubtitleTrackSourceEvent } from "../../../src/shared/page-subtitle-contract.ts";
import { startHarness, type BridgeHarness, type IsolatedSide } from "./bridge-harness.ts";
import { createFakePage } from "./fake-page.ts";
import { addSubtitleEnvironment, type FakeNetworkReply } from "./fake-subtitle-environment.ts";

export const SITE_ORIGIN = "https://site.example";

export interface SubtitlePage {
  readonly harness: BridgeHarness;
  readonly env: ReturnType<typeof addSubtitleEnvironment>;
}

/** Страница с окружением субтитров и собранным мостом (хуки встают при сборке). */
export function startSubtitlePage(): SubtitlePage {
  const page = createFakePage({ origin: SITE_ORIGIN });
  const env = addSubtitleEnvironment(page);
  return { harness: startHarness({ page, useWindowClock: true }), env };
}

export const encode = (text: string): Uint8Array => new TextEncoder().encode(text);
export const decode = (bytes: Uint8Array): string => new TextDecoder().decode(bytes);

export const VTT_DOCUMENT = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n";

type PageFetch = (url: string) => Promise<{ readonly body: ReadableStream<Uint8Array> | null }>;

/** Дать асинхронному чтению копии (несколько микрозадач и таймер) закончиться. */
export async function settleReads(): Promise<void> {
  for (let round = 0; round < 6; round += 1) await new Promise((resolve) => setTimeout(resolve, 2));
}

/** Страница качает адрес через `fetch` и сама читает своё тело до конца; возвращает прочитанное страницей. */
export async function pageFetches(subtitlePage: SubtitlePage, reply: FakeNetworkReply): Promise<Uint8Array | null> {
  subtitlePage.env.serve(reply);
  const pageFetch = (subtitlePage.harness.page.win as unknown as { fetch: PageFetch }).fetch;
  const response = await pageFetch(reply.url);
  let pageBytes: Uint8Array | null = null;
  if (response.body !== null && reply.endlessChunkBytes === undefined) {
    const reader = response.body.getReader();
    const chunks: number[] = [];
    for (;;) {
      const step = await reader.read();
      if (step.done) break;
      chunks.push(...step.value);
    }
    pageBytes = new Uint8Array(chunks);
  }
  await settleReads();
  return pageBytes;
}

/** Страница качает адрес через XHR; `respond` — как ответила сеть. */
export async function pageXhrs(
  subtitlePage: SubtitlePage,
  reply: FakeNetworkReply,
  responseType: "" | "text" | "json" | "arraybuffer" | "blob",
  value: { text?: string; json?: unknown; buffer?: ArrayBuffer },
): Promise<void> {
  const xhr = new subtitlePage.env.FakeXhr();
  const opened = reply.url;
  xhr.open("GET", opened);
  xhr.responseType = responseType;
  xhr.send();
  xhr.testRespond(reply, value);
  await settleReads();
}

/** Субтитровые события, пришедшие изолированному миру. */
export function subtitleEventsOf(isolated: IsolatedSide): PageSubtitleEvent[] {
  return isolated.received.filter((message): message is PageSubtitleEvent => (message as PageEvent).kind === "subtitle_document" || (message as PageEvent).kind === "subtitle_track_source");
}

export function documentsOf(isolated: IsolatedSide): PageSubtitleDocumentEvent[] {
  return subtitleEventsOf(isolated).filter((event): event is PageSubtitleDocumentEvent => event.kind === "subtitle_document");
}

export function trackSourcesOf(isolated: IsolatedSide): PageSubtitleTrackSourceEvent[] {
  return subtitleEventsOf(isolated).filter((event): event is PageSubtitleTrackSourceEvent => event.kind === "subtitle_track_source");
}

/** Один и ровно один документ, который уже пришёл. */
export function onlyDocument(isolated: IsolatedSide): PageSubtitleDocumentEvent {
  const documents = documentsOf(isolated);
  assert.equal(documents.length, 1, `expected one subtitle document, got ${documents.length}`);
  return documents[0] as PageSubtitleDocumentEvent;
}
