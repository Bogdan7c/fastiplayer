// Файл `<video src>` через весь фон (сессия 09b): `file_source` страницы → поколение-файл на проводе
// (байт в байт как записанный поток `fixtures/streams/file-source-generation`) → `fetch_range`
// плеера → фон качает диапазон → рамки вида 2 и `range_fetch_finished` → конец сеанса обрывает загрузку.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";
import type { RangeFetch, RangeFetchInit } from "../../src/background/handoff/range-fetcher.ts";
import { decodeDataFrame, SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { FIXTURES_ROOT } from "../protocol/fixtures-support.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { setup, startHandoff, TAB } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

const FILE_URL = "https://cdn.example/movie.mp4?sig=abc";
/** Размер файла записанного потока (005.json). */
const FIXTURE_FILE_SIZE = 48_211_005;
const PLAYING_FILE: VideoCandidate = { document: "doc-7f3a", element: 4, media: { kind: "file" }, playing: true, visible_area_px: 200_000, ...DEFAULT_CANDIDATE_FACTS, file_host: "cdn.example" };

function fixtureText(name: string): string {
  return readFileSync(join(FIXTURES_ROOT, "streams", "file-source-generation", name), "utf8").trim();
}

/** Сервер: каждый запрос — 206 с первыми байтами диапазона (файл большой, отдаём куском). */
function recordingServer(options: { hang?: boolean } = {}) {
  const calls: { url: string; init: RangeFetchInit }[] = [];
  const fetch: RangeFetch = (url, init) => {
    calls.push({ url, init });
    const match = /^bytes=(\d+)-(\d+)$/.exec(init.headers["Range"] ?? "");
    assert.ok(match);
    const first = Number(match[1]);
    const last = Number(match[2]);
    const body = new Uint8Array(last - first + 1).map((_, index) => (first + index) % 256);
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        init.signal.addEventListener("abort", () => controller.error(new DOMException("aborted", "AbortError")));
        if (options.hang === true) return;
        controller.enqueue(body);
        controller.close();
      },
    });
    return Promise.resolve(new Response(stream, { status: 206, headers: { "content-range": `bytes ${first}-${last}/${FIXTURE_FILE_SIZE}` } }));
  };
  return { fetch, calls };
}

async function settle(): Promise<void> {
  for (let round = 0; round < 20; round += 1) await flushPromises();
}

describe("файл <video src> через фон", () => {
  it("file_source → generation_started + file_source_added байт в байт как записанный поток; адрес плееру не уходит", async () => {
    const server = recordingServer();
    const world = setup([{ frameId: 0, result: [PLAYING_FILE] }], server.fetch);
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "new" });
    const port = world.framePorts.last();
    port.deliver({ kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    port.deliver({ kind: "page_seek", target_us: 0 });
    const socket = world.sockets.last();
    const texts = socket.sent.filter((payload): payload is string => typeof payload === "string");
    assert.equal(texts[1], fixtureText("001.json"));
    assert.equal(texts[2], fixtureText("002.json"));
    assert.ok(!texts.some((text) => text.includes("cdn.example")), "протокол адресов не несёт");
    assert.equal(server.calls.length, 0, "без запроса плеера фон ничего не качает");
  });

  it("fetch_range → фон качает этот диапазон по адресу файла → рамки вида 2, затем итог байт в байт", async () => {
    const server = recordingServer();
    const world = setup([{ frameId: 0, result: [PLAYING_FILE] }], server.fetch);
    await startHandoff(world);
    world.framePorts.last().deliver({ kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    const socket = world.sockets.last();
    socket.receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.equal(server.calls[0]?.url, FILE_URL);
    assert.equal(server.calls[0]?.init.headers["Range"], "bytes=0-3");
    assert.deepEqual(socket.wireSummary().slice(-2), ["range 1@0+4", "range_fetch_finished"]);
    const frame = socket.sent.find((payload) => typeof payload !== "string" && decodeDataFrame(payload).ok);
    assert.ok(frame !== undefined && typeof frame !== "string");
    const decoded = decodeDataFrame(frame);
    assert.ok(decoded.ok);
    assert.deepEqual([...decoded.value.payload], [0, 1, 2, 3]);
    assert.equal(socket.sent.at(-1), fixtureText("005.json"));
  });

  it("fetch_range без поколения-файла (MSE) — фон в сеть не ходит, сеанс не рвётся", async () => {
    const server = recordingServer();
    const world = setup(undefined, server.fetch);
    await startHandoff(world);
    const socket = world.sockets.last();
    socket.receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.equal(server.calls.length, 0);
    assert.equal(socket.closedWithCode, undefined);
    assert.equal(world.handoffs.activeTabIds().includes(TAB), true);
  });

  it("конец сеанса обрывает загрузку: после закрытия ни рамки, ни итога", async () => {
    const server = recordingServer({ hang: true });
    const world = setup([{ frameId: 0, result: [PLAYING_FILE] }], server.fetch);
    await startHandoff(world);
    const port = world.framePorts.last();
    port.deliver({ kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    const socket = world.sockets.last();
    socket.receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    const sentBefore = socket.sent.length;
    // Страница забрала видео (её запись источника под тенью файла) — 4009, загрузка оборвана.
    port.deliver({ kind: "handoff_ended", cause: "element_released" });
    await settle();
    assert.equal(server.calls[0]?.init.signal.aborted, true);
    assert.equal(socket.closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.equal(socket.sent.length, sentBefore, "после конца в сокет ничего");
  });

  it("после файла пришло поколение MSE — загрузчик файла снят: текущий запрос оборван, новые в сеть не идут (находка 12)", async () => {
    const server = recordingServer({ hang: true });
    const world = setup([{ frameId: 0, result: [PLAYING_FILE] }], server.fetch);
    await startHandoff(world);
    const port = world.framePorts.last();
    port.deliver({ kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    const socket = world.sockets.last();
    socket.receive({ type: "fetch_range", request_id: 1, offset: 0, length: 4 });
    await settle();
    assert.equal(server.calls[0]?.init.signal.aborted, false, "файл качается");
    // Страница поставила элементу MediaSource: новое поколение.
    port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "video/mp4" } });
    await settle();
    assert.equal(server.calls[0]?.init.signal.aborted, true, "загрузка прошлого файла оборвана");
    assert.ok(socket.wireSummary().includes("source_buffer_added"), "поколение MSE ушло плееру");
    const sentAfterSwitch = socket.sent.length;
    socket.receive({ type: "fetch_range", request_id: 2, offset: 0, length: 4 });
    await settle();
    assert.equal(server.calls.length, 1, "запрос байт файла в поколении MSE в сеть не идёт");
    assert.equal(socket.closedWithCode, undefined, "сеанс жив");
    assert.ok(
      socket.sent.slice(sentAfterSwitch).every((payload) => typeof payload === "string" && !payload.includes("range_fetch")),
      "ни рамки, ни итога от брошенного файла",
    );
  });

  it("неподдержанная форма — not_supported_yet с формой, плеер даже не зовётся", async () => {
    const world = setup([{ frameId: 0, result: [{ ...PLAYING_FILE, media: { kind: "unsupported", form: "native_hls" }, file_host: null }] }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "not_supported_yet", form: "native_hls" });
    assert.equal(world.sockets.sockets.length, 0);
    assert.equal(world.door.demandHeld(), 0);
  });
});
