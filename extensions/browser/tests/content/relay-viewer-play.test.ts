// Изолированный мир, режим 2 (сессия 12a): донесение `viewer_play` проходит только при включённой охране,
// ответ фона проверяется, новый сеанс держит видео до длинной страховки, просьба фона доходит до MAIN-мира.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createContentRelay, HELD_VIDEO_REPLY_TIMEOUT_MS, VIEWER_PLAY_REPLY_TIMEOUT_MS } from "../../src/content/relay.ts";
import { NEW_SESSION_ADOPT_INSURANCE_MS, type HeldVideoReport } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { createFakeClock } from "../background/support.ts";
import type { ViewerOfferChoiceMessage } from "../../src/shared/viewer-offer-contract.ts";
import { AUTO_HANDOFF_WATCH_OFF, WATCH_ON, WATCH_ON_UNFILTERED } from "../support/auto-handoff-watch-fixtures.ts";

const viewerHeld = {
  kind: "next_video_held",
  element: 4,
  candidates: [{ ...DEFAULT_CANDIDATE_FACTS, document: "doc-a", element: 4, media: { kind: "none" }, playing: true, visible_area_px: 1, frame_host: "forged.example" }],
  trigger: "viewer_play",
};
const release = { kind: "release_held_video", element: 4 };

function setupRelay() {
  const clock = createFakeClock();
  const toPage: unknown[] = [];
  const reports: HeldVideoReport[] = [];
  let reply: () => Promise<unknown> = () => Promise.resolve("adopt");
  const choices: ViewerOfferChoiceMessage[] = [];
  let choiceReply: () => Promise<unknown> = () => Promise.resolve("hand_off");
  const relay = createContentRelay({
    sendToPage: (message) => void toPage.push(message),
    reportHeldVideo: (report) => {
      reports.push(report);
      return reply() as ReturnType<Parameters<typeof createContentRelay>[0]["reportHeldVideo"]>;
    },
    sendViewerOfferChoice: (message) => {
      choices.push(message);
      return choiceReply();
    },
    timers: { setTimeout: (callback, delayMs) => clock.scheduler.setTimeout(callback, delayMs), clearTimeout: (handle) => clock.scheduler.clearTimeout(handle) },
    debugOnce: () => {},
    now: () => clock.scheduler.now(),
    ownDocument: () => ({ hostname: "site.example", origin: "https://site.example" }),
  });
  return {
    clock,
    relay,
    toPage,
    reports,
    choices,
    answerWith: (answer: () => Promise<unknown>) => void (reply = answer),
    answerChoiceWith: (answer: () => Promise<unknown>) => void (choiceReply = answer),
  };
}

/** Дать цепочке `then` отработать. */
async function settleMicrotasks(): Promise<void> {
  for (let round = 0; round < 4; round += 1) await Promise.resolve();
}

describe("охрана режима 2: слово изолированного мира MAIN-миру", () => {
  it("`setAutoHandoffWatch` доходит до MAIN-мира, `grantPlayCredit` и `releaseHeldVideo` тоже", () => {
    const { relay, toPage } = setupRelay();
    relay.setAutoHandoffWatch(WATCH_ON);
    relay.grantPlayCredit(30_000);
    relay.releaseHeldVideo(4);
    assert.deepEqual(toPage, [{ kind: "auto_handoff_watch", ...WATCH_ON }, { kind: "grant_play_credit", lease_ms: 30_000 }, release]);
  });
});

