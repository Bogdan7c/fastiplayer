// Правила смены серии на той же странице (сессия 16a) по отдельности: заголовок, адрес, «ждём следующую»,
// конец по потере источника. Сеанс целиком — в next-episode-session.test.ts.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createFakeClock } from "../support.ts";
import { createNextEpisodeNotice } from "../../../src/background/handoff/episode-change/next-episode-notice.ts";
import { createPageAddressTracker } from "../../../src/background/handoff/episode-change/page-address-tracker.ts";
import { createPageTitleGate, PAGE_TITLE_AWAY_HOLD_MS, PAGE_TITLE_SETTLE_MS } from "../../../src/background/handoff/episode-change/page-title-gate.ts";
import { createPageTitleRelay, PAGE_TITLE_MIN_INTERVAL_MS } from "../../../src/background/handoff/episode-change/page-title-relay.ts";
import { createSessionEpisodeChange, type TabUpdate } from "../../../src/background/handoff/episode-change/session-episode-change.ts";
import { endAfterSourceLoss, sourceLossOfExpiredWait } from "../../../src/background/handoff/episode-change/source-loss-end.ts";
import { MAX_PAGE_TITLE_BYTES, PROTOCOL_VERSION_1_8, PROTOCOL_VERSION_1_9, sanitizePageTitle, type PageSessionMessage, type ProtocolVersion } from "../../../src/protocol/index.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../../src/shared/next-episode-wait.ts";
import { EMPTY_SOURCE_GRACE_MS } from "../../../src/page/shadow/empty-source-watch.ts";

/** Плеер-слушатель: что ушло и можно ли сейчас говорить. */
function fakePlayer(agreed: ProtocolVersion = PROTOCOL_VERSION_1_9) {
  const sent: PageSessionMessage[] = [];
  const state = { listening: true, agreed };
  return {
    sent,
    state,
    ports: { agreedProtocol: () => state.agreed, playerHearsPage: () => state.listening, send: (message: PageSessionMessage) => void sent.push(message) },
  };
}

describe("заголовок вкладки → page_title_changed", () => {
  it("новый заголовок уходит очищенным; тот же после чистки и заголовок open_session — не уходят", () => {
    const player = fakePlayer();
    const relay = createPageTitleRelay({ ...player.ports, scheduler: createFakeClock().scheduler, openedTitle: sanitizePageTitle("Серія 1") });
    assert.equal(relay.titleSeen("Серія 1"), "same_title", "заголовок open_session уже у плеера");
    assert.equal(relay.titleSeen("Серія 2\u0000‮"), "sent");
    assert.equal(relay.titleSeen("Серія 2"), "same_title", "та же строка после чистки");
    assert.deepEqual(player.sent, [{ type: "page_title_changed", title: sanitizePageTitle("Серія 2\u0000‮") }]);
    assert.notEqual(sanitizePageTitle("Серія 2\u0000‮"), "Серія 2\u0000‮", "чистка действительно что-то убрала");
  });

  it("длинный заголовок режется пределом open_session", () => {
    const player = fakePlayer();
    const relay = createPageTitleRelay({ ...player.ports, scheduler: createFakeClock().scheduler, openedTitle: "" });
    relay.titleSeen("я".repeat(MAX_PAGE_TITLE_BYTES));
    const message = player.sent[0];
    assert.ok(message?.type === "page_title_changed");
    assert.ok(new TextEncoder().encode(message.title).length <= MAX_PAGE_TITLE_BYTES);
  });

  it("плеер 1.8 — ни слова; плеер не слушает — ни слова, и «последний отправленный» не сдвигается", () => {
    const old = fakePlayer(PROTOCOL_VERSION_1_8);
    assert.equal(createPageTitleRelay({ ...old.ports, scheduler: createFakeClock().scheduler, openedTitle: "A" }).titleSeen("B"), "player_too_old");
    assert.deepEqual(old.sent, []);

    const away = fakePlayer();
    const relay = createPageTitleRelay({ ...away.ports, scheduler: createFakeClock().scheduler, openedTitle: "A" });
    away.state.listening = false;
    assert.equal(relay.titleSeen("B"), "player_not_listening");
    away.state.listening = true;
    assert.equal(relay.titleSeen("B"), "sent", "непосланное не считается отправленным");
    assert.deepEqual(away.sent, [{ type: "page_title_changed", title: "B" }]);
  });
});

