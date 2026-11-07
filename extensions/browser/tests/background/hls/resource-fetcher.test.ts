// Качальщик ресурсов HLS на настоящих `Response`/`ReadableStream`: рамки, слайсы, исходы, плейлисты,
// обратное давление и конец поколения.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { FileAddressRule } from "../../../src/background/handoff/file-address-guard.ts";
import { createHandleDesk, type HandleDesk } from "../../../src/background/handoff/hls/handle-desk.ts";
import { admitsResourceUrl } from "../../../src/background/handoff/hls/resource-admission.ts";
import { createHlsResourceFetcher, type HlsResourceFetcher, type ResourceFetchRequest } from "../../../src/background/handoff/hls/resource-fetcher.ts";
import type { OutgoingItem } from "../../../src/background/handoff/page-event-translator.ts";
import type { RangeFetch, RangeFetchInit } from "../../../src/background/handoff/range-fetcher.ts";
import { RANGE_FEED_DRAIN_POLL_MS, RANGE_FEED_HIGH_WATER_BYTES } from "../../../src/background/handoff/range-fetcher.ts";
import {
  decodeDataFrame,
  encodeDataFrame,
  formatResourceAddress,
  MAX_CONCURRENT_RESOURCE_FETCHES,
  MAX_DATA_FRAME_PAYLOAD_BYTES,
  type ResourceFetchOutcome,
} from "../../../src/protocol/index.ts";
import { createFakeClock, flushPromises } from "../support.ts";

const MIB = 1024 * 1024;
const RULE: FileAddressRule = { pageHost: "video.example.com", setting: "block", consentedHost: null };
const SEGMENT_URL = "https://cdn.example.net/seg.ts";
const PLAYLIST_URL = "https://cdn.example.net/live/media.m3u8";

/** Байты с узнаваемым узором: байт по смещению i = i mod 251. */
function patternOf(size: number): Uint8Array<ArrayBuffer> {
  return new Uint8Array(size).map((_, index) => index % 251);
}

function chunked(bytes: Uint8Array, chunkSize: number): Uint8Array[] {
  const chunks: Uint8Array[] = [];
  for (let start = 0; start < bytes.length; start += chunkSize) chunks.push(bytes.slice(start, start + chunkSize));
  return chunks;
}

/** Поток из кусков; рвётся при отмене запроса; может зависнуть или упасть после N кусков. */
function streamOf(chunks: readonly Uint8Array[], signal: AbortSignal, options: { failAfter?: number } = {}): ReadableStream<Uint8Array> {
  let index = 0;
  return new ReadableStream<Uint8Array>({
    start(controller) {
      signal.addEventListener("abort", () => {
        try {
          controller.error(new DOMException("aborted", "AbortError"));
        } catch {
          // уже закрыт
        }
      });
    },
    pull(controller) {
      if (options.failAfter !== undefined && index === options.failAfter) {
        controller.error(new TypeError("network error"));
        return;
      }
      const chunk = chunks[index];
      index += 1;
      if (chunk !== undefined) controller.enqueue(chunk);
      else controller.close();
    },
  });
}

/** Ответ с заданным конечным адресом. */
function responseOf(body: ReadableStream<Uint8Array> | null, init: { status?: number; headers?: Record<string, string>; url?: string }): Response {
  const response = new Response(body, { status: init.status ?? 200, headers: init.headers ?? {} });
  Object.defineProperty(response, "url", { value: init.url ?? "" });
  return response;
}

function setup(fetch: RangeFetch, options: { allowsAddress?: (url: string) => boolean } = {}) {
  const clock = createFakeClock();
  const desk: HandleDesk = createHandleDesk();
  const items: OutgoingItem[] = [];
  const queue = { bytes: 0 };
  const fetcher: HlsResourceFetcher = createHlsResourceFetcher({
    desk,
    fetch,
    allowsAddress: options.allowsAddress ?? ((url) => admitsResourceUrl(url, RULE)),
    issueFromPlaylist: (url, kind) => (admitsResourceUrl(url, RULE) ? desk.issue(url, kind) : undefined),
    sink: { enqueue: (item) => void items.push(item), queuedBytes: () => queue.bytes },
    scheduler: clock.scheduler,
  });
  return { clock, desk, items, queue, fetcher };
}