describe("донесение viewer_play", () => {
  it("охрана выключена (страница подделала событие): фон не будится, видео отпускается", () => {
    const { relay, toPage, reports } = setupRelay();
    relay.handlePageMessage(viewerHeld);
    assert.deepEqual(reports, []);
    assert.deepEqual(toPage, [release]);
  });

  it("охрана была включена, а потом выключена: донесение снова не проходит", () => {
    const { relay, toPage, reports } = setupRelay();
    relay.setAutoHandoffWatch(WATCH_ON);
    relay.setAutoHandoffWatch(AUTO_HANDOFF_WATCH_OFF);
    toPage.length = 0;
    relay.handlePageMessage(viewerHeld);
    assert.deepEqual(reports, []);
    assert.deepEqual(toPage, [release]);
  });

  it("охрана включена: донесение уходит фону с `viewer_play` и хостом, прочитанным здесь; взвод фона не нужен", async () => {
    const { relay, toPage, reports } = setupRelay();
    relay.setAutoHandoffWatch(WATCH_ON);
    toPage.length = 0;
    relay.handlePageMessage(viewerHeld);
    await settleMicrotasks();
    assert.equal(reports.length, 1);
    assert.equal(reports[0]?.trigger, "viewer_play");
    assert.equal(reports[0]?.candidates[0]?.frame_host, "site.example");
    assert.deepEqual(toPage, [], "ответ `adopt`: видео остаётся под тенью");
  });

  it("охрана, задерживающая и беззвучные (общий фильтр звука выключен, решения 260–261), тоже пропускает донесение `viewer_play` фону", async () => {
    const { relay, toPage, reports } = setupRelay();
    relay.setAutoHandoffWatch(WATCH_ON_UNFILTERED);
    assert.deepEqual(toPage, [{ kind: "auto_handoff_watch", ...WATCH_ON_UNFILTERED }], "MAIN-мир узнал, что беззвучные задерживаются");
    toPage.length = 0;
    relay.handlePageMessage(viewerHeld);
    await settleMicrotasks();
    assert.equal(reports.length, 1);
    assert.equal(reports[0]?.trigger, "viewer_play");
  });

  it("охрана на `viewer_play` не открывает путь событиям `next_video` без взвода", () => {
    const { relay, toPage, reports } = setupRelay();
    relay.setAutoHandoffWatch(WATCH_ON);
    toPage.length = 0;
    relay.handlePageMessage({ ...viewerHeld, trigger: "next_video" });
    assert.deepEqual(reports, []);
    assert.deepEqual(toPage, [release]);
  });

  it("событие без поля `trigger` — мусор: ни донесения, ни отпускания по этому пути", () => {
    const { relay, reports, toPage } = setupRelay();
    relay.setAutoHandoffWatch(WATCH_ON);
    toPage.length = 0;
    const { trigger: _dropped, ...withoutTrigger } = viewerHeld;
    relay.handlePageMessage(withoutTrigger);
    assert.deepEqual(reports, []);
    assert.equal(relay.counters().rejectedPageMessages, 1);
  });

  it("лимит частоты общий: страница не может молотить фон и через viewer_play", () => {
    const { relay, reports } = setupRelay();
    relay.setAutoHandoffWatch(WATCH_ON);
    for (let attempt = 0; attempt < 100; attempt += 1) relay.handlePageMessage(viewerHeld);
    assert.equal(reports.length, 16);
  });
});

describe("ответ фона на viewer_play", () => {
  it("`release` и ответ не из договора отпускают видео", async () => {
    for (const answer of ["release", "keep_it", 42, undefined, null]) {
      const { relay, toPage, answerWith } = setupRelay();
      answerWith(() => Promise.resolve(answer));
      relay.setAutoHandoffWatch(WATCH_ON);
      toPage.length = 0;
      relay.handlePageMessage(viewerHeld);
      await settleMicrotasks();
      assert.deepEqual(toPage, [release], `ответ ${String(answer)}`);
    }
  });

  it("`adopt`: страховка 2 с — если `begin_handoff` не пришёл, видео отпускается", async () => {
    const { relay, toPage, clock } = setupRelay();
    relay.setAutoHandoffWatch(WATCH_ON);
    toPage.length = 0;
    relay.handlePageMessage(viewerHeld);
    await settleMicrotasks();
    clock.advance(HELD_VIDEO_REPLY_TIMEOUT_MS - 1);
    assert.deepEqual(toPage, []);
    clock.advance(1);
    assert.deepEqual(toPage, [release]);
  });

  it("`adopt_new_session`: видео ждёт долгую страховку, а не 2 с", async () => {
    const { relay, toPage, clock, answerWith } = setupRelay();
    answerWith(() => Promise.resolve("adopt_new_session"));
    relay.setAutoHandoffWatch(WATCH_ON);
    toPage.length = 0;
    relay.handlePageMessage(viewerHeld);
    await settleMicrotasks();
    clock.advance(HELD_VIDEO_REPLY_TIMEOUT_MS * 3);
    assert.deepEqual(toPage, [], "сеанс стартует до десятков секунд: видео держим");
    clock.advance(NEW_SESSION_ADOPT_INSURANCE_MS - HELD_VIDEO_REPLY_TIMEOUT_MS * 3);
    assert.deepEqual(toPage, [release], "фон умер: страховка отпустила");
  });

  it("фон решает дольше обычного: ответ в пределах 5 с ещё принимается, молчание дольше — отпускает", async () => {
    const slow = setupRelay();
    let answer: (value: unknown) => void = () => {};
    slow.answerWith(() => new Promise((resolve) => void (answer = resolve)));
    slow.relay.setAutoHandoffWatch(WATCH_ON);
    slow.toPage.length = 0;
    slow.relay.handlePageMessage(viewerHeld);
    slow.clock.advance(HELD_VIDEO_REPLY_TIMEOUT_MS + 500);
    assert.deepEqual(slow.toPage, [], "2 с для viewer_play мало: фон будит service worker и пробует рекламу");
    answer("adopt_new_session");
    await settleMicrotasks();
    assert.deepEqual(slow.toPage, []);

    const silent = setupRelay();
    silent.answerWith(() => new Promise(() => {}));
    silent.relay.setAutoHandoffWatch(WATCH_ON);
    silent.toPage.length = 0;
    silent.relay.handlePageMessage(viewerHeld);
    silent.clock.advance(VIEWER_PLAY_REPLY_TIMEOUT_MS);
    assert.deepEqual(silent.toPage, [release]);
  });

  it("обычный `next_video` держит прежний срок ответа 2 с", () => {
    const { relay, toPage, clock, answerWith } = setupRelay();
    answerWith(() => new Promise(() => {}));
    relay.armNextVideo(10_000);
    toPage.length = 0;
    relay.handlePageMessage({ ...viewerHeld, trigger: "next_video" });
    clock.advance(HELD_VIDEO_REPLY_TIMEOUT_MS);
    assert.deepEqual(toPage, [release]);
  });
});

