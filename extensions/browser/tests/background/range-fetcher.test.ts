// Загрузчик диапазонов `<video src>` (сессия 09b): рамки вида 2 и итоги `range_fetch_finished`
// на настоящих `Response`/`ReadableStream` Node — порядок, смещения, все исходы, вытеснение,
// конец сеанса и обратное давление.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  createRangeFetcher,
  MAX_SUBREQUESTS_PER_RANGE,
  RANGE_FEED_HIGH_WATER_BYTES,
  type RangeFetch,
  type RangeFetchInit,
  type RangeFetcher,
} from "../../src/background/handoff/range-fetcher.ts";
import type { OutgoingItem } from "../../src/background/handoff/page-event-translator.ts";
import { MAX_DATA_FRAME_PAYLOAD_BYTES, type RangeFetchOutcome } from "../../src/protocol/index.ts";
import { createFakeClock, flushPromises } from "./support.ts";

const URL_OF_FILE = "https://cdn.example/movie.mp4";
const MIB = 1024 * 1024;

/** Файл с узнаваемыми байтами: байт по смещению i = i mod 251. */
function fileOf(size: number): Uint8Array<ArrayBuffer> {
  return new Uint8Array(size).map((_, index) => index % 251);
}

/** `Range: bytes=a-b` запроса → [a, b]. */
function rangeOf(init: RangeFetchInit): [number, number] {
  const match = /^bytes=(\d+)-(\d+)$/.exec(init.headers["Range"] ?? "");
  assert.ok(match, `Range header: ${String(init.headers["Range"])}`);
  return [Number(match[1]), Number(match[2])];
}

/** Тело из кусков; поток рвётся при отмене запроса (как у настоящего fetch). */
function bodyOf(chunks: readonly Uint8Array[], signal: AbortSignal, options: { failAfter?: number; hang?: boolean } = {}) {
  let index = 0;
  const state = { cancelled: false };
  const stream = new ReadableStream<Uint8Array>({
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
      else if (options.hang === true) return new Promise<void>(() => {});
      else controller.close();
      return undefined;
    },
    cancel() {
      state.cancelled = true;
    },
  });
  return { stream, state };
}

/** Режет байты на куски заданного размера. */
function chunked(bytes: Uint8Array, chunkSize: number): Uint8Array[] {
  const chunks: Uint8Array[] = [];
  for (let start = 0; start < bytes.length; start += chunkSize) chunks.push(bytes.slice(start, start + chunkSize));
  return chunks;
}

interface ServerOptions {
  /** Отдавать полный размер (`false` — `*`). */
  readonly announceSize?: boolean;
  /** Сервер режет каждый ответ до стольких байт. */
  readonly maxResponseBytes?: number;
  readonly chunkSize?: number;
  readonly etag?: (call: number) => string | null;
}

/** «Сервер» с поддержкой Range над файлом. */
function rangeServer(file: Uint8Array, options: ServerOptions = {}) {
  const calls: RangeFetchInit[] = [];
  const fetch: RangeFetch = (_url, init) => {
    calls.push(init);
    const [first, requestedLast] = rangeOf(init);
    if (first >= file.length) return Promise.resolve(new Response(null, { status: 416, headers: { "content-range": `bytes */${file.length}` } }));
    const cap = options.maxResponseBytes ?? Number.MAX_SAFE_INTEGER;
    const last = Math.min(requestedLast, file.length - 1, first + cap - 1);
    const total = options.announceSize === false ? "*" : String(file.length);
    const headers: Record<string, string> = { "content-range": `bytes ${first}-${last}/${total}`, "content-length": String(last - first + 1) };
    const etag = options.etag?.(calls.length) ?? '"v1"';
    if (etag !== null) headers["etag"] = etag;
    const { stream } = bodyOf(chunked(file.subarray(first, last + 1), options.chunkSize ?? 64 * 1024), init.signal);
    return Promise.resolve(new Response(stream, { status: 206, headers }));
  };
  return { fetch, calls };
}

/** Очередь сеанса: всё, что ушло, и управляемый «объём в очереди». */
function recordingSink() {
  const items: OutgoingItem[] = [];
  const state = { queuedBytes: 0 };
  return { items, state, sink: { enqueue: (item: OutgoingItem) => void items.push(item), queuedBytes: () => state.queuedBytes } };
}

