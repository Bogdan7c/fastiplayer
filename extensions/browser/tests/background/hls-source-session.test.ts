// Нативный HLS через весь фон (сессия 10a2, пакет P3): `hls_source` страницы -> поколение HLS на проводе ->
// `fetch_resource` плеера по номеркам -> фон качает и отдаёт ПЕРЕПИСАННЫЕ плейлисты и байты сегментов ->
// `resource_fetch_finished`. Проверяются приватность (настоящих адресов на проводе нет), заслон локальных
// адресов, версия протокола, смена поколения и конец сеанса.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { RangeFetch, RangeFetchInit } from "../../src/background/handoff/range-fetcher.ts";
import {
  decodeDataFrame,
  formatResourceAddress,
  parseResourceAddress,
  PROTOCOL_VERSION_1_0,
  SESSION_CLOSE_CODES,
  type PageSessionMessage,
  type ResourceFetchOutcome,
} from "../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { REFUSED_RESOURCE_HANDLE } from "../../src/background/handoff/hls/limits.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { flushPromises, grantFor, type FakeWebSocket } from "./support.ts";

const REAL_HOST = "cdn.secret-host.example";
const ROOT_URL = `https://${REAL_HOST}/vod/master.m3u8`;
const CHILD_URL = `https://${REAL_HOST}/vod/low/index.m3u8`;
const SEGMENT_URL = `https://${REAL_HOST}/vod/low/seg0.ts`;
const LOCAL_SEGMENT_URL = "http://192.168.1.10/seg-local.ts";
const PLAYING_HLS: VideoCandidate = {
  document: "doc-7f3a",
  element: 4,
  media: { kind: "hls" },
  playing: true,
  visible_area_px: 200_000,
  ...DEFAULT_CANDIDATE_FACTS,
  file_host: REAL_HOST,
};

const MASTER_TEXT = ["#EXTM3U", "#EXT-X-STREAM-INF:BANDWIDTH=800000", "low/index.m3u8", ""].join("\n");
const CHILD_TEXT = ["#EXTM3U", "#EXT-X-TARGETDURATION:6", "#EXTINF:6,", "seg0.ts", "#EXTINF:6,", LOCAL_SEGMENT_URL, "#EXT-X-ENDLIST", ""].join("\n");
/** Сегмент: байты с узором, длиннее нескольких кусков. */
const SEGMENT_BYTES = new Uint8Array(70_000).map((_, index) => (index * 7) % 251);

/** Сервер: тело по адресу; `Range` честно обслуживается (206). Все обращения записываются. */
function hlsServer(options: { hang?: boolean } = {}) {
  const bodies = new Map<string, Uint8Array>([
    [ROOT_URL, new TextEncoder().encode(MASTER_TEXT)],
    [CHILD_URL, new TextEncoder().encode(CHILD_TEXT)],
    [SEGMENT_URL, SEGMENT_BYTES],
  ]);
  const calls: { url: string; init: RangeFetchInit }[] = [];
  const fetch: RangeFetch = (url, init) => {
    calls.push({ url, init });
    const body = bodies.get(url);
    if (body === undefined) return Promise.resolve(new Response(null, { status: 404 }));
    const range = /^bytes=(\d+)-(\d+)$/.exec(init.headers["Range"] ?? "");
    const first = range === null ? 0 : Number(range[1]);
    const last = range === null ? body.length - 1 : Math.min(Number(range[2]), body.length - 1);
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        init.signal.addEventListener("abort", () => controller.error(new DOMException("aborted", "AbortError")));
        if (options.hang === true) return;
        controller.enqueue(body.slice(first, last + 1));
        controller.close();
      },
    });
    const headers = range === null ? {} : { "content-range": `bytes ${first}-${last}/${body.length}` };
    const response = new Response(stream, { status: range === null ? 200 : 206, headers });
    // Настоящий `fetch` всегда сообщает конечный адрес; от него переписчик считает относительные ссылки.
    Object.defineProperty(response, "url", { value: url });
    return Promise.resolve(response);
  };
  return { fetch, calls, callsTo: (url: string) => calls.filter((call) => call.url === url) };
}

async function settle(): Promise<void> {
  for (let round = 0; round < 30; round += 1) await flushPromises();
}

function hlsWorld(fetch: RangeFetch): World {
  return setup([{ frameId: 0, result: [PLAYING_HLS] }], fetch);
}

function deliverHls(world: World, url: string = ROOT_URL): void {
  world.framePorts.last().deliver({ kind: "hls_source", url, referrer_origin: null, referrer_policy: "no-referrer", stream: "vod" });
}

