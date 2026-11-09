// Закачка `<track src>` фоном: каждый исход закрытого списка на фейковой сети.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  createTrackDownloader,
  MAX_CONCURRENT_TRACK_DOWNLOADS,
  MAX_WAITING_TRACK_DOWNLOADS,
  TRACK_DOWNLOAD_TIMEOUT_MS,
  type TrackDownloadOutcome,
} from "../../../src/background/handoff/page-subtitles/track-downloader.ts";
import { MAX_PAGE_SUBTITLE_DOCUMENT_BYTES } from "../../../src/protocol/limits.ts";
import { createFakeClock, flushPromises } from "../support.ts";
import { bytesOf, hangingBody, networkResponse, recordingNetwork, TRACK_URL, VTT_TEXT } from "./subtitle-support.ts";

const ALLOW_ALL = (): boolean => true;
const OVER_LIMIT = MAX_PAGE_SUBTITLE_DOCUMENT_BYTES + 1;

function setupDownloader(answer: Parameters<typeof recordingNetwork>[0]) {
  const clock = createFakeClock();
  const network = recordingNetwork(answer);
  const downloader = createTrackDownloader(clock.scheduler);
  const owner = new AbortController();
  const download = (options: { allowsAddress?: (url: string) => boolean; url?: string; signal?: AbortSignal } = {}): Promise<TrackDownloadOutcome> =>
    downloader.download({ url: options.url ?? TRACK_URL, network: network.fetch, allowsAddress: options.allowsAddress ?? ALLOW_ALL, signal: options.signal ?? owner.signal });
  return { clock, network, owner, download };
}

describe("закачка дорожки: успех", () => {
  it("целое тело со статусом 200 → ready с байтами и подсказкой формата; запрос с куками, кэшем и следованием редиректам", async () => {
    const world = setupDownloader(() => Promise.resolve(networkResponse(bytesOf(VTT_TEXT))));
    const outcome = await world.download();
    assert.equal(outcome.kind, "ready");
    assert.ok(outcome.kind === "ready");
    assert.equal(new TextDecoder().decode(outcome.bytes), VTT_TEXT);
    assert.equal(outcome.formatHint, "web_vtt");
    const init = world.network.calls[0]?.init;
    assert.equal(init?.credentials, "include");
    assert.equal(init?.redirect, "follow");
    assert.equal(init?.cache, "default");
  });
});

describe("закачка дорожки: отказы", () => {
  it("адрес запрещён правилом — в сеть не ушло ничего", async () => {
    const world = setupDownloader(() => Promise.reject(new Error("не должно быть запроса")));
    assert.deepEqual(await world.download({ allowsAddress: () => false }), { kind: "address_refused" });
    assert.equal(world.network.calls.length, 0);
  });

  it("редирект увёл на запрещённый адрес — байты не читаются, отказ address_refused", async () => {
    let bodyCancelled = false;
    const body = new ReadableStream<Uint8Array>({ pull: (controller) => controller.enqueue(bytesOf(VTT_TEXT)), cancel: () => void (bodyCancelled = true) });
    const world = setupDownloader(() => Promise.resolve(networkResponse(body, { url: "http://192.168.1.1/secret.vtt" })));
    const outcome = await world.download({ allowsAddress: (url) => !url.startsWith("http://192.168.") });
    assert.deepEqual(outcome, { kind: "address_refused" });
    await flushPromises();
    assert.equal(bodyCancelled, true, "тело отменено, а не прочитано");
  });

  it("редирект на другой ПУБЛИЧНЫЙ адрес разрешён", async () => {
    const world = setupDownloader(() => Promise.resolve(networkResponse(bytesOf(VTT_TEXT), { url: "https://cdn2.example/en.vtt" })));
    assert.equal((await world.download()).kind, "ready");
  });

  it("сбой сети → network_error", async () => {
    const world = setupDownloader(() => Promise.reject(new TypeError("Failed to fetch")));
    assert.deepEqual(await world.download(), { kind: "network_error" });
  });

  it("статус 404 → http_error со статусом, тело не читается", async () => {
    const world = setupDownloader(() => Promise.resolve(networkResponse(bytesOf("Not found"), { status: 404 })));
    assert.deepEqual(await world.download(), { kind: "http_error", status: 404 });
  });

  it("Content-Length больше 8 МиБ → too_large БЕЗ чтения тела", async () => {
    let bodyPulls = 0;
    const body = new ReadableStream<Uint8Array>({ pull: (controller) => void (bodyPulls += 1, controller.enqueue(new Uint8Array(1024))) });
    const world = setupDownloader(() => Promise.resolve(networkResponse(body, { headers: { "content-length": String(OVER_LIMIT) } })));
    assert.deepEqual(await world.download(), { kind: "too_large" });
    assert.ok(bodyPulls <= 1, "тело дальше первой подкачки браузера не читалось");
  });

  it("Content-Length нет, тело перешло 8 МиБ → too_large по счёту байт, чтение отменено", async () => {
    let cancelled = false;
    const piece = new Uint8Array(1024 * 1024);
    const body = new ReadableStream<Uint8Array>({ pull: (controller) => controller.enqueue(piece), cancel: () => void (cancelled = true) });
    const world = setupDownloader(() => Promise.resolve(networkResponse(body)));
    assert.deepEqual(await world.download(), { kind: "too_large" });
    assert.equal(cancelled, true);
  });

  it("тело ровно 8 МиБ допускается (потолок включительно), если это субтитры", async () => {
    const edge = new Uint8Array(MAX_PAGE_SUBTITLE_DOCUMENT_BYTES);
    edge.set(bytesOf(VTT_TEXT));
    const world = setupDownloader(() => Promise.resolve(networkResponse(edge)));
    const outcome = await world.download();
    assert.ok(outcome.kind === "ready");
    assert.equal(outcome.bytes.length, MAX_PAGE_SUBTITLE_DOCUMENT_BYTES);
  });

  it("обрыв тела посреди чтения → network_error", async () => {
    const body = new ReadableStream<Uint8Array>({ start: (controller) => controller.error(new Error("reset")) });
    const world = setupDownloader(() => Promise.resolve(networkResponse(body)));
    assert.deepEqual(await world.download(), { kind: "network_error" });
  });

  it("не субтитры (HTML страницы-заглушки) → not_subtitles", async () => {
    const world = setupDownloader(() => Promise.resolve(networkResponse(bytesOf("<!doctype html><html><body>login</body></html>"))));
    assert.deepEqual(await world.download(), { kind: "not_subtitles" });
  });

  it("пустое тело → not_subtitles", async () => {
    const world = setupDownloader(() => Promise.resolve(networkResponse(new Uint8Array(0))));
    assert.deepEqual(await world.download(), { kind: "not_subtitles" });
  });

  it("кусочек HLS (X-TIMESTAMP-MAP) → hls_segment, это не дорожка", async () => {
    const segment = "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n00:00.000 --> 00:01.000\nHi\n";
    const world = setupDownloader(() => Promise.resolve(networkResponse(bytesOf(segment))));
    assert.deepEqual(await world.download(), { kind: "hls_segment" });
  });
});