function setupFetcher(fetch: RangeFetch) {
  const clock = createFakeClock();
  const recorded = recordingSink();
  const fetcher: RangeFetcher = createRangeFetcher({ url: URL_OF_FILE, fetch, sink: recorded.sink, scheduler: clock.scheduler });
  return { clock, fetcher, ...recorded };
}

/** Рамки запроса: смещение и длина каждой. */
function framesOf(items: readonly OutgoingItem[], requestId: number): { offset: number; length: number; payload: Uint8Array }[] {
  return items.flatMap((item) =>
    item.kind === "frame" && item.header.kind === "range_bytes" && item.header.request_id === requestId
      ? [{ offset: item.header.offset, length: item.payload.length, payload: item.payload }]
      : [],
  );
}

/** Итоги запросов по порядку. */
function finishes(items: readonly OutgoingItem[]): { request_id: number; outcome: RangeFetchOutcome }[] {
  return items.flatMap((item) =>
    item.kind === "message" && item.message.type === "range_fetch_finished"
      ? [{ request_id: item.message.request_id, outcome: item.message.outcome }]
      : [],
  );
}

/** Рамки идут строго подряд от `offset` и складываются ровно в эти байты файла. */
function assertContiguous(items: readonly OutgoingItem[], requestId: number, offset: number, expected: Uint8Array): void {
  const frames = framesOf(items, requestId);
  let position = offset;
  const joined = new Uint8Array(expected.length);
  for (const frame of frames) {
    assert.equal(frame.offset, position, "рамки подряд");
    assert.ok(frame.length >= 1 && frame.length <= MAX_DATA_FRAME_PAYLOAD_BYTES, "рамка 1 байт…1 МиБ");
    joined.set(frame.payload, position - offset);
    position += frame.length;
  }
  assert.equal(position - offset, expected.length, "доставлено ровно столько байт");
  assert.deepEqual(joined, expected);
}

/** Дождаться, пока загрузчик доработает все ждущие обещания. */
async function settle(): Promise<void> {
  for (let round = 0; round < 20; round += 1) await flushPromises();
}

const completedKnown = (size: number): RangeFetchOutcome => ({ kind: "completed", resource_size: { kind: "known", size_bytes: size } });
const completedUnknown: RangeFetchOutcome = { kind: "completed", resource_size: { kind: "unknown" } };
const failure = (kind: "network_error" | "http_error" | "resource_changed" | "out_of_range"): RangeFetchOutcome => ({ kind: "failed", failure: kind });

describe("загрузчик диапазонов: обычный путь", () => {
  it("206: рамки ≤ 1 МиБ подряд от смещения, итог после последней рамки, размер известен; запрос — с куками и Range", async () => {
    const file = fileOf(5 * MIB);
    const server = rangeServer(file, { chunkSize: 700 * 1024 });
    const world = setupFetcher(server.fetch);
    world.fetcher.request({ request_id: 7, offset: 100, length: 3 * MIB });
    await settle();
    assertContiguous(world.items, 7, 100, file.subarray(100, 100 + 3 * MIB));
    assert.deepEqual(finishes(world.items), [{ request_id: 7, outcome: completedKnown(file.length) }]);
    const last = world.items.at(-1);
    assert.ok(last?.kind === "message" && last.message.type === "range_fetch_finished", "итог не обгоняет свои байты");
    assert.equal(server.calls.length, 1);
    assert.equal(server.calls[0]?.headers["Range"], `bytes=100-${100 + 3 * MIB - 1}`);
    assert.equal(server.calls[0]?.credentials, "include");
    assert.equal(server.calls[0]?.cache, "no-store");
  });

  it("кусок сети больше 1 МиБ режется на рамки по 1 МиБ", async () => {
    const file = fileOf(3 * MIB);
    const world = setupFetcher(rangeServer(file, { chunkSize: Math.floor(2.5 * MIB) }).fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 3 * MIB });
    await settle();
    assert.deepEqual(framesOf(world.items, 1).map((frame) => frame.length), [MIB, MIB, MIB / 2, MIB / 2]);
  });

  it("короткое чтение у конца файла известного размера — completed, рамки кончаются ровно на размере", async () => {
    const file = fileOf(1000);
    const world = setupFetcher(rangeServer(file).fetch);
    world.fetcher.request({ request_id: 1, offset: 900, length: 500 });
    await settle();
    assertContiguous(world.items, 1, 900, file.subarray(900));
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: completedKnown(1000) }]);
  });

  it("сервер режет диапазоны — загрузчик докачивает остаток подзапросами с того же места", async () => {
    const file = fileOf(1000);
    const server = rangeServer(file, { maxResponseBytes: 100 });
    const world = setupFetcher(server.fetch);
    world.fetcher.request({ request_id: 3, offset: 50, length: 300 });
    await settle();
    assertContiguous(world.items, 3, 50, file.subarray(50, 350));
    assert.deepEqual(server.calls.map((init) => init.headers["Range"]), ["bytes=50-349", "bytes=150-349", "bytes=250-349"]);
    assert.deepEqual(finishes(world.items), [{ request_id: 3, outcome: completedKnown(1000) }]);
  });

  it("сервер отдаёт по 0 байт… — не крутимся вечно: после предела подзапросов network_error", async () => {
    let calls = 0;
    const fetch: RangeFetch = (_url, init) => {
      calls += 1;
      const [first] = rangeOf(init);
      // Обещает 1 байт, но отдаёт тело без байтов.
      return Promise.resolve(new Response(new Uint8Array(0), { status: 206, headers: { "content-range": `bytes ${first}-${first}/*` } }));
    };
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 10 });
    await settle();
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: failure("network_error") }]);
    assert.ok(calls <= MAX_SUBREQUESTS_PER_RANGE);
  });
});