function wholeRequest(requestId: number, resource: number, maxBytes = 8 * MIB): ResourceFetchRequest {
  return { request_id: requestId, resource, byte_range: { kind: "whole" }, max_bytes: maxBytes };
}

function sliceRequest(requestId: number, resource: number, offset: number, length: number): ResourceFetchRequest {
  return { request_id: requestId, resource, byte_range: { kind: "slice", offset, length }, max_bytes: 8 * MIB };
}

async function settle(): Promise<void> {
  for (let round = 0; round < 40; round += 1) await flushPromises();
}

/** Итоги по порядку. */
function finishes(items: readonly OutgoingItem[]): { request_id: number; outcome: ResourceFetchOutcome }[] {
  return items.flatMap((item) =>
    item.kind === "message" && item.message.type === "resource_fetch_finished" ? [{ request_id: item.message.request_id, outcome: item.message.outcome }] : [],
  );
}

/**
 * Рамки запроса. Каждая проходит через настоящие кодировщик и разборщик рамок: проверяем, что их
 * заголовок и размер годятся на проводе.
 */
function framesOf(items: readonly OutgoingItem[], requestId: number): { offset: number; payload: Uint8Array }[] {
  return items.flatMap((item) => {
    if (item.kind !== "frame" || item.header.kind !== "range_bytes" || item.header.request_id !== requestId) return [];
    const encoded = encodeDataFrame(item.header, item.payload);
    assert.ok(encoded.ok, "рамка кодируется");
    const decoded = decodeDataFrame(encoded.value);
    assert.ok(decoded.ok, "рамка разбирается");
    assert.deepEqual(decoded.value.header, item.header);
    return [{ offset: item.header.offset, payload: item.payload }];
  });
}

/** Рамки строго подряд от 0 и ≤ 1 МиБ; возвращает склеенное тело. */
function joinedBody(items: readonly OutgoingItem[], requestId: number): Uint8Array {
  const frames = framesOf(items, requestId);
  const total = frames.reduce((sum, frame) => sum + frame.payload.length, 0);
  const joined = new Uint8Array(total);
  let position = 0;
  for (const frame of frames) {
    assert.equal(frame.offset, position, "рамки подряд от 0");
    assert.ok(frame.payload.length >= 1 && frame.payload.length <= MAX_DATA_FRAME_PAYLOAD_BYTES);
    joined.set(frame.payload, position);
    position += frame.payload.length;
  }
  return joined;
}

function rangeOf(init: RangeFetchInit): [number, number] {
  const match = /^bytes=(\d+)-(\d+)$/.exec(init.headers["Range"] ?? "");
  assert.ok(match, `заголовок Range: ${String(init.headers["Range"])}`);
  return [Number(match[1]), Number(match[2])];
}

/** Сервер, отдающий `file` целиком на запрос без Range. */
function wholeServer(file: Uint8Array, options: { chunkSize?: number; url?: string; headers?: Record<string, string> } = {}) {
  const calls: { url: string; init: RangeFetchInit }[] = [];
  const fetch: RangeFetch = (url, init) => {
    calls.push({ url, init });
    return Promise.resolve(responseOf(streamOf(chunked(file, options.chunkSize ?? 64 * 1024), init.signal), { headers: options.headers ?? {}, url: options.url ?? url }));
  };
  return { fetch, calls };
}