function requestWhole(socket: FakeWebSocket, requestId: number, resource: number): void {
  socket.receive({ type: "fetch_resource", request_id: requestId, resource, byte_range: { kind: "whole" }, max_bytes: 8 * 1024 * 1024 });
}

/** Тело ответа на запрос: рамки подряд (смещения без дыр) и итог. */
function answerOf(socket: FakeWebSocket, requestId: number): { bytes: Uint8Array; outcome: ResourceFetchOutcome | undefined } {
  const pieces: Uint8Array[] = [];
  let expectedOffset = 0;
  for (const payload of socket.sent) {
    if (typeof payload === "string") continue;
    const frame = decodeDataFrame(payload);
    assert.ok(frame.ok);
    if (frame.value.header.kind !== "range_bytes" || frame.value.header.request_id !== requestId) continue;
    assert.equal(frame.value.header.offset, expectedOffset, "рамки запроса идут подряд");
    pieces.push(frame.value.payload);
    expectedOffset += frame.value.payload.length;
  }
  const bytes = new Uint8Array(expectedOffset);
  let position = 0;
  for (const piece of pieces) {
    bytes.set(piece, position);
    position += piece.length;
  }
  const finished = socket
    .sentMessages()
    .flatMap((message: PageSessionMessage) => (message.type === "resource_fetch_finished" && message.request_id === requestId ? [message.outcome] : []));
  assert.ok(finished.length <= 1, "на запрос не больше одного итога");
  return { bytes, outcome: finished[0] };
}

/** Номерки из переписанного плейлиста по порядку появления. */
function handlesIn(text: string): number[] {
  return text
    .split("\n")
    .flatMap((line) => {
      const handle = parseResourceAddress(line);
      return handle === undefined ? [] : [handle];
    });
}

const textOf = (bytes: Uint8Array): string => new TextDecoder().decode(bytes);

describe("нативный HLS через фон: поколение на проводе", () => {
  it("hls_source -> generation_started + hls_source_added{root: 0}; адрес плееру не уходит и в сеть ничего не идёт", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world);
    const socket = world.sockets.last();
    const messages = socket.sentMessages();
    assert.deepEqual(messages.slice(-2), [
      { type: "generation_started", generation: 1 },
      { type: "hls_source_added", generation: 1, root: 0 },
    ]);
    assert.ok(!socket.sent.some((payload) => typeof payload === "string" && payload.includes(REAL_HOST)), "адреса на проводе нет");
    await settle();
    assert.equal(server.calls.length, 0, "без запроса плеера фон ничего не качает");
  });

  it("второе поколение HLS получает номерок корня больше прежних, а прежние номерки перестают работать", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world);
    const socket = world.sockets.last();
    requestWhole(socket, 1, 0);
    await settle();
    const firstHandles = handlesIn(textOf(answerOf(socket, 1).bytes));
    assert.deepEqual(firstHandles, [1], "корень 0 и дочерний плейлист 1");
    deliverHls(world, CHILD_URL);
    const second = socket.sentMessages().filter((message) => message.type === "hls_source_added");
    assert.deepEqual(second.at(-1), { type: "hls_source_added", generation: 2, root: 2 });
    const callsBefore = server.calls.length;
    requestWhole(socket, 2, 1);
    await settle();
    assert.deepEqual(answerOf(socket, 2).outcome, { kind: "failed", failure: { kind: "refused" } }, "номерок прошлого поколения не работает");
    assert.equal(server.calls.length, callsBefore, "и в сеть не ходит");
  });
});

