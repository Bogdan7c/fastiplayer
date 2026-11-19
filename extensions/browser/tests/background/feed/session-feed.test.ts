// Края ленты одного сеанса (16b3, правила F2, F3, F5): что значит потеря ролика, когда уход из ленты доказан,
// какой задержанный ролик годится. Проверяется ответ модуля и просьбы о конце (`leaveFeed`), а не вызовы внутри.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  createSessionFeed,
  FEED_EXIT_WINDOW_MS,
  FEED_SECTION_LEARN_MS,
  FEED_SECTION_LEAVE_GRACE_MS,
  PAGE_DETACHED_GRACE_MS,
  type LeaveFeedAnswer,
} from "../../../src/background/handoff/feed/session-feed.ts";
import { DETACHED_ELEMENT_GRACE_MS } from "../../../src/page/shadow/detached-element-watch.ts";
import { createFakeClock } from "../support.ts";

const REEL_1 = "https://www.instagram.com/reels/C7xYz1AbCdE/";
const REEL_2 = "https://www.instagram.com/reels/DBq-9_kLmNo/";
const EXPLORE = "https://www.instagram.com/explore/";

/** Модуль на фейковых часах: переезд «ждёт ролик ленты» и ответ сеанса на просьбу о конце задаёт сценарий. */
function feedWorld(opened: { readonly url: string | undefined } = { url: REEL_1 }) {
  const clock = createFakeClock();
  const state = { feedAwaits: false, answer: "ended" as LeaveFeedAnswer };
  const leaveRequests: number[] = [];
  const feed = createSessionFeed({
    scheduler: clock.scheduler,
    openedUrl: opened.url,
    feedAwaitsNextVideo: () => state.feedAwaits,
    leaveFeed: () => {
      leaveRequests.push(clock.scheduler.now());
      return state.answer;
    },
  });
  feed.videoStarted();
  /** Шаг ленты: наш ролик ушёл с экрана прокруткой, переезд принят, новая тень встала, адрес шага уже пришёл. */
  const takeFeedStep = (): void => {
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.moveToHeldVideoBegan();
    state.feedAwaits = false;
    feed.videoStarted();
    clock.advance(FEED_SECTION_LEARN_MS + 1);
  };
  return { clock, state, feed, leaveRequests, takeFeedStep };
}

describe("окно правила F3 сложено из названных частей", () => {
  it("допуск тени «вне документа» в фоне совпадает с допуском MAIN-мира, окно — около 3 с", () => {
    assert.equal(PAGE_DETACHED_GRACE_MS, DETACHED_ELEMENT_GRACE_MS);
    assert.equal(FEED_EXIT_WINDOW_MS, 3_000);
  });
});

