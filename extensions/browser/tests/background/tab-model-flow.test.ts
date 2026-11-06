// Передача вкладки с моделью вкладки (09c): отказы без сеанса, запись опросов и отказов,
// ограниченный опрос фреймов, сбой хранилища.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { capCandidates, MAX_SURVEY_CANDIDATES_TOTAL, SURVEY_TIMEOUT_MS } from "../../src/background/handoff/frame-survey.ts";
import type { TabModelStore } from "../../src/background/tab-model/session-store.ts";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { PLAYING_VIDEO, setup, startHandoff, TAB } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

function video(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return { ...PLAYING_VIDEO, element, ...overrides };
}

describe("отказы без сеанса: плеер не будится", () => {
  it("лучшее видео под DRM — protected, open_session не отправлен, отказ записан в модель", async () => {
    const world = setup([{ frameId: 0, result: [video(1, { drm: true })] }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "protected" });
    assert.equal(world.door.requests.length, 0);
    assert.equal(world.door.demandAcquired(), 0);
    assert.equal(world.sockets.sockets.length, 0);
    assert.equal(world.framePorts.ports.length, 0);
    assert.deepEqual(world.handoffs.activeTabIds(), []);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "protected" });
    assert.equal((await world.tabModel.videosFoundIn(TAB))[0]?.verdict, "protected");
  });

  it("играет только рекламный ролик — сеанс открывается на нём, модель видит рекламу (решение 201)", async () => {
    const ad = video(1, { media: { kind: "file" }, file_host: "x.doubleclick.net" });
    const world = setup([{ frameId: 0, result: [ad] }]);
    void world.handoffs.handOffTab(TAB);
    await flushPromises();
    assert.equal(world.door.requests.length, 1, "плеер разбужен");
    assert.equal(await world.tabModel.isAdOn(TAB, { frame_id: 0, document: ad.document, element: 1 }), true);
  });

  it("преролл с неизвестного домена рядом с пустым плеером — сеанс на рекламе, главного видео ещё нет (Tortuga)", async () => {
    const preroll = video(2, { media: { kind: "file" }, file_host: "video.unknown-ad-cdn.example", duration_s: 30 });
    const emptySlot = video(1, { media: { kind: "none" }, playing: false, duration_s: null });
    const world = setup([{ frameId: 0, result: [emptySlot, preroll] }]);
    void world.handoffs.handOffTab(TAB);
    await flushPromises();
    assert.equal(world.door.requests.length, 1, "плеер разбужен");
    assert.equal(await world.tabModel.isAdOn(TAB, { frame_id: 0, document: preroll.document, element: 2 }), true);
    assert.equal(await world.tabModel.mainVideoOf(TAB), null);
  });

  it("not_supported_yet и no_video тоже записываются как закрытые причины", async () => {
    const hls = setup([{ frameId: 0, result: [video(1, { media: { kind: "unsupported", form: "native_hls" } })] }]);
    assert.deepEqual(await hls.handoffs.handOffTab(TAB), { kind: "not_supported_yet", form: "native_hls" });
    assert.deepEqual(await hls.tabModel.lastRefusalOf(TAB), { kind: "not_supported_yet", form: "native_hls" });
    const empty = setup([{ frameId: 0, result: null }]);
    assert.deepEqual(await empty.handoffs.handOffTab(TAB), { kind: "no_video" });
    assert.deepEqual(await empty.tabModel.lastRefusalOf(TAB), { kind: "no_video" });
  });

  it("отказ плеера записывается как player_unavailable", async () => {
    const world = setup();
    world.door.nextOutcome = () => ({
      kind: "refused",
      refusal: { type: "refused", in_reply_to: { kind: "open_session", request_id: 1 }, reason: { kind: "player_not_responding" } },
    });
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "player_unavailable", reason: "player_not_responding" });
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "player_unavailable" });
  });

  it("DRM появился между опросом и командой — protected, сеанс закрыт через tab_gone и не остаётся открытым", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: 1, outcome: "protected" });
    assert.deepEqual(await outcome, { kind: "protected" });
    assert.deepEqual(world.sockets.last().wireSummary(), ["attach", "tab_gone"]);
    assert.equal(world.door.demandHeld(), 0);
    assert.deepEqual(world.handoffs.activeTabIds(), []);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "protected" });
  });
});