describe("нативный HLS через фон: ресурсы", () => {
  it("fetch_resource(0): переписанный мастер (настоящего хоста в байтах нет) и итог completed с размером, равным отданным байтам", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world);
    const socket = world.sockets.last();
    requestWhole(socket, 5, 0);
    await settle();
    const { bytes, outcome } = answerOf(socket, 5);
    const text = textOf(bytes);
    assert.ok(!text.includes(REAL_HOST) && !text.includes("index.m3u8"), "настоящий адрес из плейлиста убран");
    assert.equal(text, ["#EXTM3U", "#EXT-X-STREAM-INF:BANDWIDTH=800000", formatResourceAddress(1), ""].join("\n"));
    assert.deepEqual(outcome, { kind: "completed", size_bytes: bytes.length });
    assert.equal(server.calls[0]?.url, ROOT_URL);
    assert.equal(server.calls[0]?.init.cache, "no-store", "плейлист качается без кэша");
  });

  it("дочерний плейлист переписан, сегмент отдан байт в байт, кусок сегмента (slice) совпадает со срезом", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world);
    const socket = world.sockets.last();
    requestWhole(socket, 1, 0);
    await settle();
    requestWhole(socket, 2, 1);
    await settle();
    const child = answerOf(socket, 2);
    const [segmentHandle, refusedHandle] = handlesIn(textOf(child.bytes));
    assert.ok(segmentHandle !== undefined);
    assert.equal(refusedHandle, REFUSED_RESOURCE_HANDLE, "локальный сегмент заменён номерком «отказ»");
    assert.ok(!textOf(child.bytes).includes("192.168.1.10"));

    requestWhole(socket, 3, segmentHandle);
    await settle();
    const segment = answerOf(socket, 3);
    assert.deepEqual([...segment.bytes], [...SEGMENT_BYTES]);
    assert.deepEqual(segment.outcome, { kind: "completed", size_bytes: SEGMENT_BYTES.length });
    assert.equal(server.callsTo(SEGMENT_URL)[0]?.init.cache, "default", "сегмент идёт через кэш браузера");

    socket.receive({ type: "fetch_resource", request_id: 4, resource: segmentHandle, byte_range: { kind: "slice", offset: 100, length: 50 }, max_bytes: 4096 });
    await settle();
    const slice = answerOf(socket, 4);
    assert.deepEqual([...slice.bytes], [...SEGMENT_BYTES.slice(100, 150)]);
    assert.deepEqual(slice.outcome, { kind: "completed", size_bytes: 50 });
    assert.equal(server.callsTo(SEGMENT_URL)[1]?.init.headers["Range"], "bytes=100-149");
  });

  it("локальный адрес в плейлисте: плеер просит номерок «отказ» - получает refused, запроса на локальный адрес нет", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world);
    const socket = world.sockets.last();
    requestWhole(socket, 1, 0);
    await settle();
    requestWhole(socket, 2, 1);
    await settle();
    requestWhole(socket, 3, REFUSED_RESOURCE_HANDLE);
    await settle();
    assert.deepEqual(answerOf(socket, 3).outcome, { kind: "failed", failure: { kind: "refused" } });
    assert.equal(server.callsTo(LOCAL_SEGMENT_URL).length, 0);
    assert.equal(socket.closedWithCode, undefined, "сеанс жив");
  });

  it("неизвестный номерок: refused без сети, сеанс жив", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world);
    const socket = world.sockets.last();
    requestWhole(socket, 9, 424_242);
    await settle();
    assert.deepEqual(answerOf(socket, 9).outcome, { kind: "failed", failure: { kind: "refused" } });
    assert.equal(server.calls.length, 0);
    assert.equal(world.handoffs.activeTabIds().includes(TAB), true);
  });
});

describe("нативный HLS через фон: запрещённое и неподходящее", () => {
  it("корень на локальном адресе при публичной странице: передача кончена как у файла, в сеть ничего не ушло", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world, "http://192.168.1.1/master.m3u8");
    await settle();
    assert.deepEqual(world.ended, [{ kind: "local_address_refused" }]);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.ok(!world.sockets.last().sentMessages().some((message) => message.type === "hls_source_added"));
    assert.equal(server.calls.length, 0);
  });

  it("протокол 1.0: hls_source_added на провод не уходит, сеанс кончается честным отказом native_hls", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    world.door.nextOutcome = (request) => grantFor(request, "started", PROTOCOL_VERSION_1_0);
    await startHandoff(world);
    deliverHls(world);
    await settle();
    const socket = world.sockets.last();
    assert.ok(!socket.sentMessages().some((message) => message.type === "hls_source_added" || message.type === "generation_started"));
    assert.deepEqual(world.ended, [{ kind: "source_needs_newer_player", form: "native_hls" }]);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "not_supported_yet", form: "native_hls" });
  });

  it("протокол 1.0, источник назван ещё до handoff_began: исход команды not_supported_yet{native_hls}", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    world.door.nextOutcome = (request) => grantFor(request, "started", PROTOCOL_VERSION_1_0);
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    const socket = world.sockets.last();
    socket.open();
    socket.receive({ type: "attached" });
    deliverHls(world);
    assert.deepEqual(await outcome, { kind: "not_supported_yet", form: "native_hls" });
    assert.ok(!socket.sentMessages().some((message) => message.type === "hls_source_added"));
  });

  it("fetch_resource в поколении-файле и в поколении MSE: refused без сети", async () => {
    const server = hlsServer();
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    const port = world.framePorts.last();
    port.deliver({ kind: "file_source", url: "https://cdn.secret-host.example/movie.mp4", referrer_origin: null, referrer_policy: "no-referrer" });
    const socket = world.sockets.last();
    requestWhole(socket, 1, 0);
    port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "video/mp4" } });
    requestWhole(socket, 2, 0);
    await settle();
    assert.deepEqual(answerOf(socket, 1).outcome, { kind: "failed", failure: { kind: "refused" } });
    assert.deepEqual(answerOf(socket, 2).outcome, { kind: "failed", failure: { kind: "refused" } });
    assert.equal(server.calls.length, 0);
  });
});