describe("потеря ролика (F2, F3)", () => {
  it("сеанс без шага ленты и без ухода с экрана — не лента, прежнее правило; отсрочки нет", () => {
    const { feed, clock, leaveRequests } = feedWorld();
    feed.addressSeen(EXPLORE);
    assert.equal(feed.currentVideoLost(), "not_feed");
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 10);
    assert.deepEqual(leaveRequests, [], "модуль не трогает конец сеанса, который ему не принадлежит");
  });

  it("F2: после шага ленты ролик пропал без смены адреса — лента продолжается", () => {
    const { feed, takeFeedStep, leaveRequests, clock } = feedWorld();
    takeFeedStep();
    assert.equal(feed.currentVideoLost(), "feed_continues");
    clock.advance(60_000);
    assert.deepEqual(leaveRequests, []);
  });

  it("F3: адрес сменился, и в окне ролик пропал, не уйдя прокруткой, — уход из ленты", () => {
    const { feed, takeFeedStep, clock } = feedWorld();
    takeFeedStep();
    feed.addressSeen(EXPLORE);
    clock.advance(FEED_EXIT_WINDOW_MS);
    assert.equal(feed.currentVideoLost(), "left_feed");
  });

  it("F3: раздел ушёл и вернулся до потери — не уход", () => {
    const { feed, takeFeedStep } = feedWorld();
    takeFeedStep();
    feed.addressSeen(EXPLORE);
    feed.addressSeen(REEL_2);
    assert.equal(feed.currentVideoLost(), "feed_continues");
  });

  it("F3: адрес сменился давно (дальше окна) — это не уход, лента продолжается", () => {
    const { feed, takeFeedStep, clock } = feedWorld();
    takeFeedStep();
    feed.addressSeen(EXPLORE);
    clock.advance(FEED_EXIT_WINDOW_MS + 1);
    assert.equal(feed.currentVideoLost(), "feed_continues", "дальше окна F3 молчит (уход тогда решает отсрочка F5)");
  });

  it("F3: ролик ушёл прокруткой (шаг Instagram), потом адрес и удаление — лента продолжается", () => {
    const { feed, takeFeedStep, state } = feedWorld();
    takeFeedStep();
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(REEL_2);
    assert.equal(feed.currentVideoLost(), "feed_continues");
  });

  it("без шага ленты раздел не выучен: F3 по разделу молчит, вынутый ролик с новым адресом решает правило «до первого шага»", () => {
    const { feed, state, leaveRequests } = feedWorld();
    feed.addressSeen(EXPLORE);
    state.feedAwaits = true;
    feed.videoLeftScreen("removed_from_document");
    assert.equal(leaveRequests.length, 1, "уход до первого шага");
    assert.equal(feed.currentVideoLost(), "feed_continues", "F3 без выученного раздела не отвечает `left_feed`");
  });

  it("адрес, пришедший в окне уточнения после шага, становится разделом ленты; после окна — уже уход", () => {
    const { feed, state, clock, leaveRequests } = feedWorld({ url: "http://127.0.0.1:8080/pages/feed.html" });
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.moveToHeldVideoBegan();
    state.feedAwaits = false;
    feed.videoStarted();
    clock.advance(FEED_SECTION_LEARN_MS - 1);
    feed.addressSeen("http://127.0.0.1:8080/reels/2/");
    clock.advance(2);
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen("http://127.0.0.1:8080/reels/3/");
    assert.equal(feed.heldVideoPlace(), "feed_section");
    feed.addressSeen("http://127.0.0.1:8080/explore/");
    assert.equal(feed.heldVideoPlace(), "other_section");
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.equal(leaveRequests.length, 1);
  });

  it("F3, порядок «удаление → адрес»: другой раздел в окне после потери просит конец сразу, без отсрочки", () => {
    const { feed, takeFeedStep, clock, leaveRequests } = feedWorld();
    takeFeedStep();
    assert.equal(feed.currentVideoLost(), "feed_continues");
    const lossAtMs = clock.scheduler.now();
    clock.advance(FEED_EXIT_WINDOW_MS - 1);
    feed.addressSeen(EXPLORE);
    assert.deepEqual(leaveRequests, [lossAtMs + FEED_EXIT_WINDOW_MS - 1]);
  });

  it("F3 судит раздел, а не адрес: шаг Instagram (адрес следующего ролика) вокруг потери — лента продолжается", () => {
    const { feed, takeFeedStep, clock, leaveRequests } = feedWorld();
    takeFeedStep();
    feed.addressSeen(REEL_2);
    assert.equal(feed.currentVideoLost(), "feed_continues", "адрес раньше потери");
    feed.addressSeen("https://www.instagram.com/reels/EEe_5Zz9QwRt/");
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(leaveRequests, [], "адрес после потери");
  });

  it("F3, «удаление → адрес»: адрес позже окна того же раздела — не уход", () => {
    const { feed, takeFeedStep, clock, leaveRequests } = feedWorld();
    takeFeedStep();
    feed.currentVideoLost();
    clock.advance(FEED_EXIT_WINDOW_MS + 1);
    feed.addressSeen(REEL_2);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(leaveRequests, []);
  });

  it("раздел сменился до того, как встал текущий ролик (это уже его раздел), — не уход", () => {
    const { feed, state, clock } = feedWorld();
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(EXPLORE);
    feed.moveToHeldVideoBegan();
    clock.advance(100);
    state.feedAwaits = false;
    feed.videoStarted();
    clock.advance(500);
    assert.equal(feed.currentVideoLost(), "feed_continues");
  });

  it("раздел сменился до старта ролика, который не был шагом ленты, — окно F3 считает только смены после старта", () => {
    const { feed, takeFeedStep, state, clock } = feedWorld();
    takeFeedStep();
    feed.addressSeen(EXPLORE);
    // Новый ролик встал не шагом ленты (переезд не ждал ролик ленты): выученный раздел прежний.
    state.feedAwaits = false;
    feed.moveToHeldVideoBegan();
    clock.advance(100);
    feed.videoStarted();
    state.feedAwaits = true;
    assert.equal(feed.currentVideoLost(), "feed_continues");
  });

  it("тот же адрес повторно — не смена: окно F3 не открывается", () => {
    const { feed, takeFeedStep } = feedWorld();
    takeFeedStep();
    feed.addressSeen(REEL_1);
    assert.equal(feed.currentVideoLost(), "feed_continues");
  });
});