describe("предел частоты page_title_changed", () => {
  function throttled() {
    const clock = createFakeClock();
    const player = fakePlayer();
    const relay = createPageTitleRelay({ ...player.ports, scheduler: clock.scheduler, openedTitle: "A" });
    return { clock, player, relay };
  }
  const titles = (player: ReturnType<typeof fakePlayer>) => player.sent.map((message) => (message.type === "page_title_changed" ? message.title : "?"));

  it("поток из 50 заголовков за секунду — ровно два слова: первый сразу и последний по концу интервала", () => {
    const { clock, player, relay } = throttled();
    assert.equal(relay.titleSeen("T0"), "sent");
    for (let index = 1; index < 50; index += 1) {
      clock.advance(20);
      assert.equal(relay.titleSeen(`T${index}`), "deferred");
    }
    assert.deepEqual(titles(player), ["T0"], "внутри интервала ничего лишнего");
    clock.advance(PAGE_TITLE_MIN_INTERVAL_MS);
    assert.deepEqual(titles(player), ["T0", "T49"]);
    clock.advance(10 * PAGE_TITLE_MIN_INTERVAL_MS);
    assert.equal(player.sent.length, 2, "после замыкающей отправки тишина");
  });

  it("изменение после тишины дольше интервала уходит сразу", () => {
    const { clock, player, relay } = throttled();
    relay.titleSeen("B");
    clock.advance(PAGE_TITLE_MIN_INTERVAL_MS);
    assert.equal(relay.titleSeen("C"), "sent");
    assert.deepEqual(titles(player), ["B", "C"]);
  });

  it("замыкающий заголовок, равный последнему отправленному, не уходит", () => {
    const { clock, player, relay } = throttled();
    relay.titleSeen("B");
    clock.advance(100);
    relay.titleSeen("C");
    clock.advance(100);
    assert.equal(relay.titleSeen("B"), "same_title");
    clock.advance(PAGE_TITLE_MIN_INTERVAL_MS * 3);
    assert.deepEqual(titles(player), ["B"]);
  });

  it("stop() с ждущей замыкающей отправкой — позже ничего не уходит", () => {
    const { clock, player, relay } = throttled();
    relay.titleSeen("B");
    assert.equal(relay.titleSeen("C"), "deferred");
    relay.stop();
    relay.stop();
    clock.advance(PAGE_TITLE_MIN_INTERVAL_MS * 3);
    assert.deepEqual(titles(player), ["B"]);
  });

  it("страница ушла (плеер не слушает) — ждущий заголовок отброшен и после возврата не всплывает", () => {
    const { clock, player, relay } = throttled();
    relay.titleSeen("B");
    relay.titleSeen("C");
    player.state.listening = false;
    clock.advance(PAGE_TITLE_MIN_INTERVAL_MS);
    player.state.listening = true;
    clock.advance(PAGE_TITLE_MIN_INTERVAL_MS * 3);
    assert.deepEqual(titles(player), ["B"]);
  });

  it("сеанс целиком: stop() подписки снимает и ждущий заголовок", () => {
    const clock = createFakeClock();
    const player = fakePlayer();
    const listeners: Array<(update: TabUpdate) => void> = [];
    const change = createSessionEpisodeChange({
      ...player.ports,
      scheduler: clock.scheduler,
      watchTabUpdates: (listener) => {
        listeners.push(listener);
        return () => void listeners.splice(0);
      },
      currentTabId: () => 1,
      openedPage: { title: "A", url: undefined },
    });
    listeners[0]?.({ tabId: 1, title: "B", url: undefined });
    clock.advance(PAGE_TITLE_SETTLE_MS);
    listeners[0]?.({ tabId: 1, title: "C", url: undefined });
    clock.advance(PAGE_TITLE_SETTLE_MS);
    change.stop();
    clock.advance(PAGE_TITLE_MIN_INTERVAL_MS * 3);
    assert.deepEqual(titles(player), ["B"]);
  });
});

describe("«ждём следующую серию» → awaiting_next_episode один раз на видео", () => {
  it("два начала ожидания одной серии — одно слово; новое видео — снова слово", () => {
    const player = fakePlayer();
    const notice = createNextEpisodeNotice(player.ports);
    assert.equal(notice.waitBegan(), "sent");
    assert.equal(notice.waitBegan(), "already_sent");
    notice.videoBegan();
    assert.equal(notice.waitBegan(), "sent");
    assert.deepEqual(player.sent, [
      { type: "awaiting_next_episode", patience_ms: NEXT_EPISODE_WAIT_MS },
      { type: "awaiting_next_episode", patience_ms: NEXT_EPISODE_WAIT_MS },
    ]);
  });

  it("плееру 1.8 и не слушающему плееру — ничего", () => {
    const old = fakePlayer(PROTOCOL_VERSION_1_8);
    assert.equal(createNextEpisodeNotice(old.ports).waitBegan(), "player_too_old");
    const away = fakePlayer();
    away.state.listening = false;
    assert.equal(createNextEpisodeNotice(away.ports).waitBegan(), "player_not_listening");
    assert.deepEqual([...old.sent, ...away.sent], []);
  });
});