describe("предложение «Ask» (12b)", () => {
  /** Охрана включена, донесение ушло, фон ответил «спроси». */
  async function askedRelay() {
    const world = setupRelay();
    world.relay.setAutoHandoffWatch(WATCH_ON);
    world.answerWith(() => Promise.resolve("ask_viewer"));
    world.toPage.length = 0;
    world.relay.handlePageMessage(viewerHeld);
    await settleMicrotasks();
    return world;
  }

  it("ответ ask_viewer: MAIN-миру «держи под предложением», страховочного отпускания нет вовсе", async () => {
    const { toPage, clock } = await askedRelay();
    assert.deepEqual(toPage, [{ kind: "hold_for_viewer_offer", element: 4 }]);
    clock.advance(10 * NEW_SESSION_ADOPT_INSURANCE_MS);
    assert.deepEqual(toPage, [{ kind: "hold_for_viewer_offer", element: 4 }], "решение 256: висит, пока человек не выберет");
  });

  it("выбор уходит фону с меткой документа и ключом из донесения; «в плеер» — ждём begin_handoff, страховка 45 с", async () => {
    const { relay, toPage, choices, clock } = await askedRelay();
    toPage.length = 0;
    assert.equal(await relay.answerViewerOffer("always"), "hand_off");
    assert.deepEqual(choices, [{ kind: "viewer_offer_choice", document: "doc-a", element: 4, choice: "always" }]);
    assert.deepEqual(toPage, []);
    clock.advance(NEW_SESSION_ADOPT_INSURANCE_MS);
    assert.deepEqual(toPage, [release], "фон умер до begin_handoff — видео не остаётся под тенью");
  });

  it("«здесь», «фон не узнал», ответ не из договора и недоступный фон — видео отпускается и играет", async () => {
    const answers: readonly { reply: () => Promise<unknown>; delivery: string }[] = [
      { reply: () => Promise.resolve("play_here"), delivery: "played_here" },
      { reply: () => Promise.resolve("no_offer"), delivery: "played_here" },
      { reply: () => Promise.resolve("sure, whatever"), delivery: "played_here" },
      { reply: () => Promise.reject(new Error("background asleep")), delivery: "send_failed" },
    ];
    for (const row of answers) {
      const { relay, toPage, answerChoiceWith } = await askedRelay();
      answerChoiceWith(row.reply);
      toPage.length = 0;
      assert.equal(await relay.answerViewerOffer("here"), row.delivery);
      assert.deepEqual(toPage, [release], row.delivery);
    }
  });

  it("«спроси» на донесение переезда (не от зрителя) — видео отпускается, а не висит под тенью без панели", async () => {
    const { relay, toPage, answerWith } = setupRelay();
    relay.armNextVideo(3_000);
    answerWith(() => Promise.resolve("ask_viewer"));
    toPage.length = 0;
    relay.handlePageMessage({ ...viewerHeld, trigger: "next_video" });
    await settleMicrotasks();
    assert.deepEqual(toPage, [release]);
  });

  it("второй клик по тому же предложению ничего не шлёт; без предложения — no_offer", async () => {
    const { relay, choices } = await askedRelay();
    await relay.answerViewerOffer("here");
    assert.equal(await relay.answerViewerOffer("fastiplayer"), "no_offer");
    assert.equal(choices.length, 1);
    const fresh = setupRelay();
    assert.equal(await fresh.relay.answerViewerOffer("here"), "no_offer");
    assert.deepEqual(fresh.choices, []);
  });

  it("фон снял предложение: MAIN-миру «сними», и выбор по снятому уже не уходит", async () => {
    const { relay, toPage, choices } = await askedRelay();
    toPage.length = 0;
    relay.withdrawViewerOffer(4);
    assert.deepEqual(toPage, [{ kind: "withdraw_viewer_offer", element: 4 }]);
    assert.equal(await relay.answerViewerOffer("fastiplayer"), "no_offer");
    assert.deepEqual(choices, []);
  });
});