describe("смена раздела, пока ждём ролик ленты (F5)", () => {
  it("лента ждёт, вкладка ушла в другой раздел — конец после отсрочки, не раньше", () => {
    const { feed, state, clock, leaveRequests, takeFeedStep } = feedWorld();
    takeFeedStep();
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(EXPLORE);
    const leftAtMs = clock.scheduler.now();
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS - 1);
    assert.deepEqual(leaveRequests, []);
    clock.advance(1);
    assert.deepEqual(leaveRequests, [leftAtMs + FEED_SECTION_LEAVE_GRACE_MS]);
  });

  it("раздел вернулся до конца отсрочки — конца нет", () => {
    const { feed, state, clock, leaveRequests, takeFeedStep } = feedWorld();
    takeFeedStep();
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(EXPLORE);
    feed.addressSeen(REEL_2);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 3);
    assert.deepEqual(leaveRequests, []);
  });

  it("ролик встал во время отсрочки — конца нет", () => {
    const { feed, state, clock, leaveRequests, takeFeedStep } = feedWorld();
    takeFeedStep();
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(EXPLORE);
    state.feedAwaits = false;
    feed.videoStarted();
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 3);
    assert.deepEqual(leaveRequests, []);
  });

  it("сеанс посреди переезда (`not_now`) — спросить снова после отсрочки, пока повод в силе", () => {
    const { feed, state, clock, leaveRequests, takeFeedStep } = feedWorld();
    takeFeedStep();
    state.feedAwaits = true;
    state.answer = "not_now";
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(EXPLORE);
    const leftAtMs = clock.scheduler.now();
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    state.answer = "ended";
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.deepEqual(leaveRequests, [leftAtMs + FEED_SECTION_LEAVE_GRACE_MS, leftAtMs + 2 * FEED_SECTION_LEAVE_GRACE_MS]);
  });

  it("смена раздела, когда лента не ждёт (ролик на экране и играет), — ничего", () => {
    const { feed, clock, leaveRequests } = feedWorld();
    feed.addressSeen(EXPLORE);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 3);
    assert.deepEqual(leaveRequests, []);
    assert.equal(feed.heldVideoPlace(), "not_a_feed_move", "суд над разделом — только для переезда ленты");
  });

  it("задержанный ролик: тот же раздел годится, другой — нет", () => {
    const { feed, state, takeFeedStep } = feedWorld();
    takeFeedStep();
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(REEL_2);
    assert.equal(feed.heldVideoPlace(), "feed_section");
    feed.addressSeen(EXPLORE);
    assert.equal(feed.heldVideoPlace(), "other_section");
  });

  it("адрес при старте неизвестен — о разделе судить не по чему: ролик годится, конца нет", () => {
    const { feed, state, clock, leaveRequests } = feedWorld({ url: undefined });
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(EXPLORE);
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 3);
    assert.equal(feed.heldVideoPlace(), "feed_section");
    assert.deepEqual(leaveRequests, []);
  });

  it("после stop() ни новости, ни отсрочка конца не просят", () => {
    const { feed, state, clock, leaveRequests, takeFeedStep } = feedWorld();
    takeFeedStep();
    state.feedAwaits = true;
    feed.videoLeftScreen("scrolled_off_screen");
    feed.addressSeen(EXPLORE);
    feed.stop();
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 3);
    feed.addressSeen(REEL_2);
    assert.deepEqual(leaveRequests, []);
    assert.equal(clock.liveTimers(), 0);
  });
});

