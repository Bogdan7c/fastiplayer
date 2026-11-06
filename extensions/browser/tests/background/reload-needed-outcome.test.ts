// Исход `reload_needed` (сессия 10d) на НАСТОЯЩЕМ владельце передач: вкладка старше скрипта не будит плеер.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { PLAYING_VIDEO, setup, TAB, WORLD_EXTENSION_VERSION } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

const SCRIPTLESS_TOP = { frameId: 0, result: { script_absent: true, on_web_page: true } };
const missedStream: VideoCandidate = { ...PLAYING_VIDEO, media: { kind: "unsupported", form: "started_before_script" } };

/** Ни плеер, ни страницу передача не тронула. */
function assertNothingOpened(world: ReturnType<typeof setup>): void {
  assert.equal(world.door.requests.length, 0, "плеер не разбужен");
  assert.equal(world.door.demandAcquired(), 0);
  assert.equal(world.framePorts.ports.length, 0, "к странице не подключались");
}

describe("вкладка старше скрипта", () => {
  it("Chrome: скрипта нет — reload_needed, плеер не будится", async () => {
    const world = setup([SCRIPTLESS_TOP]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "reload_needed", reason: "script_missing" });
    assertNothingOpened(world);
  });

  it("Firefox: скрипт опоздал к началу потока — reload_needed вместо ложного «not supported yet»", async () => {
    const world = setup([{ frameId: 0, result: [missedStream] }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "reload_needed", reason: "stream_started_before_script" });
    assertNothingOpened(world);
  });

  it("выбранная строка с упущенным потоком ведёт к той же кнопке", async () => {
    const world = setup([{ frameId: 0, result: [missedStream] }]);
    const video = { frame_id: 0, document: missedStream.document, element: missedStream.element };
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, video), { kind: "reload_needed", reason: "stream_started_before_script" });
  });

  it("другая версия скрипта — reload_needed, его видео не передаётся", async () => {
    const world = setup([{ frameId: 0, result: { script_version: `${WORLD_EXTENSION_VERSION}-old`, candidates: [PLAYING_VIDEO] } }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "reload_needed", reason: "script_outdated" });
    assertNothingOpened(world);
  });

  it("скрипт на месте, видео нет — обычное no_video, кнопки перезагрузки нет", async () => {
    const world = setup([{ frameId: 0, result: [] }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "no_video" });
  });

  it("настоящий Blob при скрипте вовремя — честное «not supported yet», перезагрузка тут не поможет", async () => {
    const blobFile: VideoCandidate = { ...PLAYING_VIDEO, media: { kind: "unsupported", form: "blob_file" } };
    const world = setup([{ frameId: 0, result: [blobFile] }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "not_supported_yet", form: "blob_file" });
  });

  it("в старой вкладке свежий подфрейм с видео передаётся без перезагрузки", async () => {
    const world = setup([SCRIPTLESS_TOP, { frameId: 3, result: [PLAYING_VIDEO] }]);
    const pending = world.handoffs.handOffTab(TAB);
    await flushPromises();
    assert.equal(world.door.requests.length, 1, "сеанс открывается: видео есть");
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    const port = world.framePorts.last();
    const begin = port.posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    assert.deepEqual(await pending, { kind: "started", player_window: "new" });
  });

  it("запись об отказе не остаётся: это предложение, а не отказ", async () => {
    const world = setup([SCRIPTLESS_TOP]);
    await world.handoffs.handOffTab(TAB);
    assert.equal(await world.tabModel.lastRefusalOf(TAB), null);
  });
});
