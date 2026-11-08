// Дублёр кнопки пропуска на уровне сеанса (09d): аренда наблюдения, ad_state на проводе, команды плеера
// skip_ad. Фон целиком на фейковых часах, плеере, порте и браузере.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  AD_STATE_MIN_GAP_MS,
  AD_WATCH_LEASE_MS,
  AD_WATCH_RENEW_MS,
  createSessionAdMirror,
  type AdMirrorBrowser,
  type SessionAdMirror,
} from "../../src/background/handoff/ad-skip-mirror.ts";
import { createAdWatchReportListener } from "../../src/background/handoff/ad-watch-listener.ts";
import { AD_REPORT_SILENCE_MS } from "../../src/background/handoff/ad-on-screen.ts";
import { translatePlayerMessage } from "../../src/background/handoff/player-message-translator.ts";
import type { PageAdState, PageSessionMessage } from "../../src/protocol/index.ts";
import { AD_WATCH_REPORT_KIND } from "../../src/shared/ad-watch-contract.ts";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { PLAYING_VIDEO, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { createFakeClock, flushPromises } from "./support.ts";

const NO_ORGANS: PageAdState = { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "unavailable" } };
const AVAILABLE: PageAdState = { kind: "showing", pod: { kind: "known", index: 1, count: 2 }, skip: { kind: "available" } };
const ON_PAGE: PageAdState = { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "on_page" } };
const NONE: PageAdState = { kind: "none" };

/** Состояния рекламы, которые плеер получил по проводу. */
const adStates = (world: World): PageAdState[] =>
  world.sockets
    .last()
    .sentMessages()
    .filter((message): message is Extract<PageSessionMessage, { type: "ad_state" }> => message.type === "ad_state")
    .map((message) => message.ad);

const AD_VIDEO: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 3, media: { kind: "file" }, file_host: "video.unknown-ad-cdn.example", playing: true, visible_area_px: 296_000, duration_s: 24 };
const FILM_VIDEO: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 1, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: 296_000, duration_s: 6381 };

describe("аренда наблюдения во фреймах", () => {
  it("до начала тени фон молчит: ни аренды, ни ad_state, донесение чужой страницы вне сеанса игнорируется (решение 198)", async () => {
    const world = setup();
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    world.clock.advance(1_000);
    assert.deepEqual(world.adMirror.armings, [], "сеанс открыт, тени ещё нет");
    world.handoffs.adWatchReport(TAB, 0, AVAILABLE);
    assert.deepEqual(adStates(world), []);
    const begin = world.framePorts.last().posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    assert.deepEqual(await outcome, { kind: "started", player_window: "new" });
  });

  it("с началом тени аренда уходит сразу и продлевается раз в секунду, пока сеанс жив", async () => {
    const world = setup();
    await startHandoff(world);
    assert.deepEqual(world.adMirror.armings, [{ tabId: TAB, leaseMs: AD_WATCH_LEASE_MS }]);
    world.clock.advance(AD_WATCH_RENEW_MS * 5);
    assert.equal(world.adMirror.armings.length, 6);
    assert.ok(world.adMirror.armings.every((arming) => arming.tabId === TAB && arming.leaseMs === AD_WATCH_LEASE_MS));
  });

  it("сеанс кончился — продления прекращаются (фрейм снимет наблюдателя сам по сроку)", async () => {
    const world = setup();
    await startHandoff(world);
    world.sockets.last().receive({ type: "return_to_page", position_us: 0, resume: "paused" });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    const armingsAtEnd = world.adMirror.armings.length;
    world.clock.advance(AD_WATCH_RENEW_MS * 10);
    assert.equal(world.adMirror.armings.length, armingsAtEnd);
    assert.equal(world.clock.liveTimers(), 0);
  });
});