describe("целое тело", () => {
  it("3 МиБ с неровными кусками уходят рамками ≤ 1 МиБ подряд от 0, итог равен числу байт", async () => {
    const file = patternOf(3 * MIB + 123);
    const server = wholeServer(file, { chunkSize: 700_000 });
    const { fetcher, desk, items } = setup(server.fetch);
    const handle = desk.issue(SEGMENT_URL, "media");
    assert.equal(handle, 0);
    fetcher.request(wholeRequest(7, 0));
    await settle();
    assert.deepEqual(joinedBody(items, 7), file);
    assert.deepEqual(finishes(items), [{ request_id: 7, outcome: { kind: "completed", size_bytes: file.length } }]);
    // Итог идёт после своих байт.
    assert.equal(items.at(-1)?.kind, "message");
    assert.equal(server.calls[0]?.url, SEGMENT_URL);
    assert.equal(server.calls[0]?.init.credentials, "include");
    assert.equal(server.calls[0]?.init.cache, "default");
    assert.equal(server.calls[0]?.init.headers["Range"], undefined);
  });

  it("пустое тело - completed 0 без рамок; короткое не-плейлист тело проходит как есть", async () => {
    const server = wholeServer(new Uint8Array(0));
    const first = setup(server.fetch);
    first.desk.issue(SEGMENT_URL, "media");
    first.fetcher.request(wholeRequest(1, 0));
    await settle();
    assert.equal(framesOf(first.items, 1).length, 0);
    assert.deepEqual(finishes(first.items), [{ request_id: 1, outcome: { kind: "completed", size_bytes: 0 } }]);

    const tiny = setup(wholeServer(new Uint8Array([1, 2, 3])).fetch);
    tiny.desk.issue(SEGMENT_URL, "media");
    tiny.fetcher.request(wholeRequest(2, 0));
    await settle();
    assert.deepEqual([...joinedBody(tiny.items, 2)], [1, 2, 3]);
    assert.deepEqual(finishes(tiny.items), [{ request_id: 2, outcome: { kind: "completed", size_bytes: 3 } }]);
  });

  it("плейлист качается без кэша браузера", async () => {
    const server = wholeServer(new TextEncoder().encode("#EXTM3U\n#EXT-X-ENDLIST\n"));
    const { fetcher, desk } = setup(server.fetch);
    desk.issue(PLAYLIST_URL, "playlist");
    fetcher.request(wholeRequest(1, 0));
    await settle();
    assert.equal(server.calls[0]?.init.cache, "no-store");
  });
});

describe("слайс", () => {
  it("206: рамки отсчитываются от 0 тела, а не от смещения слайса", async () => {
    const file = patternOf(5000);
    const fetch: RangeFetch = (_url, init) => {
      const [first, last] = rangeOf(init);
      return Promise.resolve(
        responseOf(streamOf(chunked(file.subarray(first, last + 1), 1000), init.signal), {
          status: 206,
          headers: { "content-range": `bytes ${first}-${last}/${file.length}` },
        }),
      );
    };
    const { fetcher, desk, items } = setup(fetch);
    desk.issue(SEGMENT_URL, "media");
    fetcher.request(sliceRequest(3, 0, 1200, 2000));
    await settle();
    assert.deepEqual(joinedBody(items, 3), file.subarray(1200, 3200));
    assert.deepEqual(finishes(items), [{ request_id: 3, outcome: { kind: "completed", size_bytes: 2000 } }]);
  });

  it("200 на запрос слайса: первые offset байт пропущены, отдано length", async () => {
    const file = patternOf(5000);
    const server = wholeServer(file, { chunkSize: 700 });
    const { fetcher, desk, items } = setup(server.fetch);
    desk.issue(SEGMENT_URL, "media");
    fetcher.request(sliceRequest(4, 0, 1234, 2000));
    await settle();
    assert.deepEqual(joinedBody(items, 4), file.subarray(1234, 3234));
    assert.deepEqual(finishes(items), [{ request_id: 4, outcome: { kind: "completed", size_bytes: 2000 } }]);
    assert.equal(server.calls[0]?.init.headers["Range"], "bytes=1234-3233");
  });

  it("тело короче слайса - completed с настоящим размером", async () => {
    const file = patternOf(1500);
    const server = wholeServer(file);
    const { fetcher, desk, items } = setup(server.fetch);
    desk.issue(SEGMENT_URL, "media");
    fetcher.request(sliceRequest(5, 0, 1000, 4000));
    await settle();
    assert.deepEqual(joinedBody(items, 5), file.subarray(1000));
    assert.deepEqual(finishes(items), [{ request_id: 5, outcome: { kind: "completed", size_bytes: 500 } }]);
  });

  it("206 с чужого места (Content-Range не с offset) - network_error без рамок", async () => {
    const fetch: RangeFetch = (_url, init) =>
      Promise.resolve(responseOf(streamOf([patternOf(100)], init.signal), { status: 206, headers: { "content-range": "bytes 0-99/1000" } }));
    const { fetcher, desk, items } = setup(fetch);
    desk.issue(SEGMENT_URL, "media");
    fetcher.request(sliceRequest(6, 0, 500, 100));
    await settle();
    assert.equal(framesOf(items, 6).length, 0);
    assert.deepEqual(finishes(items), [{ request_id: 6, outcome: { kind: "failed", failure: { kind: "network_error" } } }]);
  });
});