describe("адрес вкладки с начала видео", () => {
  it("смена адреса после начала видео — «сменился»; новое видео делает новый адрес опорным", () => {
    const tracker = createPageAddressTracker("https://site.example/serial/1");
    assert.equal(tracker.sinceVideoBegan(), "unchanged");
    tracker.addressSeen("https://site.example/serial/1");
    assert.equal(tracker.sinceVideoBegan(), "unchanged", "тот же адрес повторно — не смена");
    tracker.addressSeen("https://site.example/serial/2");
    assert.equal(tracker.sinceVideoBegan(), "changed");
    tracker.videoBegan();
    assert.equal(tracker.sinceVideoBegan(), "unchanged", "серия 2 началась на своём адресе");
    tracker.addressSeen("https://site.example/#/home");
    assert.equal(tracker.sinceVideoBegan(), "changed", "хеш-маршрут — тоже уход");
  });

  it("браузер не назвал адрес при старте — судить не о чем: «не сменился» до первого опорного", () => {
    const tracker = createPageAddressTracker(undefined);
    tracker.addressSeen("https://site.example/a");
    assert.equal(tracker.sinceVideoBegan(), "unchanged");
    tracker.videoBegan();
    tracker.addressSeen("https://site.example/b");
    assert.equal(tracker.sinceVideoBegan(), "changed");
  });
});

describe("конец по потере источника", () => {
  const facts = (pageAddress: "unchanged" | "changed", agreedProtocol: ProtocolVersion = PROTOCOL_VERSION_1_9) => ({ pageAddress, agreedProtocol });

  it("посреди ролика — 4009; после конца серии — 4011 (1.9) или 4009 (1.8); адрес сменился — уход", () => {
    assert.deepEqual(endAfterSourceLoss("video_taken_back", facts("unchanged")), { kind: "end_on_page_side", closeReason: "page_took_video_back", end: { kind: "page_took_video_back" } });
    assert.deepEqual(endAfterSourceLoss("next_episode_not_started", facts("unchanged")), {
      kind: "end_on_page_side",
      closeReason: "next_episode_not_started",
      end: { kind: "next_episode_not_started" },
    });
    assert.deepEqual(endAfterSourceLoss("next_episode_not_started", facts("unchanged", PROTOCOL_VERSION_1_8)), {
      kind: "end_on_page_side",
      closeReason: "page_took_video_back",
      end: { kind: "next_episode_not_started" },
    });
    for (const loss of ["video_taken_back", "next_episode_not_started"] as const) {
      assert.deepEqual(endAfterSourceLoss(loss, facts("changed")), { kind: "page_navigated_away" }, loss);
    }
  });

  it("истёкшее ожидание переезда: после конца серии — «не поставила следующую», иначе — «забрала видео»", () => {
    assert.equal(sourceLossOfExpiredWait("after_episode_end"), "next_episode_not_started");
    assert.equal(sourceLossOfExpiredWait("next_video"), "video_taken_back");
  });
});

describe("подписка сеанса на вкладки", () => {
  function subscription() {
    const listeners = new Set<(update: TabUpdate) => void>();
    const watch = (listener: (update: TabUpdate) => void) => {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    };
    const emit = (update: TabUpdate) => listeners.forEach((listener) => listener(update));
    return { listeners, watch, emit };
  }

  it("чужая вкладка не трогает ни заголовок, ни адрес; stop снимает подписку один раз", () => {
    const tabs = subscription();
    const player = fakePlayer();
    const clock = createFakeClock();
    let currentTab = 7;
    const change = createSessionEpisodeChange({
      ...player.ports,
      scheduler: clock.scheduler,
      watchTabUpdates: tabs.watch,
      currentTabId: () => currentTab,
      openedPage: { title: "A", url: "https://site.example/1" },
    });
    tabs.emit({ tabId: 8, title: "Other", url: "https://other.example/" });
    assert.deepEqual(player.sent, []);
    assert.equal(change.pageAddressSinceVideoBegan(), "unchanged");
    currentTab = 8; // страница вернулась в другую вкладку (11b2): теперь это вкладка сеанса
    tabs.emit({ tabId: 8, title: "B", url: undefined });
    clock.advance(PAGE_TITLE_SETTLE_MS);
    assert.deepEqual(player.sent, [{ type: "page_title_changed", title: "B" }]);
    change.stop();
    change.stop();
    assert.equal(tabs.listeners.size, 0);
    tabs.emit({ tabId: 8, title: "C", url: "https://site.example/2" });
    assert.equal(player.sent.length, 1, "после конца сеанса — тишина");
  });
});