describe("закачка дорожки: срок и отмена", () => {
  it("зависший ответ обрывается по сроку на часах сеанса → timed_out, запрос отменён", async () => {
    const world = setupDownloader((_url, init) => Promise.resolve(networkResponse(hangingBody(init.signal))));
    const pending = world.download();
    await flushPromises();
    world.clock.advance(TRACK_DOWNLOAD_TIMEOUT_MS);
    assert.deepEqual(await pending, { kind: "timed_out" });
    assert.equal(world.network.calls[0]?.init.signal.aborted, true);
    assert.equal(world.clock.liveTimers(), 0, "таймер не течёт");
  });

  it("зависший запрос (ответа нет вовсе) тоже обрывается по сроку", async () => {
    const world = setupDownloader((_url, init) => new Promise((_resolve, reject) => init.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")))));
    const pending = world.download();
    await flushPromises();
    world.clock.advance(TRACK_DOWNLOAD_TIMEOUT_MS);
    assert.deepEqual(await pending, { kind: "timed_out" });
  });

  it("отмена владельцем в разгар чтения → cancelled, запрос отменён, таймер снят", async () => {
    const world = setupDownloader((_url, init) => Promise.resolve(networkResponse(hangingBody(init.signal))));
    const pending = world.download();
    await flushPromises();
    world.owner.abort();
    assert.deepEqual(await pending, { kind: "cancelled" });
    assert.equal(world.network.calls[0]?.init.signal.aborted, true);
    assert.equal(world.clock.liveTimers(), 0);
  });

  it("уже отменённый владелец → cancelled без запроса", async () => {
    const world = setupDownloader(() => Promise.reject(new Error("не должно быть запроса")));
    world.owner.abort();
    assert.deepEqual(await world.download(), { kind: "cancelled" });
    assert.equal(world.network.calls.length, 0);
  });
});

describe("закачка дорожки: одновременность", () => {
  it(`одновременно идёт не больше ${MAX_CONCURRENT_TRACK_DOWNLOADS}; остальные ждут слота и стартуют по мере освобождения`, async () => {
    const releases: (() => void)[] = [];
    const world = setupDownloader(
      () =>
        new Promise((resolve) => {
          releases.push(() => resolve(networkResponse(bytesOf(VTT_TEXT))));
        }),
    );
    const results = Array.from({ length: MAX_CONCURRENT_TRACK_DOWNLOADS + 2 }, () => world.download());
    await flushPromises();
    assert.equal(world.network.calls.length, MAX_CONCURRENT_TRACK_DOWNLOADS);
    releases[0]?.();
    await flushPromises();
    assert.equal(world.network.calls.length, MAX_CONCURRENT_TRACK_DOWNLOADS + 1, "освободившийся слот занял следующий");
    for (let index = 1; index < releases.length; index += 1) {
      releases[index]?.();
      await flushPromises();
    }
    assert.deepEqual((await Promise.all(results)).map((outcome) => outcome.kind), Array(MAX_CONCURRENT_TRACK_DOWNLOADS + 2).fill("ready"));
  });

  it("очередь ожидания полна → queue_full; отмена ожидающей закачки освобождает место без запроса", async () => {
    const world = setupDownloader((_url, init) => Promise.resolve(networkResponse(hangingBody(init.signal))));
    const waiting = new AbortController();
    const running = Array.from({ length: MAX_CONCURRENT_TRACK_DOWNLOADS }, () => world.download());
    const queued = Array.from({ length: MAX_WAITING_TRACK_DOWNLOADS }, () => world.download({ signal: waiting.signal }));
    await flushPromises();
    assert.deepEqual(await world.download(), { kind: "queue_full" });
    waiting.abort();
    assert.deepEqual((await Promise.all(queued)).map((outcome) => outcome.kind), Array(MAX_WAITING_TRACK_DOWNLOADS).fill("cancelled"));
    assert.equal(world.network.calls.length, MAX_CONCURRENT_TRACK_DOWNLOADS, "ожидавшие в сеть не ходили");
    world.owner.abort();
    await Promise.all(running);
  });
});