describe("исходы-ошибки", () => {
  it("too_large по Content-Length: тело не читается", async () => {
    const server = wholeServer(patternOf(1000), { headers: { "content-length": "1000" } });
    const { fetcher, desk, items } = setup(server.fetch);
    desk.issue(SEGMENT_URL, "media");
    fetcher.request(wholeRequest(1, 0, 999));
    await settle();
    assert.equal(framesOf(items, 1).length, 0);
    assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "too_large" } } }]);
  });

  it("too_large по потоку без длины; ровно max_bytes ещё проходит", async () => {
    const over = setup(wholeServer(patternOf(5000), { chunkSize: 1000 }).fetch);
    over.desk.issue(SEGMENT_URL, "media");
    over.fetcher.request(wholeRequest(1, 0, 4999));
    await settle();
    assert.deepEqual(finishes(over.items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "too_large" } } }]);

    const exact = setup(wholeServer(patternOf(5000), { chunkSize: 1000 }).fetch);
    exact.desk.issue(SEGMENT_URL, "media");
    exact.fetcher.request(wholeRequest(2, 0, 5000));
    await settle();
    assert.deepEqual(finishes(exact.items), [{ request_id: 2, outcome: { kind: "completed", size_bytes: 5000 } }]);
  });

  it("HTTP 404 и 503 - http_error со статусом и без рамок", async () => {
    for (const status of [404, 503, 304]) {
      const fetch: RangeFetch = () => Promise.resolve(responseOf(null, { status }));
      const { fetcher, desk, items } = setup(fetch);
      desk.issue(SEGMENT_URL, "media");
      fetcher.request(wholeRequest(1, 0));
      await settle();
      assert.equal(framesOf(items, 1).length, 0);
      assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "http_error", status } } }]);
    }
  });

  it("сбой сети до ответа и посреди тела - network_error", async () => {
    const dead = setup(() => Promise.reject(new TypeError("offline")));
    dead.desk.issue(SEGMENT_URL, "media");
    dead.fetcher.request(wholeRequest(1, 0));
    await settle();
    assert.deepEqual(finishes(dead.items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "network_error" } } }]);

    const broken = setup((_url, init) => Promise.resolve(responseOf(streamOf(chunked(patternOf(4000), 1000), init.signal, { failAfter: 2 }), {})));
    broken.desk.issue(SEGMENT_URL, "media");
    broken.fetcher.request(wholeRequest(2, 0));
    await settle();
    assert.deepEqual(finishes(broken.items), [{ request_id: 2, outcome: { kind: "failed", failure: { kind: "network_error" } } }]);
  });

  it("перенаправление на запрещённый адрес - refused, тело не отдано", async () => {
    const fetch: RangeFetch = (_url, init) => Promise.resolve(responseOf(streamOf([patternOf(100)], init.signal), { url: "http://192.168.1.1/admin" }));
    const { fetcher, desk, items } = setup(fetch);
    desk.issue(SEGMENT_URL, "media");
    fetcher.request(wholeRequest(1, 0));
    await settle();
    assert.equal(framesOf(items, 1).length, 0);
    assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "refused" } } }]);
  });

  it("неизвестный, вытесненный и «отказ» номерки - refused без обращения к сети", async () => {
    let networkCalls = 0;
    const fetch: RangeFetch = () => {
      networkCalls += 1;
      return Promise.reject(new TypeError("не должно вызываться"));
    };
    const { fetcher, desk, items } = setup(fetch);
    desk.issue(SEGMENT_URL, "media");
    desk.beginGeneration();
    fetcher.request(wholeRequest(1, 0)); // номерок прошлого поколения
    fetcher.request(wholeRequest(2, 99)); // никогда не выдавался
    fetcher.request(wholeRequest(3, 0xffff_ffff)); // «отказ»
    await settle();
    assert.equal(networkCalls, 0);
    const refused: ResourceFetchOutcome = { kind: "failed", failure: { kind: "refused" } };
    assert.deepEqual(finishes(items), [
      { request_id: 1, outcome: refused },
      { request_id: 2, outcome: refused },
      { request_id: 3, outcome: refused },
    ]);
  });

  it("девятый одновременный запрос - refused, первые восемь доезжают", async () => {
    const releases: (() => void)[] = [];
    const fetch: RangeFetch = (_url, init) =>
      new Promise((resolve) => {
        releases.push(() => resolve(responseOf(streamOf([patternOf(10)], init.signal), {})));
      });
    const { fetcher, desk, items } = setup(fetch);
    for (let index = 0; index <= MAX_CONCURRENT_RESOURCE_FETCHES; index += 1) desk.issue(`https://cdn.example.net/s${index}.ts`, "media");
    for (let index = 0; index <= MAX_CONCURRENT_RESOURCE_FETCHES; index += 1) fetcher.request(wholeRequest(100 + index, index));
    await settle();
    assert.deepEqual(finishes(items), [{ request_id: 100 + MAX_CONCURRENT_RESOURCE_FETCHES, outcome: { kind: "failed", failure: { kind: "refused" } } }]);
    for (const release of releases) release();
    await settle();
    const done = finishes(items).filter((entry) => entry.outcome.kind === "completed");
    assert.equal(done.length, MAX_CONCURRENT_RESOURCE_FETCHES);
    // Слот освободился: следующий запрос принимается.
    fetcher.request(wholeRequest(200, MAX_CONCURRENT_RESOURCE_FETCHES));
    await settle();
    releases.at(-1)?.();
    await settle();
    assert.equal(finishes(items).at(-1)?.request_id, 200);
    assert.equal(finishes(items).at(-1)?.outcome.kind, "completed");
  });
});