describe("успешная передача и модель", () => {
  it("опрос записан, отказа нет, передача стартует; методы модели не трогают состояние передачи", async () => {
    const world = setup();
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "new" });
    assert.deepEqual(await world.tabModel.mainVideoOf(TAB), { frame_id: 0, document: PLAYING_VIDEO.document, element: PLAYING_VIDEO.element });
    assert.equal(await world.tabModel.lastRefusalOf(TAB), null);
    const socketsBefore = world.sockets.sockets.length;
    await world.tabModel.noteHandoffRefused(TAB, { kind: "failed" });
    await world.tabModel.recordSurvey(TAB, []);
    await world.tabModel.forgetTab(TAB);
    assert.deepEqual(world.handoffs.activeTabIds(), [TAB], "передача жива");
    assert.equal(world.sockets.sockets.length, socketsBefore);
    assert.equal(world.door.demandHeld(), 1, "нужда связи не тронута моделью");
    assert.equal(world.sockets.last().closedWithCode, undefined, "сокет не закрыт моделью");
  });

  it("сбой хранилища не ломает передачу и виден в журнале один раз", async () => {
    const broken: TabModelStore = {
      get: () => Promise.reject(new Error("quota")),
      set: () => Promise.reject(new Error("quota")),
      remove: () => Promise.reject(new Error("quota")),
      keys: () => Promise.reject(new Error("quota")),
    };
    const world = setup(undefined, undefined, broken);
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "new" });
    assert.equal(world.storeFailures.length, 1);
  });
});

describe("ограниченный опрос фреймов", () => {
  it("фрейм завис — через таймаут честный исход failed/survey_timed_out, причина в модели, повторная команда работает", async () => {
    const world = setup(new Promise(() => {}));
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.clock.advance(SURVEY_TIMEOUT_MS);
    assert.deepEqual(await outcome, { kind: "failed", reason: "survey_timed_out" });
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "survey_timed_out" });
    assert.equal(world.door.requests.length, 0);
    assert.equal(world.clock.liveTimers(), 0, "таймер опроса снят");
    assert.deepEqual(world.handoffs.activeTabIds(), [], "запись вкладки не осталась висеть");
  });

  it("быстрый ответ не оставляет живого таймера", async () => {
    const world = setup([{ frameId: 0, result: null }]);
    await world.handoffs.handOffTab(TAB);
    assert.equal(world.clock.liveTimers(), 0);
  });

  it("команда для уже закрытой вкладки записи не оставляет: опрос не внедрился, вкладки нет", async () => {
    const world = setup(Promise.reject(new Error("No tab with id: 7")));
    world.closeTab();
    const outcome = await world.handoffs.handOffTab(TAB);
    assert.equal(outcome.kind, "failed");
    assert.deepEqual(await world.store.keys(), [], "в хранилище нет записи про вкладку, которой нет");
  });

  it("запрет внедрения — failed/tab_not_scriptable и запись в модель", async () => {
    const world = setup(Promise.reject(new Error("Cannot access chrome:// pages")));
    const outcome = await world.handoffs.handOffTab(TAB);
    assert.equal(outcome.kind, "failed");
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "tab_not_scriptable" });
  });

  it("потолок кандидатов: лишнее отбрасывается, играющие и крупные остаются, порядок сохранён", () => {
    const frames = [
      { frameId: 0, candidates: Array.from({ length: 300 }, (_, index) => video(index + 1, { playing: false, visible_area_px: 10 })) },
      { frameId: 1, candidates: Array.from({ length: 300 }, (_, index) => video(index + 1, { playing: index === 299, visible_area_px: index === 299 ? 5 : 1 })) },
    ];
    const capped = capCandidates(frames, MAX_SURVEY_CANDIDATES_TOTAL);
    assert.equal(capped.truncated, true);
    assert.equal(capped.frames.reduce((sum, frame) => sum + frame.candidates.length, 0), MAX_SURVEY_CANDIDATES_TOTAL);
    const frameOne = capped.frames.find((frame) => frame.frameId === 1);
    assert.ok(frameOne?.candidates.some((candidate) => candidate.element === 300 && candidate.playing), "играющее выжило");
    const elements = capped.frames[0]?.candidates.map((candidate) => candidate.element) ?? [];
    assert.deepEqual(elements, [...elements].sort((left, right) => left - right), "порядок документа сохранён");
  });

  it("под потолком ничего не режется", () => {
    const frames = [{ frameId: 0, candidates: [video(1), video(2)] }];
    const capped = capCandidates(frames, MAX_SURVEY_CANDIDATES_TOTAL);
    assert.equal(capped.truncated, false);
    assert.deepEqual(capped.frames, frames);
  });

  it("враждебная страница с тысячами кандидатов по фреймам: передача всё равно выбирает играющее видео", async () => {
    const hostile = Array.from({ length: 20 }, (_, index) => ({
      frameId: index + 1,
      result: Array.from({ length: 256 }, (_unused, element) => video(element + 1, { document: `doc-${index}`, playing: false, visible_area_px: 1 })),
    }));
    const world = setup([{ frameId: 0, result: [PLAYING_VIDEO] }, ...hostile]);
    assert.deepEqual(await startHandoff(world), { kind: "started", player_window: "new" });
    assert.equal(world.framePorts.last().frameId, 0);
    const kept = await world.tabModel.videosFoundIn(TAB);
    assert.ok(kept.length <= 64);
  });
});
