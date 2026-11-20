// Лента одного элемента в модуле краёв ленты (16b3c, решения 374, 381, 382): потеря ролика Shorts, уход по адресу,
// новое поколение страницы вне ленты. Проверяются ответы модуля и просьбы о конце (`leaveFeed`) по часам.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createSessionFeed, FEED_SECTION_LEARN_MS, FEED_SECTION_LEAVE_GRACE_MS, type LeaveFeedAnswer } from "../../../src/background/handoff/feed/session-feed.ts";
import { feedSectionOf } from "../../../src/background/handoff/feed/feed-section.ts";
import { createFakeClock } from "../support.ts";

const SHORT_1 = "https://www.youtube.com/shorts/dQw4w9WgXcQ";
const SHORT_2 = "https://www.youtube.com/shorts/aBcDeFgHiJk";
const HOME = "https://www.youtube.com/";
const BENCH_PAGE = "http://127.0.0.1:41234/pages/feed-edges.html?shape=moved";
const BENCH_SHORT_2 = "http://127.0.0.1:41234/shorts/Sh2aBcDeFgHiJ";
const SERIAL_4 = "https://serial.example/serial/123/4";
const SERIAL_5 = "https://serial.example/serial/123/5";

/** Модуль на фейковых часах; `videoStood` — встала ли уже тень (сеанс вышел из `beginning`). */
function shortsWorld(openedUrl: string, videoStood = true) {
  const clock = createFakeClock();
  const state = { answer: "ended" as LeaveFeedAnswer, feedAwaits: false };
  const leaveRequests: number[] = [];
  const feed = createSessionFeed({
    scheduler: clock.scheduler,
    openedUrl,
    feedAwaitsNextVideo: () => state.feedAwaits,
    leaveFeed: () => {
      leaveRequests.push(clock.scheduler.now());
      return state.answer;
    },
  });
  if (videoStood) feed.videoStarted();
  return { clock, state, feed, leaveRequests };
}

describe("382: страница убрала ролик Shorts, адрес в ленте", () => {
  it("открыт в Shorts, переездов не было — лента продолжается, конца не просим", () => {
    const { feed, clock, leaveRequests } = shortsWorld(SHORT_1);
    feed.addressSeen(SHORT_2); // шаг Shorts: новый адрес в той же ленте
    assert.equal(feed.currentVideoLost(), "feed_continues");
    clock.advance(60_000);
    assert.deepEqual(leaveRequests, []);
  });

  it("стенд: открыт вне ленты, вошёл в неё адресом шага, элемент вынут — не «уход до первого шага»", () => {
    const { feed, clock, leaveRequests } = shortsWorld(BENCH_PAGE);
    feed.addressSeen(BENCH_SHORT_2);
    feed.videoLeftScreen("removed_from_document");
    assert.deepEqual(leaveRequests, [], "адрес Shorts сменился с начала ролика — это шаг ленты, а не уход");
    assert.equal(feed.currentVideoLost(), "feed_continues");
    clock.advance(60_000);
    assert.deepEqual(leaveRequests, []);
  });

  it("сериал: новое поколение и смена адреса вне списка — не лента, решает прежнее правило", () => {
    const { feed, clock, leaveRequests } = shortsWorld(SERIAL_4);
    feed.addressSeen(SERIAL_5);
    assert.equal(feed.pageGenerationBegan(), "forward");
    assert.equal(feed.currentVideoLost(), "not_feed");
    clock.advance(60_000);
    assert.deepEqual(leaveRequests, []);
  });
});

describe("раздел ленты из списка сильнее выученного", () => {
  it("стенд: переезд выучил раздел страницы до входа в Shorts — адрес Shorts не «другой раздел», потеря — ожидание", () => {
    const { feed, clock, state, leaveRequests } = shortsWorld(BENCH_PAGE);
    // Ролик пропал до первого шага (382), переезд ждёт ролик ленты и встаёт на следующий: шаг ленты выучил раздел
    // ещё по адресу страницы стенда, а адрес Shorts пришёл позже окна уточнения.
    state.feedAwaits = true;
    feed.moveToHeldVideoBegan();
    state.feedAwaits = false;
    feed.videoStarted();
    clock.advance(FEED_SECTION_LEARN_MS + 1);
    feed.addressSeen(BENCH_SHORT_2);
    state.feedAwaits = true;
    assert.equal(feed.heldVideoPlace(), "feed_section", "следующий ролик Shorts — та же лента");
    assert.equal(feed.currentVideoLost(), "feed_continues");
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(leaveRequests, []);
  });
});

