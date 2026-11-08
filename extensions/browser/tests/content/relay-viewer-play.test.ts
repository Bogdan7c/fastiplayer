// Изолированный мир, режим 2 (сессия 12a): донесение `viewer_play` проходит только при включённой охране,
// ответ фона проверяется, новый сеанс держит видео до длинной страховки, просьба фона доходит до MAIN-мира.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createContentRelay, HELD_VIDEO_REPLY_TIMEOUT_MS, VIEWER_PLAY_REPLY_TIMEOUT_MS } from "../../src/content/relay.ts";
import { NEW_SESSION_ADOPT_INSURANCE_MS, type HeldVideoReport } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { createFakeClock } from "../background/support.ts";

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
  const relay = createContentRelay({
    sendToPage: (message) => void toPage.push(message),
    reportHeldVideo: (report) => {
      reports.push(report);
      return reply() as ReturnType<Parameters<typeof createContentRelay>[0]["reportHeldVideo"]>;
    },
    timers: { setTimeout: (callback, delayMs) => clock.scheduler.setTimeout(callback, delayMs), clearTimeout: (handle) => clock.scheduler.clearTimeout(handle) },
    debugOnce: () => {},
    now: () => clock.scheduler.now(),
    ownDocument: () => ({ hostname: "site.example", origin: "https://site.example" }),
  });
  return { clock, relay, toPage, reports, answerWith: (answer: () => Promise<unknown>) => void (reply = answer) };
}

/** Дать цепочке `then` отработать. */
async function settleMicrotasks(): Promise<void> {
  for (let round = 0; round < 4; round += 1) await Promise.resolve();
}

describe("охрана режима 2: слово изолированного мира MAIN-миру", () => {
  it("`setAutoHandoffWatch` доходит до MAIN-мира, `grantPlayCredit` и `releaseHeldVideo` тоже", () => {
    const { relay, toPage } = setupRelay();
    relay.setAutoHandoffWatch("on");
    relay.grantPlayCredit(30_000);
    relay.releaseHeldVideo(4);
    assert.deepEqual(toPage, [{ kind: "auto_handoff_watch", state: "on" }, { kind: "grant_play_credit", lease_ms: 30_000 }, release]);
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
    relay.setAutoHandoffWatch("on");
    relay.setAutoHandoffWatch("off");
    toPage.length = 0;
    relay.handlePageMessage(viewerHeld);
    assert.deepEqual(reports, []);
    assert.deepEqual(toPage, [release]);
  });

  it("охрана включена: донесение уходит фону с `viewer_play` и хостом, прочитанным здесь; взвод фона не нужен", async () => {
    const { relay, toPage, reports } = setupRelay();
    relay.setAutoHandoffWatch("on");
    toPage.length = 0;
    relay.handlePageMessage(viewerHeld);
    await settleMicrotasks();
    assert.equal(reports.length, 1);
    assert.equal(reports[0]?.trigger, "viewer_play");
    assert.equal(reports[0]?.candidates[0]?.frame_host, "site.example");
    assert.deepEqual(toPage, [], "ответ `adopt`: видео остаётся под тенью");
  });

  it("охрана на `viewer_play` не открывает путь событиям `next_video` без взвода", () => {
    const { relay, toPage, reports } = setupRelay();
    relay.setAutoHandoffWatch("on");
    toPage.length = 0;
    relay.handlePageMessage({ ...viewerHeld, trigger: "next_video" });
    assert.deepEqual(reports, []);
    assert.deepEqual(toPage, [release]);
  });

  it("событие без поля `trigger` — мусор: ни донесения, ни отпускания по этому пути", () => {
    const { relay, reports, toPage } = setupRelay();
    relay.setAutoHandoffWatch("on");
    toPage.length = 0;
    const { trigger: _dropped, ...withoutTrigger } = viewerHeld;
    relay.handlePageMessage(withoutTrigger);
    assert.deepEqual(reports, []);
    assert.equal(relay.counters().rejectedPageMessages, 1);
  });

  it("лимит частоты общий: страница не может молотить фон и через viewer_play", () => {
    const { relay, reports } = setupRelay();
    relay.setAutoHandoffWatch("on");
    for (let attempt = 0; attempt < 100; attempt += 1) relay.handlePageMessage(viewerHeld);
    assert.equal(reports.length, 16);
  });
});

describe("ответ фона на viewer_play", () => {
  it("`release` и ответ не из договора отпускают видео", async () => {
    for (const answer of ["release", "keep_it", 42, undefined, null]) {
      const { relay, toPage, answerWith } = setupRelay();
      answerWith(() => Promise.resolve(answer));
      relay.setAutoHandoffWatch("on");
      toPage.length = 0;
      relay.handlePageMessage(viewerHeld);
      await settleMicrotasks();
      assert.deepEqual(toPage, [release], `ответ ${String(answer)}`);
    }
  });

  it("`adopt`: страховка 2 с — если `begin_handoff` не пришёл, видео отпускается", async () => {
    const { relay, toPage, clock } = setupRelay();
    relay.setAutoHandoffWatch("on");
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
    relay.setAutoHandoffWatch("on");
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
    slow.relay.setAutoHandoffWatch("on");
    slow.toPage.length = 0;
    slow.relay.handlePageMessage(viewerHeld);
    slow.clock.advance(HELD_VIDEO_REPLY_TIMEOUT_MS + 500);
    assert.deepEqual(slow.toPage, [], "2 с для viewer_play мало: фон будит service worker и пробует рекламу");
    answer("adopt_new_session");
    await settleMicrotasks();
    assert.deepEqual(slow.toPage, []);

    const silent = setupRelay();
    silent.answerWith(() => new Promise(() => {}));
    silent.relay.setAutoHandoffWatch("on");
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
