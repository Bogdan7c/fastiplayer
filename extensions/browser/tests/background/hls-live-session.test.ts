// Эфир нативного HLS через фон (сессия 10a3, решение владельца 232): допуск по версии протокола плеера,
// окно эфира `live_window` → команда тени и сверка поколения, задержанное видео (09c2).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { playerAbilitiesOf, playerCanPlayHls } from "../../src/background/handoff/hls-player-support.ts";
import { judgeHeldVideo } from "../../src/background/handoff/held-video-choice.ts";
import { translatePlayerMessage } from "../../src/background/handoff/player-message-translator.ts";
import {
  PROTOCOL_VERSION_1_0,
  PROTOCOL_VERSION_1_1,
  PROTOCOL_VERSION_1_2,
  SESSION_CLOSE_CODES,
  type PlayerSessionMessage,
  type ProtocolVersion,
} from "../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, type HlsStreamKind, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { flushPromises, generationsAt, grantFor } from "./support.ts";

const LIVE_URL = "https://live.example/channel/index.m3u8";
const PLAYING_LIVE: VideoCandidate = {
  ...DEFAULT_CANDIDATE_FACTS,
  document: "doc-live",
  element: 2,
  media: { kind: "hls" },
  playing: true,
  visible_area_px: 300_000,
  duration_s: null,
  file_host: "live.example",
};

/** Мир, где плеер договорился на `agreed`, а на вкладке играет эфир. */
function liveWorld(agreed: ProtocolVersion): World {
  const world = setup([{ frameId: 0, result: [PLAYING_LIVE] }]);
  world.door.nextOutcome = (request) => grantFor(request, "started", agreed);
  return world;
}

function deliverHls(world: World, stream: HlsStreamKind): void {
  world.framePorts.last().deliver({ kind: "hls_source", url: LIVE_URL, referrer_origin: null, referrer_policy: "no-referrer", stream });
}

const sentTypes = (world: World): string[] => world.sockets.last().sentMessages().map((message) => message.type);

/** Команды тени, которые фон отдал странице. */
function shadowCommandsOf(world: World): Extract<PageCommand, { kind: "shadow" }>["command"][] {
  return world.framePorts.last().posted.flatMap((command) => (command.kind === "shadow" ? [command.command] : []));
}

const liveWindow = (generation: number, edgeUs: number): PlayerSessionMessage => ({
  type: "live_window",
  generation,
  edge_us: edgeUs,
  seekable: { kind: "range", start_us: edgeUs - 20_000_000, end_us: edgeUs - 500_000 },
});

describe("возможности плеера по версии протокола (hls-player-support.ts)", () => {
  it("1.0 — ни записи, ни эфира; 1.1 — только запись; 1.2 — и эфир", () => {
    assert.deepEqual(playerAbilitiesOf(PROTOCOL_VERSION_1_0), { nativeHls: false, hlsLive: false });
    assert.deepEqual(playerAbilitiesOf(PROTOCOL_VERSION_1_1), { nativeHls: true, hlsLive: false });
    assert.deepEqual(playerAbilitiesOf(PROTOCOL_VERSION_1_2), { nativeHls: true, hlsLive: true });
  });

  it("эфир требует обе возможности: без записи (механика номерков) эфира тоже нет", () => {
    assert.equal(playerCanPlayHls({ nativeHls: true, hlsLive: true }, "live"), true);
    assert.equal(playerCanPlayHls({ nativeHls: true, hlsLive: false }, "live"), false);
    assert.equal(playerCanPlayHls({ nativeHls: false, hlsLive: true }, "live"), false);
    assert.equal(playerCanPlayHls({ nativeHls: true, hlsLive: false }, "vod"), true);
    assert.equal(playerCanPlayHls({ nativeHls: false, hlsLive: true }, "vod"), false);
  });
});