describe("плейлисты", () => {
  const MEDIA_PLAYLIST = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nseg1.ts\n#EXTINF:6,\nhttps://other.example.org/seg2.ts\n#EXT-X-ENDLIST\n";

  it("плейлист переписан по конечному адресу, номерки выданы в стойку, итог - размер переписанного", async () => {
    const encoded = new TextEncoder().encode(MEDIA_PLAYLIST);
    const fetch: RangeFetch = (_url, init) => Promise.resolve(responseOf(streamOf(chunked(encoded, 5), init.signal), { url: "https://redirected.example.net/v/media.m3u8" }));
    const { fetcher, desk, items } = setup(fetch);
    desk.issue(PLAYLIST_URL, "playlist");
    fetcher.request(wholeRequest(1, 0));
    await settle();
    const expected = `#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n${formatResourceAddress(1)}\n#EXTINF:6,\n${formatResourceAddress(2)}\n#EXT-X-ENDLIST\n`;
    assert.equal(new TextDecoder().decode(joinedBody(items, 1)), expected);
    assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "completed", size_bytes: new TextEncoder().encode(expected).length } }]);
    assert.deepEqual(desk.lookup(1), { url: "https://redirected.example.net/v/seg1.ts", kind: "media" });
    assert.equal(desk.lookup(2)?.url, "https://other.example.org/seg2.ts");
  });

  it("плейлист с BOM определяется, даже если BOM и метка пришли разными кусками", async () => {
    const encoded = new TextEncoder().encode(`﻿${MEDIA_PLAYLIST}`);
    const { fetcher, desk, items } = setup((_url, init) => Promise.resolve(responseOf(streamOf(chunked(encoded, 2), init.signal), {})));
    desk.issue(PLAYLIST_URL, "playlist");
    fetcher.request(wholeRequest(1, 0));
    await settle();
    assert.ok(new TextDecoder().decode(joinedBody(items, 1)).startsWith("#EXTM3U\n"));
    assert.ok(!new TextDecoder().decode(joinedBody(items, 1)).includes("seg1.ts"));
  });

  it("тело, не начинающееся с #EXTM3U, проходит нетронутым", async () => {
    const html = new TextEncoder().encode("<html>https://secret.example/</html>");
    const { fetcher, desk, items } = setup((_url, init) => Promise.resolve(responseOf(streamOf(chunked(html, 4), init.signal), {})));
    desk.issue(SEGMENT_URL, "media");
    fetcher.request(wholeRequest(1, 0));
    await settle();
    assert.deepEqual(joinedBody(items, 1), html);
  });

  it("номерок-плейлист с телом не-плейлистом (HTML, UTF-16 BOM, 17 пробелов впереди) - refused, сырые байты не уходят", async () => {
    const bodies: Uint8Array[] = [
      new TextEncoder().encode("<html>https://secret.example/a.ts</html>"),
      new Uint8Array([0xff, 0xfe, ...new TextEncoder().encode("#EXTM3U\nhttps://secret.example/a.ts\n")]),
      new TextEncoder().encode(`${" ".repeat(17)}#EXTM3U\nhttps://secret.example/a.ts\n`),
      new TextEncoder().encode("ok"),
    ];
    for (const body of bodies) {
      const { fetcher, desk, items } = setup((_url, init) => Promise.resolve(responseOf(streamOf(chunked(body, 7), init.signal), {})));
      desk.issue(PLAYLIST_URL, "playlist");
      fetcher.request(wholeRequest(1, 0));
      await settle();
      assert.equal(framesOf(items, 1).length, 0, "ни одной рамки");
      assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "refused" } } }]);
    }
  });

  it("плейлист с пустой строкой и BOM впереди переписывается; media-номерок, оказавшийся плейлистом, тоже", async () => {
    const text = new TextEncoder().encode(`\uFEFF\r\n  \n#EXTM3U\n#EXTINF:6,secret title\nseg1.ts\n`);
    for (const kind of ["playlist", "media"] as const) {
      const { fetcher, desk, items } = setup((_url, init) => Promise.resolve(responseOf(streamOf(chunked(text, 3), init.signal), {})));
      desk.issue(PLAYLIST_URL, kind);
      fetcher.request(wholeRequest(1, 0));
      await settle();
      const body = new TextDecoder().decode(joinedBody(items, 1));
      assert.equal(body, `#EXTM3U\n#EXTINF:6,\n${formatResourceAddress(1)}\n`);
    }
  });

  it("кусок (slice) номерка-плейлиста - refused без обращения к сети", async () => {
    let networkCalls = 0;
    const { fetcher, desk, items } = setup(() => {
      networkCalls += 1;
      return Promise.reject(new TypeError("не должно вызываться"));
    });
    desk.issue(PLAYLIST_URL, "playlist");
    fetcher.request(sliceRequest(1, 0, 0, 100));
    await settle();
    assert.equal(networkCalls, 0);
    assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "refused" } } }]);
  });

  it("плейлист не в UTF-8 - refused без рамок", async () => {
    const broken = new Uint8Array([...new TextEncoder().encode("#EXTM3U\n#EXTINF:1,\n"), 0xff, 0xfe, 0x0a]);
    const { fetcher, desk, items } = setup((_url, init) => Promise.resolve(responseOf(streamOf([broken], init.signal), {})));
    desk.issue(PLAYLIST_URL, "playlist");
    fetcher.request(wholeRequest(1, 0));
    await settle();
    assert.equal(framesOf(items, 1).length, 0);
    assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "refused" } } }]);
  });

  it("плейлист, который не переписать (IMPORT), - refused", async () => {
    const text = new TextEncoder().encode('#EXTM3U\n#EXT-X-DEFINE:IMPORT="x"\n');
    const { fetcher, desk, items } = setup((_url, init) => Promise.resolve(responseOf(streamOf([text], init.signal), {})));
    desk.issue(PLAYLIST_URL, "playlist");
    fetcher.request(wholeRequest(1, 0));
    await settle();
    assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "refused" } } }]);
  });

  it("переписанный плейлист больше max_bytes - too_large", async () => {
    const encoded = new TextEncoder().encode("#EXTM3U\n#EXTINF:1,\na.ts\n");
    const { fetcher, desk, items } = setup((_url, init) => Promise.resolve(responseOf(streamOf([encoded], init.signal), {})));
    desk.issue(PLAYLIST_URL, "playlist");
    // Исходный плейлист (24 байта) влезает, а с адресом-заглушкой нет.
    fetcher.request(wholeRequest(1, 0, encoded.length));
    await settle();
    assert.equal(framesOf(items, 1).length, 0);
    assert.deepEqual(finishes(items), [{ request_id: 1, outcome: { kind: "failed", failure: { kind: "too_large" } } }]);
  });
});