describe("ad_state на проводе", () => {
  it("донесение фрейма уходит плееру, повторы молчат, перемена уходит", async () => {
    const world = setup();
    await startHandoff(world);
    assert.deepEqual(adStates(world), [], "рекламы нет — ничего не объявляем");
    world.handoffs.adWatchReport(TAB, 4, AVAILABLE);
    world.handoffs.adWatchReport(TAB, 4, AVAILABLE);
    world.clock.advance(AD_WATCH_RENEW_MS);
    world.handoffs.adWatchReport(TAB, 4, AVAILABLE);
    world.handoffs.adWatchReport(TAB, 4, ON_PAGE);
    world.clock.advance(AD_STATE_MIN_GAP_MS);
    assert.deepEqual(adStates(world), [AVAILABLE, ON_PAGE]);
  });

  it("фрейм IMA удалён и замолчал: через срок молчания плеер узнаёт «рекламы нет»", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.adWatchReport(TAB, 4, AVAILABLE);
    world.clock.advance(AD_REPORT_SILENCE_MS + AD_WATCH_RENEW_MS);
    assert.deepEqual(adStates(world), [AVAILABLE, NONE]);
  });

  it("пока фрейм продлевает донесения, реклама не гаснет", async () => {
    const world = setup();
    await startHandoff(world);
    for (let second = 0; second < 8; second += 1) {
      world.handoffs.adWatchReport(TAB, 4, AVAILABLE);
      world.clock.advance(AD_WATCH_RENEW_MS);
    }
    assert.deepEqual(adStates(world), [AVAILABLE]);
  });

  it("донесение чужой вкладки не меняет сводку", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.adWatchReport(TAB + 1, 0, AVAILABLE);
    assert.deepEqual(adStates(world), []);
    assert.equal(world.handoffs.isAdOnScreen(TAB), false);
  });

  it("реклама узнана по ролику, органов нет — «реклама есть, органов не знаем»; переезд на фильм пересчитывает сводку", async () => {
    const world = setup([{ frameId: 0, result: [FILM_VIDEO, AD_VIDEO] }]);
    await startHandoff(world);
    assert.deepEqual(adStates(world), [NO_ORGANS]);
    // Реклама кончилась, страница её убрала, сторож принёс фильм: сеанс переезжает.
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    const heldFilm = { kind: "held_video_report" as const, element: FILM_VIDEO.element, candidates: [{ ...FILM_VIDEO, playing: true }], trigger: "next_video" as const };
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm), "adopt");
    const begin = world.framePorts.last().posted.filter((command) => command.kind === "begin_handoff").at(-1);
    assert.ok(begin?.kind === "begin_handoff");
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    world.clock.advance(AD_STATE_MIN_GAP_MS);
    assert.deepEqual(adStates(world), [NO_ORGANS, NONE]);
  });

  it("донесение фрейма сильнее признака сеанса, а его уход возвращает признак", async () => {
    const world = setup([{ frameId: 0, result: [FILM_VIDEO, AD_VIDEO] }]);
    await startHandoff(world);
    world.clock.advance(AD_STATE_MIN_GAP_MS);
    world.handoffs.adWatchReport(TAB, 2, AVAILABLE);
    world.clock.advance(AD_STATE_MIN_GAP_MS);
    world.handoffs.adWatchReport(TAB, 2, NONE);
    assert.deepEqual(adStates(world), [NO_ORGANS, AVAILABLE, NO_ORGANS]);
  });
});

describe("skip_ad", () => {
  it("идёт именно во фрейм, отчитавшийся available, и в нём — один раз", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.adWatchReport(TAB, 2, ON_PAGE);
    world.handoffs.adWatchReport(TAB, 7, AVAILABLE);
    world.sockets.last().receive({ type: "skip_ad" });
    await flushPromises();
    assert.deepEqual(world.adMirror.pressed, [{ tabId: TAB, frameId: 7 }]);
    assert.deepEqual(world.adMirror.notes, []);
  });

  it("устаревшая команда (кнопки уже нет): фрейм ответил no_button — в журнал, ничего другого не нажато", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.adWatchReport(TAB, 7, AVAILABLE);
    world.adMirror.behavior.pressOutcome = "no_button";
    world.sockets.last().receive({ type: "skip_ad" });
    await flushPromises();
    assert.deepEqual(world.adMirror.pressed, [{ tabId: TAB, frameId: 7 }]);
    assert.equal(world.adMirror.notes.length, 1);
    assert.match(world.adMirror.notes[0] ?? "", /no_button/u);
  });

  it("никто не говорил available (только on_page или тишина): не жмём никуда, отметка в журнале", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.adWatchReport(TAB, 2, ON_PAGE);
    world.sockets.last().receive({ type: "skip_ad" });
    world.handoffs.adWatchReport(TAB, 2, NONE);
    world.sockets.last().receive({ type: "skip_ad" });
    await flushPromises();
    assert.deepEqual(world.adMirror.pressed, []);
    assert.equal(world.adMirror.notes.length, 2);
  });

  it("кнопка пропала после донесения: фрейм ушёл в none — команда не уходит никуда", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.adWatchReport(TAB, 7, AVAILABLE);
    world.handoffs.adWatchReport(TAB, 7, NONE);
    world.sockets.last().receive({ type: "skip_ad" });
    assert.deepEqual(world.adMirror.pressed, []);
  });

  it("фрейм недоступен (отказ доставки): в журнал, сеанс жив", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.adWatchReport(TAB, 7, AVAILABLE);
    world.adMirror.behavior.pressOutcome = "pressed";
    const failing = world.adMirror.browser.pressSkipInFrame;
    (world.adMirror.browser as { pressSkipInFrame: typeof failing }).pressSkipInFrame = () => Promise.reject(new Error("no receiver"));
    world.sockets.last().receive({ type: "skip_ad" });
    await flushPromises();
    assert.match(world.adMirror.notes[0] ?? "", /could not reach the frame/u);
    assert.deepEqual(world.ended, []);
  });

  it("до тени команда игнорируется: плеер ещё не может ею командовать", async () => {
    const world = setup();
    world.handoffs.handOffTab(TAB).catch(() => {});
    await flushPromises();
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    world.sockets.last().receive({ type: "skip_ad" });
    await flushPromises();
    assert.deepEqual(world.adMirror.pressed, []);
  });
});

