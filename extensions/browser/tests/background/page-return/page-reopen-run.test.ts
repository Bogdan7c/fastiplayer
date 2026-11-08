// Открытие ушедшей страницы (сессия 11b2): окно, вкладка, взвод сторожа возврата, срок и причины отказа.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { PAGE_REOPEN_ARM_RETRY_MS, PAGE_REOPEN_DEADLINE_MS } from "../../../src/background/handoff/constants.ts";
import { startPageReopen, type PageReopenOutcome } from "../../../src/background/handoff/page-return/page-reopen-run.ts";
import type { ReturnablePage } from "../../../src/background/handoff/page-return/page-return-memory.ts";
import { NEXT_VIDEO_ARM_LEASE_MS, NEXT_VIDEO_ARM_REFRESH_MS } from "../../../src/background/handoff/video-relocation.ts";
import type { PageReturnTarget } from "../../../src/shared/handoff-contract.ts";
import { createFakeClock, flushPromises } from "../support.ts";
import { BROWSER_WINDOW, createFakePageReopenBrowser, REOPENED_TAB } from "./fake-page-reopen.ts";

const PAGE: ReturnablePage = { url: "https://video.example/watch?v=7", windowId: 3, incognito: false };
const FILM: PageReturnTarget = { kind: "recorded", duration_s: 1_200, start_s: 61 };

function setupRun() {
  const clock = createFakeClock();
  const fake = createFakePageReopenBrowser(PAGE);
  const outcomes: PageReopenOutcome[] = [];
  const homes: number[] = [];
  const notes: string[] = [];
  const start = () =>
    startPageReopen(
      { browser: fake.browser, scheduler: clock.scheduler, note: (line) => void notes.push(line) },
      { page: PAGE, currentTarget: () => FILM },
      { pageIsHere: (tabId) => void homes.push(tabId), finished: (outcome) => void outcomes.push(outcome) },
    );
  return { clock, fake, outcomes, homes, notes, start };
}

describe("открытие страницы: путь удачи", () => {
  it("новая вкладка в окне страницы, владелец узнаёт её номер, сторож взведён с целью; тень встала — звук вкладки вернулся", async () => {
    const { clock, fake, outcomes, homes, start } = setupRun();
    const run = start();
    await flushPromises();
    assert.deepEqual(fake.windowQuestions, [PAGE]);
    assert.deepEqual(fake.opened, [{ windowId: BROWSER_WINDOW, url: PAGE.url }]);
    assert.deepEqual(homes, [REOPENED_TAB], "донесения сторожа новой вкладки пойдут этому сеансу");
    assert.deepEqual(fake.arms, [{ tabId: REOPENED_TAB, leaseMs: NEXT_VIDEO_ARM_LEASE_MS, target: FILM }]);
    clock.advance(NEXT_VIDEO_ARM_REFRESH_MS);
    await flushPromises();
    assert.equal(fake.arms.length, 2, "срок взвода продлевается, пока тень не встала");
    run.shadowStood();
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "returned" }]);
    assert.deepEqual(fake.muted, [{ tabId: REOPENED_TAB, muted: false }], "вкладка снова со звуком: «Return» потом не будет немым");
    assert.deepEqual(fake.closed, []);
    clock.advance(PAGE_REOPEN_DEADLINE_MS);
    await flushPromises();
    assert.equal(fake.arms.length, 2, "после итога взводов нет");
    assert.equal(clock.liveTimers(), 0);
  });

  it("скрипта в новой вкладке ещё нет — взвод чаще, пока страница грузится", async () => {
    const { clock, fake, start } = setupRun();
    fake.behavior.arm = "no_script";
    start();
    await flushPromises();
    clock.advance(PAGE_REOPEN_ARM_RETRY_MS);
    await flushPromises();
    clock.advance(PAGE_REOPEN_ARM_RETRY_MS);
    await flushPromises();
    assert.equal(fake.arms.length, 3);
  });

  it("та же страница уже открыта — берём её: без новой вкладки, а при отказе она остаётся со своим звуком", async () => {
    const { clock, fake, outcomes, homes, start } = setupRun();
    fake.behavior.openTab = 44;
    fake.behavior.openTabWasMuted = true;
    start();
    await flushPromises();
    assert.deepEqual(fake.opened, []);
    assert.deepEqual(fake.broughtForward, [44]);
    assert.deepEqual(homes, [44]);
    clock.advance(PAGE_REOPEN_DEADLINE_MS);
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "failed", reason: "video_did_not_start" }]);
    assert.deepEqual(fake.closed, [], "вкладку зрителя не закрываем");
    assert.deepEqual(fake.muted, [{ tabId: 44, muted: true }], "звук как был до нас");
  });
});