describe("обратное давление и конец поколения", () => {
  it("пока очередь сеанса выше порога, рамки не уходят; после опустошения идут", async () => {
    const file = patternOf(3 * 64 * 1024);
    const server = wholeServer(file);
    const { fetcher, desk, items, queue, clock } = setup(server.fetch);
    desk.issue(SEGMENT_URL, "media");
    queue.bytes = RANGE_FEED_HIGH_WATER_BYTES + 1;
    fetcher.request(wholeRequest(1, 0));
    await settle();
    clock.advance(RANGE_FEED_DRAIN_POLL_MS * 5);
    await settle();
    assert.equal(framesOf(items, 1).length, 0, "очередь полна - тело не читается");
    assert.equal(finishes(items).length, 0);
    queue.bytes = 0;
    clock.advance(RANGE_FEED_DRAIN_POLL_MS);
    await settle();
    assert.deepEqual(joinedBody(items, 1), file);
    assert.equal(finishes(items).length, 1);
  });

  it("dispose: больше ни рамок, ни итогов; сеть оборвана; новые запросы игнорируются", async () => {
    let aborted = false;
    const fetch: RangeFetch = (_url, init) => {
      init.signal.addEventListener("abort", () => {
        aborted = true;
      });
      return Promise.resolve(responseOf(streamOf([patternOf(1000)], init.signal), {}));
    };
    const { fetcher, desk, items, queue, clock } = setup(fetch);
    desk.issue(SEGMENT_URL, "media");
    queue.bytes = RANGE_FEED_HIGH_WATER_BYTES + 1; // тело застряло на давлении
    fetcher.request(wholeRequest(1, 0));
    await settle();
    fetcher.dispose();
    fetcher.dispose(); // повтор безвреден
    queue.bytes = 0;
    clock.advance(RANGE_FEED_DRAIN_POLL_MS * 3);
    await settle();
    fetcher.request(wholeRequest(2, 0));
    await settle();
    assert.equal(aborted, true);
    assert.deepEqual(items, []);
  });

  it("на каждый запрос ровно один итог при смеси исходов", async () => {
    const fetch: RangeFetch = (url, init) => {
      if (url.endsWith("/bad.ts")) return Promise.resolve(responseOf(null, { status: 500 }));
      if (url.endsWith("/boom.ts")) return Promise.reject(new TypeError("offline"));
      return Promise.resolve(responseOf(streamOf(chunked(patternOf(2000), 300), init.signal), {}));
    };
    const { fetcher, desk, items } = setup(fetch);
    desk.issue("https://cdn.example.net/ok.ts", "media");
    desk.issue("https://cdn.example.net/bad.ts", "media");
    desk.issue("https://cdn.example.net/boom.ts", "media");
    for (const [requestId, handle] of [[1, 0], [2, 1], [3, 2], [4, 50]] as const) fetcher.request(wholeRequest(requestId, handle));
    await settle();
    const ids = finishes(items).map((entry) => entry.request_id).sort();
    assert.deepEqual(ids, [1, 2, 3, 4]);
  });
});