describe("эфир HLS через сеанс: допуск по версии", () => {
  it("плеер 1.2: эфир передаётся — поколение HLS на проводе, сеанс жив", async () => {
    const world = liveWorld(PROTOCOL_VERSION_1_2);
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "new" });
    deliverHls(world, "live");
    assert.deepEqual(sentTypes(world).slice(-2), ["generation_started", "hls_source_added"]);
    assert.deepEqual(world.ended, []);
  });

  it("плеер 1.1: эфир — честный отказ native_hls, ни одного сообщения поколения на проводе", async () => {
    const world = liveWorld(PROTOCOL_VERSION_1_1);
    await startHandoff(world);
    deliverHls(world, "live");
    await flushPromises();
    assert.ok(!sentTypes(world).includes("hls_source_added") && !sentTypes(world).includes("generation_started"));
    assert.deepEqual(world.ended, [{ kind: "source_needs_newer_player", form: "native_hls" }]);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.protocol_violation);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "not_supported_yet", form: "native_hls" });
  });

  it("плеер 1.1, эфир назван до handoff_began: исход команды (кнопка, попап) — not_supported_yet{native_hls}", async () => {
    const world = liveWorld(PROTOCOL_VERSION_1_1);
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    const socket = world.sockets.last();
    socket.open();
    socket.receive({ type: "attached" });
    deliverHls(world, "live");
    assert.deepEqual(await outcome, { kind: "not_supported_yet", form: "native_hls" });
    assert.ok(!sentTypes(world).includes("hls_source_added"));
  });

  it("плеер 1.1: запись HLS по-прежнему передаётся (регрессия 10a2)", async () => {
    const world = liveWorld(PROTOCOL_VERSION_1_1);
    await startHandoff(world);
    deliverHls(world, "vod");
    assert.equal(sentTypes(world).at(-1), "hls_source_added");
    assert.deepEqual(world.ended, []);
  });
});

describe("окно эфира: от плеера к тени", () => {
  it("переводчик: окно последнего поколения — команда тени без номера поколения; чужое — пропуск", () => {
    const window = liveWindow(3, 61_000_000);
    assert.deepEqual(translatePlayerMessage(window, generationsAt(3)), {
      kind: "shadow",
      command: { kind: "live_window", edge_us: 61_000_000, seekable: { kind: "range", start_us: 41_000_000, end_us: 60_500_000 } },
    });
    assert.deepEqual(translatePlayerMessage(window, generationsAt(4)), { kind: "ignore" }, "окно прошлого поколения");
    assert.deepEqual(translatePlayerMessage(window, generationsAt(undefined)), { kind: "ignore" }, "поколений ещё нет");
    assert.deepEqual(translatePlayerMessage({ type: "live_window", generation: 3, edge_us: 5, seekable: { kind: "none" } }, generationsAt(3)), {
      kind: "shadow",
      command: { kind: "live_window", edge_us: 5, seekable: { kind: "none" } },
    });
  });

  it("переводчик: окно записи (протокол 1.11) — то же правило поколения, без номера в команде тени", () => {
    const recording = {
      type: "recording_window",
      generation: 3,
      end_us: 70_000_000,
      seekable: { kind: "range", start_us: 41_000_000, end_us: 66_000_000 },
    } as const;
    assert.deepEqual(translatePlayerMessage(recording, generationsAt(3)), {
      kind: "shadow",
      command: { kind: "recording_window", end_us: 70_000_000, seekable: { kind: "range", start_us: 41_000_000, end_us: 66_000_000 } },
    });
    assert.deepEqual(translatePlayerMessage(recording, generationsAt(4)), { kind: "ignore" }, "окно записи прошлого поколения");
  });

  it("сеанс: окно текущего поколения доходит до страницы, окно чужого поколения — нет", async () => {
    const world = liveWorld(PROTOCOL_VERSION_1_2);
    await startHandoff(world);
    deliverHls(world, "live");
    const socket = world.sockets.last();
    const generation = socket.sentMessages().find((message) => message.type === "generation_started");
    assert.ok(generation?.type === "generation_started");
    socket.receive(liveWindow(generation.generation + 1, 90_000_000));
    socket.receive(liveWindow(generation.generation, 61_000_000));
    const windows = shadowCommandsOf(world).filter((command) => command.kind === "live_window");
    assert.deepEqual(windows, [{ kind: "live_window", edge_us: 61_000_000, seekable: { kind: "range", start_us: 41_000_000, end_us: 60_500_000 } }]);
    assert.equal(socket.closedWithCode, undefined, "окно чужого поколения — не нарушение, просто пропуск");
  });
});

describe("эфир, задержанный сторожем (09c2)", () => {
  const report = { kind: HELD_VIDEO_REPORT_KIND, element: PLAYING_LIVE.element, candidates: [PLAYING_LIVE], trigger: "next_video" } as const;
  const rule = { pageHost: "site.example", setting: "ask", consentedHost: null } as const;

  it("плееру 1.2 эфир переезжает, плееру 1.1 — отпускается странице", () => {
    assert.equal(judgeHeldVideo(0, report, rule, playerAbilitiesOf(PROTOCOL_VERSION_1_2)).kind, "adopt");
    assert.equal(judgeHeldVideo(0, report, rule, playerAbilitiesOf(PROTOCOL_VERSION_1_1)).kind, "release");
  });

  it("запись плееру 1.1 переезжает, как в 10a2", () => {
    const vod = { ...report, candidates: [{ ...PLAYING_LIVE, duration_s: 600 }] };
    assert.equal(judgeHeldVideo(0, vod, rule, playerAbilitiesOf(PROTOCOL_VERSION_1_1)).kind, "adopt");
  });
});