describe("пауза окна по слову страницы (досмотр при уходе из ленты)", () => {
  it("слова не было — окно играет; пауза — да; «играть» снова — нет; скорость игру не меняет", () => {
    const { feed } = feedWorld();
    assert.equal(feed.windowPausedByPage(), false);
    feed.pagePlaybackForwarded("pause");
    assert.equal(feed.windowPausedByPage(), true);
    feed.pagePlaybackForwarded("set_rate");
    assert.equal(feed.windowPausedByPage(), true);
    feed.pagePlaybackForwarded("play");
    assert.equal(feed.windowPausedByPage(), false);
  });
});

describe("уход до первого шага ленты (раздел не выучен)", () => {
  it("адрес сменился с начала ролика, потом ролик вынут — конец просится сразу", () => {
    const { feed, state, leaveRequests, clock } = feedWorld();
    feed.addressSeen(EXPLORE);
    clock.advance(500);
    state.feedAwaits = true;
    feed.videoLeftScreen("removed_from_document");
    assert.deepEqual(leaveRequests, [500]);
  });

  it("ролик вынут, адрес пришёл в окне — конец; дальше окна — нет", () => {
    const inWindow = feedWorld();
    inWindow.state.feedAwaits = true;
    inWindow.feed.videoLeftScreen("removed_from_document");
    inWindow.clock.advance(FEED_EXIT_WINDOW_MS);
    inWindow.feed.addressSeen(EXPLORE);
    assert.equal(inWindow.leaveRequests.length, 1);

    const late = feedWorld();
    late.state.feedAwaits = true;
    late.feed.videoLeftScreen("removed_from_document");
    late.clock.advance(FEED_EXIT_WINDOW_MS + 1);
    late.feed.addressSeen(EXPLORE);
    late.clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(late.leaveRequests, []);
  });

  it("шаг TikTok: вынут без смены адреса — не уход; уход прокруткой с новым адресом (Instagram) — не уход", () => {
    const tiktok = feedWorld();
    tiktok.state.feedAwaits = true;
    tiktok.feed.videoLeftScreen("removed_from_document");
    tiktok.clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(tiktok.leaveRequests, []);
    assert.equal(tiktok.feed.heldVideoPlace(), "feed_section");

    const instagram = feedWorld();
    instagram.feed.addressSeen(REEL_2);
    instagram.state.feedAwaits = true;
    instagram.feed.videoLeftScreen("scrolled_off_screen");
    instagram.clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(instagram.leaveRequests, []);
  });

  it("сеанс посреди переезда (`not_now`): пока конец не принят, задержанное видео не берётся", () => {
    const { feed, state, leaveRequests, clock } = feedWorld();
    state.answer = "not_now";
    feed.addressSeen(EXPLORE);
    state.feedAwaits = true;
    feed.videoLeftScreen("removed_from_document");
    assert.equal(feed.heldVideoPlace(), "other_section");
    state.answer = "ended";
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.equal(leaveRequests.length, 2, "спрошено снова после отсрочки");
  });

  it("раздел уже выучен — правило молчит: тот же раздел и вынутый ролик решают F3/F2", () => {
    const { feed, state, takeFeedStep, leaveRequests, clock } = feedWorld();
    takeFeedStep();
    feed.addressSeen(REEL_2);
    state.feedAwaits = true;
    feed.videoLeftScreen("removed_from_document");
    clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(leaveRequests, []);
  });
});