describe("загрузчик диапазонов: размер неизвестен", () => {
  it("`*`: полный запрос — completed unknown; продолжение за концом — короткий ответ, затем 416 = конец файла", async () => {
    const file = fileOf(1000);
    const server = rangeServer(file, { announceSize: false });
    const world = setupFetcher(server.fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 600 });
    await settle();
    world.fetcher.request({ request_id: 2, offset: 600, length: 600 });
    await settle();
    assertContiguous(world.items, 2, 600, file.subarray(600));
    assert.deepEqual(finishes(world.items), [
      { request_id: 1, outcome: completedUnknown },
      { request_id: 2, outcome: completedUnknown },
    ]);
    assert.deepEqual(server.calls.map((init) => init.headers["Range"]), ["bytes=0-599", "bytes=600-1199", "bytes=1000-1199"]);
  });

  it("вид размера не переключается: первый ответ без размера, позже сервер его назвал — всё равно unknown", async () => {
    const file = fileOf(1000);
    let call = 0;
    const fetch: RangeFetch = (_url, init) => {
      call += 1;
      const [first, last] = rangeOf(init);
      const total = call === 1 ? "*" : "1000";
      const body = file.slice(first, last + 1);
      return Promise.resolve(new Response(body, { status: 206, headers: { "content-range": `bytes ${first}-${last}/${total}` } }));
    };
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 100 });
    await settle();
    world.fetcher.request({ request_id: 2, offset: 100, length: 100 });
    await settle();
    assert.deepEqual(finishes(world.items).map((finish) => finish.outcome), [completedUnknown, completedUnknown]);
  });
});

describe("загрузчик диапазонов: сервер без Range", () => {
  it("200 на запрос с 0: читается ровно `length` байт, тело отменяется, размер из Content-Length", async () => {
    const file = fileOf(100_000);
    let cancelled: { cancelled: boolean } | undefined;
    const fetch: RangeFetch = (_url, init) => {
      const body = bodyOf(chunked(file, 4096), init.signal);
      cancelled = body.state;
      return Promise.resolve(new Response(body.stream, { status: 200, headers: { "content-length": String(file.length) } }));
    };
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 10_000 });
    await settle();
    assertContiguous(world.items, 1, 0, file.subarray(0, 10_000));
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: completedKnown(file.length) }]);
    assert.equal(cancelled?.cancelled, true, "остаток файла не качается");
  });

  it("200 со смещения > 0 — http_error, ни одной рамки", async () => {
    const fetch: RangeFetch = () => Promise.resolve(new Response(fileOf(100), { status: 200 }));
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 10, length: 10 });
    await settle();
    assert.deepEqual(framesOf(world.items, 1), []);
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: failure("http_error") }]);
  });

  it("200 без длины, тело кончилось раньше запроса — это конец файла неизвестного размера", async () => {
    const fetch: RangeFetch = () => Promise.resolve(new Response(fileOf(50), { status: 200 }));
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 100 });
    await settle();
    assertContiguous(world.items, 1, 0, fileOf(50));
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: completedUnknown }]);
  });

  it("206 не с того смещения — http_error (байты нельзя поставить по месту)", async () => {
    const fetch: RangeFetch = () =>
      Promise.resolve(new Response(fileOf(10), { status: 206, headers: { "content-range": "bytes 0-9/100" } }));
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 5, length: 10 });
    await settle();
    assert.deepEqual(framesOf(world.items, 1), []);
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: failure("http_error") }]);
  });
});