describe("ворота заголовка: адрес той же навигации и новое видео (16a3)", () => {
  function gate(addressSinceVideoBegan: () => "unchanged" | "changed") {
    const clock = createFakeClock();
    const relayed: string[] = [];
    const titles = createPageTitleGate({ scheduler: clock.scheduler, addressSinceVideoBegan, relay: (title) => void relayed.push(title) });
    return { clock, relayed, titles };
  }

  it("адрес тот же: заголовок уходит после ожидания адреса, не раньше; из пачки — только последний", () => {
    const world = gate(() => "unchanged");
    world.titles.titleSeen("Серія 2");
    world.titles.titleSeen("Серія 2 · Озвучка");
    world.clock.advance(PAGE_TITLE_SETTLE_MS - 1);
    assert.deepEqual(world.relayed, [], "адрес той же навигации ещё может прийти");
    world.clock.advance(1);
    assert.deepEqual(world.relayed, ["Серія 2 · Озвучка"]);
  });

  it("заголовок раньше адреса (уход на главную): адрес сменился в окне ожидания — заголовок ждёт, конец сеанса его выбрасывает", () => {
    let address: "unchanged" | "changed" = "unchanged";
    const world = gate(() => address);
    world.titles.titleSeen("YouTube");
    address = "changed"; // браузер сообщил адрес главной вторым событием
    world.clock.advance(PAGE_TITLE_SETTLE_MS + EMPTY_SOURCE_GRACE_MS);
    assert.deepEqual(world.relayed, [], "окно досматривает прежний ролик: его название остаётся");
    world.titles.stop(); // уход признан (`tab_gone`): сеанс кончился
    world.titles.videoBegan();
    world.clock.advance(PAGE_TITLE_SETTLE_MS * 5);
    assert.deepEqual(world.relayed, [], "после конца сеанса заголовок ушедшей страницы не уходит никогда");
  });

  it("адрес раньше заголовка (новый ролик одностраничника): заголовок ждёт и уходит, когда новое видео началось", () => {
    let address: "unchanged" | "changed" = "changed";
    const world = gate(() => address);
    world.titles.titleSeen("Ролик 2");
    world.clock.advance(PAGE_TITLE_SETTLE_MS * 2);
    assert.deepEqual(world.relayed, []);
    world.titles.titleSeen("(1) Ролик 2");
    address = "unchanged"; // сеанс сначала обновляет опорный адрес, потом зовёт ворота
    world.titles.videoBegan();
    assert.deepEqual(world.relayed, ["(1) Ролик 2"], "самый свежий заголовок — сразу с началом видео");
    world.titles.videoBegan();
    assert.deepEqual(world.relayed, ["(1) Ролик 2"], "второе начало без нового заголовка ничего не шлёт");
  });
});

describe("ворота заголовка: новое видео началось раньше смены адреса (16a3)", () => {
  it("заголовок нового ролика не застревает: уходит по сроку удержания, если сеанс жив", () => {
    const clock = createFakeClock();
    const relayed: string[] = [];
    // Порядок «новое поколение → адрес → заголовок»: опорный адрес остался от прошлого ролика.
    const titles = createPageTitleGate({ scheduler: clock.scheduler, addressSinceVideoBegan: () => "changed", relay: (title) => void relayed.push(title) });
    titles.videoBegan();
    titles.titleSeen("Ролик 3");
    clock.advance(PAGE_TITLE_SETTLE_MS + PAGE_TITLE_AWAY_HOLD_MS - 1);
    assert.deepEqual(relayed, []);
    clock.advance(1);
    assert.deepEqual(relayed, ["Ролик 3"]);
    titles.titleSeen("Ролик 3 · 2");
    clock.advance(PAGE_TITLE_SETTLE_MS + PAGE_TITLE_AWAY_HOLD_MS);
    assert.deepEqual(relayed, ["Ролик 3", "Ролик 3 · 2"], "следующий заголовок — с новым сроком");
  });

  it("срок удержания дольше допуска пустого источника тени с запасом на тики: уход успевает раньше", () => {
    assert.ok(PAGE_TITLE_AWAY_HOLD_MS >= EMPTY_SOURCE_GRACE_MS + 2_000, `${PAGE_TITLE_AWAY_HOLD_MS} vs ${EMPTY_SOURCE_GRACE_MS}`);
  });
});