describe("нативный HLS через фон: смена поколения и конец", () => {
  it("после HLS пришло поколение MSE: загрузка оборвана, старый номерок refused, HLS-рамок больше нет", async () => {
    const server = hlsServer({ hang: true });
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world);
    const socket = world.sockets.last();
    requestWhole(socket, 1, 0);
    await settle();
    assert.equal(server.calls[0]?.init.signal.aborted, false, "мастер качается");
    world.framePorts.last().deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "video/mp4" } });
    await settle();
    assert.equal(server.calls[0]?.init.signal.aborted, true, "загрузка прошлого поколения оборвана");
    requestWhole(socket, 2, 0);
    await settle();
    assert.deepEqual(answerOf(socket, 2).outcome, { kind: "failed", failure: { kind: "refused" } });
    assert.equal(answerOf(socket, 1).outcome, undefined, "от оборванного запроса итога нет");
    assert.equal(server.calls.length, 1);
  });

  it("новое поколение-файл после HLS: HLS снят, номерок корня больше не работает", async () => {
    const server = hlsServer({ hang: true });
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    deliverHls(world);
    const socket = world.sockets.last();
    requestWhole(socket, 1, 0);
    await settle();
    world.framePorts.last().deliver({ kind: "file_source", url: "https://cdn.secret-host.example/movie.mp4", referrer_origin: null, referrer_policy: "no-referrer" });
    await settle();
    assert.equal(server.calls[0]?.init.signal.aborted, true);
    requestWhole(socket, 2, 0);
    await settle();
    assert.deepEqual(answerOf(socket, 2).outcome, { kind: "failed", failure: { kind: "refused" } });
  });

  it("конец сеанса обрывает загрузки HLS: после закрытия в сокет ничего", async () => {
    const server = hlsServer({ hang: true });
    const world = hlsWorld(server.fetch);
    await startHandoff(world);
    const port = world.framePorts.last();
    deliverHls(world);
    const socket = world.sockets.last();
    requestWhole(socket, 1, 0);
    await settle();
    const sentBefore = socket.sent.length;
    port.deliver({ kind: "handoff_ended", cause: "element_released" });
    await settle();
    assert.equal(server.calls[0]?.init.signal.aborted, true);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.equal(socket.sent.length, sentBefore);
  });
});

describe("нативный HLS через фон: переезд на задержанную запись", () => {
  /** Реклама-файл играет, рядом ждёт HLS-фильм; затем реклама кончается. */
  const AD: VideoCandidate = {
    ...DEFAULT_CANDIDATE_FACTS,
    document: "doc-ad",
    element: 3,
    media: { kind: "file" },
    file_host: "video.unknown-ad-cdn.example",
    playing: true,
    visible_area_px: 296_000,
    duration_s: 24,
  };
  const HLS_FILM: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 1, media: { kind: "hls" }, playing: true, visible_area_px: 296_000, duration_s: 6381, file_host: REAL_HOST };
  const HELD = { kind: HELD_VIDEO_REPORT_KIND, element: HLS_FILM.element, candidates: [HLS_FILM] } as const;

  async function adThenHeldHls(agreed?: typeof PROTOCOL_VERSION_1_0): Promise<World> {
    const world = setup([{ frameId: 0, result: [{ ...HLS_FILM, playing: false }, AD] }], hlsServer().fetch);
    if (agreed !== undefined) world.door.nextOutcome = (request) => grantFor(request, "started", agreed);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    return world;
  }

  it("запись HLS, задержанная сторожем, принимается: begin_handoff на неё и затем hls_source_added на проводе", async () => {
    const world = await adThenHeldHls();
    assert.equal(world.handoffs.heldVideo(TAB, 0, HELD), "adopt");
    const begin = world.framePorts.last().posted.filter((command): command is Extract<PageCommand, { kind: "begin_handoff" }> => command.kind === "begin_handoff").at(-1);
    assert.equal(begin?.element, HLS_FILM.element);
    const port = world.framePorts.last();
    port.deliver({ kind: "handoff_began", request_id: begin?.request_id, outcome: "started" });
    deliverHls(world);
    assert.deepEqual(world.sockets.last().sentMessages().at(-1), { type: "hls_source_added", generation: 1, root: 0 });
  });

  it("плеер с протоколом 1.0 запись HLS не получает: видео отпущено, сеанс на месте", async () => {
    const world = await adThenHeldHls(PROTOCOL_VERSION_1_0);
    assert.equal(world.handoffs.heldVideo(TAB, 0, HELD), "release");
    assert.deepEqual(world.ended, []);
  });
});