describe("загрузчик диапазонов: сбои", () => {
  it("416 — out_of_range; 503 — http_error", async () => {
    const statuses = [416, 503];
    const fetch: RangeFetch = () => Promise.resolve(new Response(null, { status: statuses.shift() ?? 500 }));
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 10 });
    await settle();
    world.fetcher.request({ request_id: 2, offset: 0, length: 10 });
    await settle();
    assert.deepEqual(finishes(world.items), [
      { request_id: 1, outcome: failure("out_of_range") },
      { request_id: 2, outcome: failure("http_error") },
    ]);
  });

  it("fetch отвергнут (сеть) — network_error без рамок", async () => {
    const world = setupFetcher(() => Promise.reject(new TypeError("Failed to fetch")));
    world.fetcher.request({ request_id: 1, offset: 0, length: 10 });
    await settle();
    assert.deepEqual(world.items.filter((item) => item.kind === "frame"), []);
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: failure("network_error") }]);
  });

  it("обрыв посреди тела — уже отданные рамки в силе, итог network_error после них", async () => {
    const file = fileOf(10_000);
    const fetch: RangeFetch = (_url, init) => {
      const { stream } = bodyOf(chunked(file, 1000), init.signal, { failAfter: 3 });
      return Promise.resolve(new Response(stream, { status: 206, headers: { "content-range": "bytes 0-9999/10000" } }));
    };
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 10_000 });
    await settle();
    assertContiguous(world.items, 1, 0, file.subarray(0, 3000));
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: failure("network_error") }]);
    assert.equal(world.items.at(-1)?.kind, "message", "после итога рамок нет");
  });

  it("тело короче обещанного и это не конец файла — network_error (плеер повторит с первого недостающего)", async () => {
    const fetch: RangeFetch = () =>
      Promise.resolve(new Response(fileOf(500), { status: 206, headers: { "content-range": "bytes 0-999/5000" } }));
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 1000 });
    await settle();
    assert.equal(framesOf(world.items, 1).reduce((sum, frame) => sum + frame.length, 0), 500);
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: failure("network_error") }]);
  });

  it("файл сменился (другой ETag) — resource_changed; другой полный размер — тоже", async () => {
    const file = fileOf(1000);
    const byEtag = setupFetcher(rangeServer(file, { etag: (call) => (call === 1 ? '"v1"' : '"v2"') }).fetch);
    byEtag.fetcher.request({ request_id: 1, offset: 0, length: 100 });
    await settle();
    byEtag.fetcher.request({ request_id: 2, offset: 100, length: 100 });
    await settle();
    assert.deepEqual(finishes(byEtag.items).map((finish) => finish.outcome), [completedKnown(1000), failure("resource_changed")]);
    assert.deepEqual(framesOf(byEtag.items, 2), [], "байты нового файла не отдаются");

    let call = 0;
    const bySize = setupFetcher((_url, init) => {
      call += 1;
      const [first, last] = rangeOf(init);
      const total = call === 1 ? 1000 : 2000;
      return Promise.resolve(new Response(file.slice(first, last + 1), { status: 206, headers: { "content-range": `bytes ${first}-${last}/${total}` } }));
    });
    bySize.fetcher.request({ request_id: 1, offset: 0, length: 100 });
    await settle();
    bySize.fetcher.request({ request_id: 2, offset: 100, length: 100 });
    await settle();
    assert.deepEqual(finishes(bySize.items).map((finish) => finish.outcome), [completedKnown(1000), failure("resource_changed")]);
  });

  it("сервер без ETag во втором ответе — это не смена файла", async () => {
    const file = fileOf(1000);
    const world = setupFetcher(rangeServer(file, { etag: (call) => (call === 1 ? '"v1"' : null) }).fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 100 });
    await settle();
    world.fetcher.request({ request_id: 2, offset: 100, length: 100 });
    await settle();
    assert.deepEqual(finishes(world.items).map((finish) => finish.outcome), [completedKnown(1000), completedKnown(1000)]);
  });

  it("размер известен, запрос целиком за концом — out_of_range без сети", async () => {
    const file = fileOf(1000);
    const server = rangeServer(file);
    const world = setupFetcher(server.fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 1000 });
    await settle();
    world.fetcher.request({ request_id: 2, offset: 1000, length: 10 });
    await settle();
    assert.deepEqual(finishes(world.items)[1], { request_id: 2, outcome: failure("out_of_range") });
    assert.equal(server.calls.length, 1);
  });
});