describe("кнопка на странице (on_page)", () => {
  it("skip_ad на неё не нажимает: нажать может только человек, в журнале отметка", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.adWatchReport(TAB, 1, ON_PAGE);
    world.sockets.last().receive({ type: "skip_ad" });
    await flushPromises();
    assert.deepEqual(world.adMirror.pressed, []);
    assert.match(world.adMirror.notes[0] ?? "", /skip_ad ignored/u);
  });
});

describe("приёмник донесений фреймов", () => {
  const EXTENSION_ID = "our-extension";
  const collect = () => {
    const received: { tabId: number; frameId: number; ad: PageAdState }[] = [];
    const listener = createAdWatchReportListener(EXTENSION_ID, (tabId, frameId, ad) => void received.push({ tabId, frameId, ad }));
    return { received, listener };
  };
  const report = { kind: AD_WATCH_REPORT_KIND, ad: AVAILABLE };

  it("вкладку и фрейм называет браузер, а не сообщение", () => {
    const { received, listener } = collect();
    assert.equal(listener(report, { id: EXTENSION_ID, tab: { id: 12 }, frameId: 5 }), false);
    assert.deepEqual(received, [{ tabId: 12, frameId: 5, ad: AVAILABLE }]);
  });

  it("чужой экстеншен, отправитель без вкладки или фрейма, кривая форма — отбрасываются", () => {
    const { received, listener } = collect();
    listener(report, { id: "someone-else", tab: { id: 12 }, frameId: 0 });
    listener(report, { id: EXTENSION_ID, frameId: 0 });
    listener(report, { id: EXTENSION_ID, tab: { id: 12 } });
    listener({ kind: AD_WATCH_REPORT_KIND, ad: { kind: "showing", pod: { kind: "known", index: 9, count: 2 }, skip: { kind: "available" } } }, { id: EXTENSION_ID, tab: { id: 12 }, frameId: 0 });
    listener({ ...report, extra: true }, { id: EXTENSION_ID, tab: { id: 12 }, frameId: 0 });
    assert.deepEqual(received, []);
  });
});

describe("переводчик сообщений плеера", () => {
  it("skip_ad — своё действие, а не «игнорировать»", () => {
    assert.deepEqual(translatePlayerMessage({ type: "skip_ad" }, undefined), { kind: "skip_ad" });
  });
});

describe("обычный сеанс без рекламы остаётся прежним", () => {
  it("ad_state не появляется на проводе, пока рекламы нет", async () => {
    const world = setup([{ frameId: 0, result: [PLAYING_VIDEO] }]);
    await startHandoff(world);
    world.clock.advance(20_000);
    assert.deepEqual(adStates(world), []);
  });
});

describe("дублёр сам по себе", () => {
  const idleBrowser: AdMirrorBrowser = {
    armAdWatch() {},
    pressSkipInFrame: () => Promise.resolve("pressed"),
  };

  it("страница, мигающая числами, даёт не больше одного ad_state в паузу, и последнее слово доходит", () => {
    const clock = createFakeClock();
    const sent: PageAdState[] = [];
    const mirror = createSessionAdMirror({
      currentTabId: () => TAB,
      scheduler: clock.scheduler,
      browser: idleBrowser,
      sessionSaysAdOnScreen: () => false,
      sendAdState: (ad) => void sent.push(ad),
      note: () => {},
    });
    mirror.videoStarted();
    for (let seconds = 20; seconds > 0; seconds -= 1) {
      mirror.acceptFrameReport(1, { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "countdown", seconds } });
      clock.advance(10);
    }
    clock.advance(AD_STATE_MIN_GAP_MS);
    assert.ok(sent.length <= 3, `ушло ${sent.length}`);
    assert.deepEqual(sent.at(-1), { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "countdown", seconds: 1 } });
    mirror.stop();
  });

  it("конец сеанса прямо внутри отправки обрывает цепочку продлений аренды", () => {
    const clock = createFakeClock();
    const armings: number[] = [];
    let adOnScreen = false;
    let mirror: SessionAdMirror | undefined;
    mirror = createSessionAdMirror({
      currentTabId: () => TAB,
      scheduler: clock.scheduler,
      browser: { ...idleBrowser, armAdWatch: (_tab, leaseMs) => void armings.push(leaseMs) },
      sessionSaysAdOnScreen: () => adOnScreen,
      // Закрывающийся сокет синхронно заканчивает сеанс.
      sendAdState: () => mirror?.stop(),
      note: () => {},
    });
    mirror.videoStarted();
    adOnScreen = true;
    clock.advance(AD_WATCH_RENEW_MS);
    const armingsAtEnd = armings.length;
    clock.advance(AD_WATCH_RENEW_MS * 5);
    assert.equal(armings.length, armingsAtEnd, "после конца сеанса аренда не продлевается");
    assert.equal(clock.liveTimers(), 0);
  });
});
