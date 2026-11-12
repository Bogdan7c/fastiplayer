// Ожидание видео после перезагрузки страницы (16c) — модуль отдельно, на фейковом браузере: вопрос «где вкладка»,
// свежесть адреса, сбои браузера, решение о видео, пауза и остаток срока, уборка подписки.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createSessionPageReload, type PageReloadWaitKind, type ReloadedTabAnswer } from "../../src/background/handoff/session-page-reload.ts";
import type { TabDescription } from "../../src/background/handoff/handoff-session-dependencies.ts";
import type { TabUpdate } from "../../src/background/handoff/episode-change/session-episode-change.ts";
import { playerAbilitiesOf } from "../../src/background/handoff/hls-player-support.ts";
import { CURRENT_PROTOCOL_VERSION } from "../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, type HeldVideoReport } from "../../src/shared/handoff-contract.ts";
import { PLAYING_VIDEO, TAB } from "./handoff-world.ts";
import { createFakeClock, flushPromises } from "./support.ts";

const SESSION_URL = "https://www.example.com/watch?v=1";
const HELD: HeldVideoReport = { kind: HELD_VIDEO_REPORT_KIND, element: PLAYING_VIDEO.element, candidates: [PLAYING_VIDEO], trigger: "next_video" };

function reloadWorld(tab: () => Promise<TabDescription | undefined>) {
  const clock = createFakeClock();
  const listeners = new Set<(update: TabUpdate) => void>();
  const notes: string[] = [];
  const log: string[] = [];
  const reload = createSessionPageReload({
    browser: {
      scheduler: clock.scheduler,
      watchTabUpdates: (listener) => {
        listeners.add(listener);
        return () => void listeners.delete(listener);
      },
      describeTab: tab,
      noteForDeveloper: (message) => void notes.push(message),
    },
    currentTabId: () => TAB,
    sessionPageUrl: () => SESSION_URL,
    midEpisodeWaitMs: 10_000,
    fileAddressRule: () => ({ pageHost: "www.example.com", setting: "block", consentedHost: null }),
    playerAbilities: () => playerAbilitiesOf(CURRENT_PROTOCOL_VERSION),
    siteLeft: () => void log.push("siteLeft"),
    waitExpired: (kind: PageReloadWaitKind) => void log.push(`expired ${kind}`),
  });
  const answers: ReloadedTabAnswer[] = [];
  const urlUpdate = (url: string) => {
    for (const listener of [...listeners]) listener({ tabId: TAB, title: undefined, url });
  };
  return { clock, reload, notes, log, answers, urlUpdate, subscribers: () => listeners.size };
}

/** Вопрос «где вкладка» отвечен «тот же сайт», ожидание начато — как в сеансе. */
async function waiting(kind: PageReloadWaitKind) {
  const world = reloadWorld(() => Promise.resolve({ url: "https://example.com/watch?v=2" }));
  world.reload.askWhereTabWent((answer) => void world.answers.push(answer));
  await flushPromises();
  assert.deepEqual(world.answers, ["same_site"]);
  world.reload.begin(kind);
  return world;
}

describe("ожидание после перезагрузки: модуль", () => {
  it("вопрос: тот же сайт / другой / вкладки нет; сбой браузера — «закрыта» с отметкой, не молча", async () => {
    for (const [tab, expected] of [
      [{ url: "https://example.com/watch?v=2" }, "same_site"],
      [{ url: "https://other-site.test/" }, "other_site"],
      [undefined, "closed"],
    ] as const) {
      const world = reloadWorld(() => Promise.resolve(tab));
      world.reload.askWhereTabWent((answer) => void world.answers.push(answer));
      await flushPromises();
      assert.deepEqual(world.answers, [expected]);
    }
    const failing = reloadWorld(() => Promise.reject(new Error("no tab api")));
    failing.reload.askWhereTabWent((answer) => void failing.answers.push(answer));
    await flushPromises();
    assert.deepEqual(failing.answers, ["closed"]);
    assert.ok(failing.notes.some((note) => note.includes("no tab api")));
  });

  it("новость об адресе во время вопроса новее ответа браузера; повторный вопрос в пути ничего не делает", async () => {
    const world = reloadWorld(() => Promise.resolve({ url: "https://example.com/next" }));
    world.reload.askWhereTabWent((answer) => void world.answers.push(answer));
    world.reload.askWhereTabWent((answer) => void world.answers.push(answer));
    world.urlUpdate("https://other-site.test/");
    await flushPromises();
    assert.deepEqual(world.answers, ["other_site"], "один ответ, по свежему адресу");
  });

  it("вне ожидания видео отпускается; после stop подписки нет и срок не срабатывает", async () => {
    const world = reloadWorld(() => Promise.resolve({ url: SESSION_URL }));
    assert.equal(world.reload.heldVideo(0, HELD), "release");
    world.reload.begin("mid_episode");
    assert.equal(world.subscribers(), 1);
    world.reload.stop();
    world.clock.advance(60_000);
    assert.deepEqual(world.log, []);
    assert.equal(world.subscribers(), 0);
  });

  it("видео того же сайта — замена с его ссылкой; срок на паузе; отказ — остаток; отказ вне передачи — ничего", async () => {
    const world = await waiting("mid_episode");
    world.reload.handoverRefused();
    assert.deepEqual(world.log, [], "передачи не было — ничего");
    world.clock.advance(3_000);
    assert.deepEqual(world.reload.heldVideo(0, HELD), {
      kind: "replace_session",
      video: { frame_id: 0, document: PLAYING_VIDEO.document, element: PLAYING_VIDEO.element },
      consent: { kind: "absent" },
      judged: { role: "film", mayBeAd: false },
    });
    world.clock.advance(2_000);
    assert.deepEqual(world.log, []);
    world.reload.handoverRefused();
    world.clock.advance(4_999);
    assert.deepEqual(world.log, [], "остаток прежнего срока: 10 − 3 − 2 = 5 с");
    world.clock.advance(1);
    assert.deepEqual(world.log, ["expired mid_episode"]);
  });

  it("срок вышел, пока замена открывалась: не срабатывает до отказа, отказ — конец сразу", async () => {
    const world = await waiting("mid_episode");
    assert.notEqual(world.reload.heldVideo(0, HELD), "release");
    world.clock.advance(30_000);
    assert.deepEqual(world.log, [], "замену не обрывают на полпути");
    world.reload.handoverRefused();
    assert.deepEqual(world.log, ["expired mid_episode"]);
  });

  it("адрес ушёл на другой сайт — видео отпускается и сеанс узнаёт об уходе; после конца серии срок 60 с", async () => {
    const world = await waiting("after_episode_end");
    world.urlUpdate("https://other-site.test/");
    assert.deepEqual(world.log, ["siteLeft"]);
    assert.equal(world.reload.heldVideo(0, HELD), "release");
    world.clock.advance(59_999);
    assert.deepEqual(world.log, ["siteLeft"]);
    world.clock.advance(1);
    assert.deepEqual(world.log, ["siteLeft", "expired after_episode_end"]);
  });
});