describe("загрузчик диапазонов: очередь, вытеснение и конец", () => {
  it("новый запрос вытесняет текущий: тот кончается network_error после своих рамок, ждущий — без сети", async () => {
    const file = fileOf(10_000);
    const calls: [number, number][] = [];
    const signals: AbortSignal[] = [];
    const fetch: RangeFetch = (_url, init) => {
      const range = rangeOf(init);
      calls.push(range);
      signals.push(init.signal);
      const [first, last] = range;
      // Первый запрос: один кусок и «повисшая» сеть.
      const hang = calls.length === 1;
      const { stream } = bodyOf(hang ? [file.slice(first, first + 100)] : [file.slice(first, last + 1)], init.signal, { hang });
      return Promise.resolve(new Response(stream, { status: 206, headers: { "content-range": `bytes ${first}-${last}/10000` } }));
    };
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 1000 });
    await settle();
    world.fetcher.request({ request_id: 2, offset: 5000, length: 100 });
    world.fetcher.request({ request_id: 3, offset: 8000, length: 100 });
    await settle();
    assert.equal(signals[0]?.aborted, true, "текущий fetch оборван");
    assert.deepEqual(finishes(world.items), [
      { request_id: 1, outcome: failure("network_error") },
      { request_id: 2, outcome: failure("network_error") },
      { request_id: 3, outcome: completedKnown(10_000) },
    ]);
    assert.deepEqual(calls, [[0, 999], [8000, 8099]], "вытесненный ждущий запрос в сеть не ходил");
    assertContiguous(world.items, 1, 0, file.subarray(0, 100));
    assertContiguous(world.items, 3, 8000, file.subarray(8000, 8100));
    // Ни одной рамки запроса после его итога.
    const finishIndex = world.items.findIndex((item) => item.kind === "message" && item.message.type === "range_fetch_finished");
    assert.ok(world.items.slice(finishIndex).every((item) => !(item.kind === "frame" && item.header.kind === "range_bytes" && item.header.request_id === 1)));
  });

  it("конец сеанса: текущий fetch оборван, после dispose ни рамки, ни итога", async () => {
    const signals: AbortSignal[] = [];
    const fetch: RangeFetch = (_url, init) => {
      signals.push(init.signal);
      const { stream } = bodyOf([fileOf(100)], init.signal, { hang: true });
      return Promise.resolve(new Response(stream, { status: 206, headers: { "content-range": "bytes 0-999/1000" } }));
    };
    const world = setupFetcher(fetch);
    world.fetcher.request({ request_id: 1, offset: 0, length: 1000 });
    await settle();
    const before = world.items.length;
    world.fetcher.dispose();
    world.fetcher.request({ request_id: 2, offset: 0, length: 10 });
    await settle();
    assert.equal(signals[0]?.aborted, true);
    assert.equal(world.items.length, before, "после конца сеанса ничего не уходит");
    assert.equal(signals.length, 1, "новые запросы после конца в сеть не идут");
  });

  it("конец сеанса, пока fetch ещё не ответил: ответ пришёл позже — ничего не уходит", async () => {
    let respond: (response: Response) => void = () => {};
    const world = setupFetcher(() => new Promise((resolve) => (respond = resolve)));
    world.fetcher.request({ request_id: 1, offset: 0, length: 10 });
    await settle();
    world.fetcher.dispose();
    respond(new Response(fileOf(10), { status: 206, headers: { "content-range": "bytes 0-9/10" } }));
    await settle();
    assert.deepEqual(world.items, []);
  });

  it("обратное давление: очередь сеанса выше порога — тело не читается, пока она не опустится", async () => {
    const file = fileOf(1000);
    const world = setupFetcher(rangeServer(file, { chunkSize: 100 }).fetch);
    world.state.queuedBytes = RANGE_FEED_HIGH_WATER_BYTES + 1;
    world.fetcher.request({ request_id: 1, offset: 0, length: 1000 });
    await settle();
    world.clock.advance(1_000);
    await settle();
    assert.deepEqual(world.items, [], "пока очередь полна, байты не читаются");
    world.state.queuedBytes = 0;
    world.clock.advance(100);
    await settle();
    assertContiguous(world.items, 1, 0, file);
    assert.deepEqual(finishes(world.items), [{ request_id: 1, outcome: completedKnown(1000) }]);
  });
});