describe("открытие страницы: отказы", () => {
  it("нет окна нужного вида — `no_browser_window`, вкладка не открывается", async () => {
    const { fake, outcomes, start } = setupRun();
    fake.behavior.window = undefined;
    start();
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "failed", reason: "no_browser_window" }]);
    assert.deepEqual(fake.opened, []);
  });

  it("браузер не открыл вкладку — `page_did_not_load`", async () => {
    const { fake, outcomes, start } = setupRun();
    fake.behavior.openFails = true;
    start();
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "failed", reason: "page_did_not_load" }]);
  });

  it("срок вышел, наш скрипт так и не ответил — `page_did_not_load`, нашу вкладку закрываем", async () => {
    const { clock, fake, outcomes, start } = setupRun();
    fake.behavior.arm = "no_script";
    start();
    await flushPromises();
    clock.advance(PAGE_REOPEN_DEADLINE_MS);
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "failed", reason: "page_did_not_load" }]);
    assert.deepEqual(fake.closed, [REOPENED_TAB]);
  });

  it("срок вышел, на странице видео другой длины — `not_the_same_video` (одностраничник)", async () => {
    const { clock, fake, outcomes, start } = setupRun();
    fake.behavior.videos = [{ durationS: 95 }, { durationS: null }];
    start();
    await flushPromises();
    clock.advance(PAGE_REOPEN_DEADLINE_MS);
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "failed", reason: "not_the_same_video" }]);
  });

  it("срок вышел, фильм на месте, но не запустился (сайт ждёт клика) — `video_did_not_start`", async () => {
    const { clock, fake, outcomes, start } = setupRun();
    fake.behavior.videos = [{ durationS: 1_201.5 }];
    start();
    await flushPromises();
    clock.advance(PAGE_REOPEN_DEADLINE_MS - 1);
    await flushPromises();
    assert.deepEqual(outcomes, [], "до срока отказа нет");
    clock.advance(1);
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "failed", reason: "video_did_not_start" }]);
  });

  it("тень на узнанное видео не встала — `video_did_not_start` сразу, итог один", async () => {
    const { clock, outcomes, start } = setupRun();
    const run = start();
    await flushPromises();
    run.shadowDidNotStand();
    run.shadowStood();
    clock.advance(PAGE_REOPEN_DEADLINE_MS);
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "failed", reason: "video_did_not_start" }]);
  });

  it("зритель закрыл нашу вкладку — `page_did_not_load`, закрывать её второй раз не пытаемся; чужая вкладка — не повод", async () => {
    const { fake, outcomes, start } = setupRun();
    const run = start();
    await flushPromises();
    run.tabRemoved(99);
    assert.deepEqual(outcomes, []);
    run.tabRemoved(REOPENED_TAB);
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "failed", reason: "page_did_not_load" }]);
    assert.deepEqual(fake.closed, []);
  });
});

describe("открытие страницы: отмена (окно плеера закрыто во время открытия)", () => {
  it("отмена закрывает открытую нами вкладку и итога не сообщает", async () => {
    const { fake, outcomes, start } = setupRun();
    const run = start();
    await flushPromises();
    run.cancel();
    await flushPromises();
    assert.deepEqual(fake.closed, [REOPENED_TAB]);
    assert.deepEqual(outcomes, []);
  });

  it("отмена, пока браузер ещё открывает вкладку: она закрывается, как только откроется; взводов нет", async () => {
    const { clock, fake, outcomes, homes, start } = setupRun();
    fake.behavior.openHangs = true;
    const run = start();
    await flushPromises();
    run.cancel();
    fake.finishOpening();
    await flushPromises();
    assert.deepEqual(fake.closed, [REOPENED_TAB]);
    assert.deepEqual(homes, [], "отменённая вкладка сеансу не достаётся");
    assert.deepEqual(fake.arms, []);
    assert.deepEqual(outcomes, []);
    assert.equal(clock.liveTimers(), 0);
  });
});
