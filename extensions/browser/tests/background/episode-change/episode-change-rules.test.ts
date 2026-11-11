// Правила смены серии на той же странице (сессия 16a) по отдельности: заголовок, адрес, «ждём следующую»,
// конец по потере источника. Сеанс целиком — в next-episode-session.test.ts.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createNextEpisodeNotice } from "../../../src/background/handoff/episode-change/next-episode-notice.ts";
import { createPageAddressTracker } from "../../../src/background/handoff/episode-change/page-address-tracker.ts";
import { createPageTitleRelay } from "../../../src/background/handoff/episode-change/page-title-relay.ts";
import { createSessionEpisodeChange, type TabUpdate } from "../../../src/background/handoff/episode-change/session-episode-change.ts";
import { endAfterSourceLoss, sourceLossOfExpiredWait } from "../../../src/background/handoff/episode-change/source-loss-end.ts";
import { MAX_PAGE_TITLE_BYTES, PROTOCOL_VERSION_1_8, PROTOCOL_VERSION_1_9, sanitizePageTitle, type PageSessionMessage, type ProtocolVersion } from "../../../src/protocol/index.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../../src/shared/next-episode-wait.ts";

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
    const relay = createPageTitleRelay({ ...player.ports, openedTitle: sanitizePageTitle("Серія 1") });
    assert.equal(relay.titleSeen("Серія 1"), "same_title", "заголовок open_session уже у плеера");
    assert.equal(relay.titleSeen("Серія 2\u0000‮"), "sent");
    assert.equal(relay.titleSeen("Серія 2"), "same_title", "та же строка после чистки");
    assert.deepEqual(player.sent, [{ type: "page_title_changed", title: sanitizePageTitle("Серія 2\u0000‮") }]);
    assert.notEqual(sanitizePageTitle("Серія 2\u0000‮"), "Серія 2\u0000‮", "чистка действительно что-то убрала");
  });

  it("длинный заголовок режется пределом open_session", () => {
    const player = fakePlayer();
    const relay = createPageTitleRelay({ ...player.ports, openedTitle: "" });
    relay.titleSeen("я".repeat(MAX_PAGE_TITLE_BYTES));
    const message = player.sent[0];
    assert.ok(message?.type === "page_title_changed");
    assert.ok(new TextEncoder().encode(message.title).length <= MAX_PAGE_TITLE_BYTES);
  });

  it("плеер 1.8 — ни слова; плеер не слушает — ни слова, и «последний отправленный» не сдвигается", () => {
    const old = fakePlayer(PROTOCOL_VERSION_1_8);
    assert.equal(createPageTitleRelay({ ...old.ports, openedTitle: "A" }).titleSeen("B"), "player_too_old");
    assert.deepEqual(old.sent, []);

    const away = fakePlayer();
    const relay = createPageTitleRelay({ ...away.ports, openedTitle: "A" });
    away.state.listening = false;
    assert.equal(relay.titleSeen("B"), "player_not_listening");
    away.state.listening = true;
    assert.equal(relay.titleSeen("B"), "sent", "непосланное не считается отправленным");
    assert.deepEqual(away.sent, [{ type: "page_title_changed", title: "B" }]);
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
    let currentTab = 7;
    const change = createSessionEpisodeChange({
      ...player.ports,
      watchTabUpdates: tabs.watch,
      currentTabId: () => currentTab,
      openedPage: { title: "A", url: "https://site.example/1" },
    });
    tabs.emit({ tabId: 8, title: "Other", url: "https://other.example/" });
    assert.deepEqual(player.sent, []);
    assert.equal(change.pageAddressSinceVideoBegan(), "unchanged");
    currentTab = 8; // страница вернулась в другую вкладку (11b2): теперь это вкладка сеанса
    tabs.emit({ tabId: 8, title: "B", url: undefined });
    assert.deepEqual(player.sent, [{ type: "page_title_changed", title: "B" }]);
    change.stop();
    change.stop();
    assert.equal(tabs.listeners.size, 0);
    tabs.emit({ tabId: 8, title: "C", url: "https://site.example/2" });
    assert.equal(player.sent.length, 1, "после конца сеанса — тишина");
  });
});