describe("381: уход из ленты одного элемента по адресу", () => {
  it("адрес главной — конец через отсрочку, даже без потери ролика и без ожидания", () => {
    const { feed, clock, leaveRequests } = shortsWorld(SHORT_1);
    feed.addressSeen(HOME);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS - 1);
    assert.deepEqual(leaveRequests, [], "сразу не кончаем: сайт мог на миг переписать адрес");
    clock.advance(1);
    assert.deepEqual(leaveRequests, [FEED_SECTION_LEAVE_GRACE_MS]);
  });

  it("адрес вернулся в ленту до конца отсрочки — конца нет", () => {
    const { feed, clock, leaveRequests } = shortsWorld(SHORT_1);
    feed.addressSeen(HOME);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS / 2);
    feed.addressSeen(SHORT_2);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(leaveRequests, []);
  });

  it("сеанс посреди переезда (`not_now`) — просьба повторяется после отсрочки, пока не примут", () => {
    const { feed, clock, state, leaveRequests } = shortsWorld(SHORT_1);
    state.answer = "not_now";
    feed.addressSeen(HOME);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    state.answer = "ended";
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.deepEqual(leaveRequests, [FEED_SECTION_LEAVE_GRACE_MS, FEED_SECTION_LEAVE_GRACE_MS * 2]);
  });

  it("тень встала после ухода (переезд начался в ленте) — отсрочка начинается заново и кончает сеанс", () => {
    const { feed, clock, state, leaveRequests } = shortsWorld(SHORT_1);
    state.answer = "not_now";
    feed.addressSeen(HOME);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS / 2);
    state.answer = "ended";
    feed.videoStarted();
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.deepEqual(leaveRequests, [FEED_SECTION_LEAVE_GRACE_MS * 1.5]);
  });

  it("ролик потерян, пока идёт отсрочка, — уход (`left_feed`), а не ожидание следующего", () => {
    const { feed } = shortsWorld(SHORT_1);
    feed.addressSeen(HOME);
    assert.equal(feed.currentVideoLost(), "left_feed");
  });

  it("задержанное видео после ухода — другой раздел: превью главной окно не уводит", () => {
    const { feed } = shortsWorld(SHORT_1);
    feed.addressSeen(HOME);
    assert.equal(feed.heldVideoPlace(), "other_section");
  });

  it("откуда ушли — раздел ленты Shorts, даже если сеанс открыт вне ленты (стенд)", () => {
    const { feed } = shortsWorld(BENCH_PAGE);
    feed.addressSeen(BENCH_SHORT_2);
    feed.addressSeen("http://127.0.0.1:41234/");
    feed.sessionLeftFeed();
    assert.deepEqual(feed.feedLeftPlace(), { kind: "left", section: feedSectionOf(BENCH_SHORT_2) });
  });

  it("после конца сеанса (`stop`) уход по адресу ничего не просит", () => {
    const { feed, clock, leaveRequests } = shortsWorld(SHORT_1);
    feed.stop();
    feed.addressSeen(HOME);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(leaveRequests, []);
  });
});

describe("новое поколение страницы (`pageGenerationBegan`)", () => {
  it("в ленте (шаг Shorts) — отдать плееру, конца не просить", () => {
    const { feed, clock, leaveRequests } = shortsWorld(SHORT_1);
    feed.addressSeen(SHORT_2);
    assert.equal(feed.pageGenerationBegan(), "forward");
    assert.equal(feed.pageGenerationBegan(), "forward", "на шаге Shorts 2–3 поколения подряд");
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(leaveRequests, []);
  });

  it("вне ленты внутри отсрочки (превью на том же элементе) — конец сразу, поколение плееру не отдавать", () => {
    const { feed, clock, leaveRequests } = shortsWorld(SHORT_1);
    feed.addressSeen(HOME);
    clock.advance(400);
    assert.equal(feed.pageGenerationBegan(), "session_ended");
    assert.deepEqual(leaveRequests, [400], "конец не ждёт отсрочки");
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(leaveRequests, [400], "отсрочка снята: второй просьбы нет");
  });

  it("вне ленты, но сеанс переезжает (`not_now`) — поколение отдать (оно у видео переезда), конец — после отсрочки", () => {
    const { feed, clock, state, leaveRequests } = shortsWorld(SHORT_1);
    feed.addressSeen(HOME);
    state.answer = "not_now";
    assert.equal(feed.pageGenerationBegan(), "forward");
    state.answer = "ended";
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.deepEqual(leaveRequests, [0, FEED_SECTION_LEAVE_GRACE_MS]);
  });

  it("тень ещё не вставала (первое видео сеанса встаёт) — поколение отдать, конец не просить", () => {
    const { feed, leaveRequests } = shortsWorld(SHORT_1, false);
    feed.addressSeen(HOME);
    assert.equal(feed.pageGenerationBegan(), "forward");
    assert.deepEqual(leaveRequests, []);
  });

  it("сеанс кончен (`stop`) — отдать (сеанс сам не примет), конец не просить", () => {
    const { feed, leaveRequests } = shortsWorld(SHORT_1);
    feed.addressSeen(HOME);
    feed.stop();
    assert.equal(feed.pageGenerationBegan(), "forward");
    assert.deepEqual(leaveRequests, []);
  });

  it("обычная лента (Instagram) после смены раздела — поколение не судится этим правилом", () => {
    const { feed, leaveRequests } = shortsWorld("https://www.instagram.com/reels/C7xYz1AbCdE/");
    feed.addressSeen("https://www.instagram.com/explore/");
    assert.equal(feed.pageGenerationBegan(), "forward");
    assert.deepEqual(leaveRequests, []);
  });
});
