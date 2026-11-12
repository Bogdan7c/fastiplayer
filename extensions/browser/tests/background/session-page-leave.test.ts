// Уход страницы сеанса (вынесено из сеанса в 16a): порт умер, вкладку закрыли, ушли со страницы.
// Проверяем результат (какие шаги сделал сеанс и чего не трогал), а не вызовы по отдельности.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { TabGoneReason } from "../../src/protocol/index.ts";
import { PROTOCOL_VERSION_1_0 } from "../../src/protocol/index.ts";
import type { HandoffSessionEnd } from "../../src/background/handoff/session-contract.ts";
import { createPageLeave, type PageLeavePorts } from "../../src/background/handoff/session-page-leave.ts";
import type { SessionPhase } from "../../src/background/handoff/session-phase.ts";
import type { ReloadedTabAnswer } from "../../src/background/handoff/session-page-reload.ts";

interface Scenario {
  phase: SessionPhase;
  portFrameId: number;
  tabRemoved: boolean;
  returning: boolean;
  socketOpened: boolean;
  socketAttached: boolean;
  abandonmentPending: boolean;
  nextVideoExpected: boolean;
  /** Что ответит браузер на вопрос «страница на месте?» (`undefined` — вопрос не задан/не отвечен). */
  stayedAnswer: "same_page" | "closed" | "navigated_away" | undefined;
  orphaned: boolean;
  /** Настройка ожидания после перезагрузки не `0` (16c); по умолчанию выключена — старые сценарии как были. */
  reloadWaits: boolean;
  /** Что ответит браузер на вопрос «где вкладка» (16c); `undefined` — вопрос не отвечен. */
  reloadAnswer: ReloadedTabAnswer | undefined;
}

function scenario(overrides: Partial<Scenario>): Scenario {
  return {
    phase: "active",
    portFrameId: 0,
    tabRemoved: false,
    returning: false,
    socketOpened: true,
    socketAttached: true,
    abandonmentPending: false,
    nextVideoExpected: false,
    stayedAnswer: undefined,
    orphaned: false,
    reloadWaits: false,
    reloadAnswer: undefined,
    ...overrides,
  };
}

/** Фейковый сеанс: журнал всего, что модуль у него попросил. */
function fakeSession(state: Scenario) {
  const log: string[] = [];
  const ends: HandoffSessionEnd[] = [];
  const ports: PageLeavePorts = {
    presence: {
      tabId: () => 7,
      pageUrl: () => undefined,
      moveTo: () => undefined,
      markRemoved: () => void log.push("markRemoved"),
      wasRemoved: () => state.tabRemoved,
      askWhyPageLeft: (onAnswer) => {
        log.push("askWhyPageLeft");
        onAnswer("navigated_away");
      },
      askWhetherPageStayed: (onAnswer) => {
        log.push("askWhetherPageStayed");
        if (state.stayedAnswer !== undefined) onAnswer(state.stayedAnswer);
      },
    },
    pageReturn: {
      isReturning: () => state.returning,
      shadowDidNotStand: () => void log.push("shadowDidNotStand"),
      tabRemoved: (tabId: number) => void log.push(`pageReturn.tabRemoved ${tabId}`),
      pageLeft: (reason: TabGoneReason) => {
        log.push(`pageLeft ${reason}`);
        return state.orphaned ? "orphaned" : "not_returnable";
      },
    } as unknown as PageLeavePorts["pageReturn"],
    relocation: { nextVideoExpectedAfterLoss: () => state.nextVideoExpected },
    silenceWatch: { stop: () => void log.push("silenceWatch.stop") },
    pageReload: {
      waitsForReloadedPage: () => state.reloadWaits,
      askWhereTabWent: (onAnswer) => {
        log.push("askWhereTabWent");
        if (state.reloadAnswer !== undefined) onAnswer(state.reloadAnswer);
      },
      stop: () => void log.push("pageReload.stop"),
    },
    phase: () => state.phase,
    portFrameId: () => state.portFrameId,
    socketOpened: () => state.socketOpened,
    socketAttached: () => state.socketAttached,
    agreedProtocol: () => PROTOCOL_VERSION_1_0,
    sendTabGoneAndClose: (reason) => void log.push(`tab_gone ${reason}`),
    forgetFramePort: () => void log.push("forgetFramePort"),
    abandonGrantedSession: (reason, failure) => void log.push(`abandon ${reason} ${failure}`),
    isAbandonmentPending: () => state.abandonmentPending,
    relabelAbandonmentAsClosed: () => void log.push("relabelClosed"),
    waitForNextVideo: () => void log.push("waitForNextVideo"),
    beginPageReloadWait: () => void log.push("beginPageReloadWait"),
    finish: (end) => void ends.push(end),
  };
  return { leave: createPageLeave(ports), log, ends };
}

describe("уход страницы сеанса", () => {
  it("порт умер в начале: сеанс выдан без страницы, причина зависит от вкладки", () => {
    const gone = fakeSession(scenario({ phase: "beginning" }));
    gone.leave.endForLostFramePort();
    assert.deepEqual(gone.log, ["abandon navigated_away page_navigated_away"]);
    const closed = fakeSession(scenario({ phase: "beginning", tabRemoved: true }));
    closed.leave.endForLostFramePort();
    assert.deepEqual(closed.log, ["abandon closed page_navigated_away"]);
  });

  it("порт умер после конца или в сироте: ничего не происходит", () => {
    for (const phase of ["finished", "page_away"] as const) {
      const session = fakeSession(scenario({ phase }));
      session.leave.endForLostFramePort();
      assert.deepEqual(session.log, [], phase);
      assert.deepEqual(session.ends, [], phase);
    }
  });

  it("порт главного документа умер: сеанс спрашивает браузер и кончается tab_gone с его ответом", () => {
    const session = fakeSession(scenario({ phase: "active", portFrameId: 0 }));
    session.leave.endForLostFramePort();
    assert.deepEqual(session.log, ["askWhyPageLeft", "pageLeft navigated_away", "tab_gone navigated_away"]);
    assert.deepEqual(session.ends, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("порт рекламного фрейма умер, следующий ролик ждут: страница на месте — ждём ролик, сеанс жив", () => {
    const session = fakeSession(scenario({ phase: "active", portFrameId: 5, nextVideoExpected: true, stayedAnswer: "same_page" }));
    session.leave.endForLostFramePort();
    assert.deepEqual(session.log, ["forgetFramePort", "silenceWatch.stop", "askWhetherPageStayed", "waitForNextVideo"]);
    assert.deepEqual(session.ends, []);
  });

  it("порт рекламного фрейма умер, но ролик не ждали: это уход страницы, а не конец ролика", () => {
    const session = fakeSession(scenario({ phase: "active", portFrameId: 5, nextVideoExpected: false }));
    session.leave.endForLostFramePort();
    assert.ok(session.log.includes("askWhyPageLeft"));
    assert.ok(!session.log.includes("askWhetherPageStayed"));
  });

  it("адрес сменился после смерти рекламного фрейма: страница ушла, ролик не ждём", () => {
    const session = fakeSession(scenario({ phase: "between_videos", portFrameId: 5, stayedAnswer: "navigated_away" }));
    session.leave.endForLostFramePort();
    assert.ok(!session.log.includes("waitForNextVideo"));
    assert.deepEqual(session.ends, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("запоздалый ответ браузера после конца сеанса ничего не меняет", () => {
    const state = scenario({ phase: "active", portFrameId: 5, nextVideoExpected: true, stayedAnswer: "closed" });
    state.phase = "finished";
    const session = fakeSession(state);
    session.leave.leavePage("closed");
    assert.deepEqual(session.log, []);
    assert.deepEqual(session.ends, []);
  });

  it("вернувшаяся вкладка ушла раньше тени: отказ возврата, плееру tab_gone не шлётся", () => {
    const session = fakeSession(scenario({ phase: "moving", returning: true }));
    session.leave.leavePage("closed");
    assert.deepEqual(session.log, ["shadowDidNotStand"]);
    assert.deepEqual(session.ends, []);
  });

  it("страницу можно вернуть: сирота, сеанс не кончается и tab_gone не шлётся", () => {
    const session = fakeSession(scenario({ phase: "active", orphaned: true }));
    session.leave.leavePage("closed");
    assert.deepEqual(session.log, ["pageLeft closed"]);
    assert.deepEqual(session.ends, []);
  });

  it("в ending страница не сиротится: tab_gone и конец", () => {
    const session = fakeSession(scenario({ phase: "ending", orphaned: true }));
    session.leave.leavePage("closed");
    assert.deepEqual(session.log, ["tab_gone closed"]);
    assert.deepEqual(session.ends, [{ kind: "page_gone", reason: "closed" }]);
  });

  it("вкладку закрыли: по фазам — сеанс выдан без страницы / подмена причины / уход / ничего", () => {
    const begun = fakeSession(scenario({ phase: "beginning" }));
    begun.leave.notifyTabRemoved();
    assert.deepEqual(begun.log, ["markRemoved", "abandon closed tab_closed"]);

    const abandoning = fakeSession(scenario({ phase: "abandoning", abandonmentPending: true }));
    abandoning.leave.notifyTabRemoved();
    assert.deepEqual(abandoning.log, ["markRemoved", "relabelClosed"]);

    const noSocket = fakeSession(scenario({ phase: "opening", socketOpened: false }));
    noSocket.leave.notifyTabRemoved();
    assert.deepEqual(noSocket.log, ["markRemoved"], "opening без сокета: увидит флаг, когда плеер выдаст сеанс");
    assert.deepEqual(noSocket.ends, []);

    const active = fakeSession(scenario({ phase: "active" }));
    active.leave.notifyTabRemoved();
    assert.deepEqual(active.ends, [{ kind: "page_gone", reason: "closed" }]);

    const orphan = fakeSession(scenario({ phase: "page_away" }));
    orphan.leave.notifyTabRemoved();
    assert.deepEqual(orphan.log, ["markRemoved", "pageReturn.tabRemoved 7"]);
    assert.deepEqual(orphan.ends, []);
  });

  describe("перезагрузка страницы (16c): документ ушёл, вкладка жива", () => {
    it("порт главного документа умер, тот же сайт — ожидание вместо ухода; порт забыт, пульс не слушаем", () => {
      const session = fakeSession(scenario({ phase: "active", portFrameId: 0, reloadWaits: true, reloadAnswer: "same_site" }));
      session.leave.endForLostFramePort();
      assert.deepEqual(session.log, ["forgetFramePort", "silenceWatch.stop", "askWhereTabWent", "beginPageReloadWait"]);
      assert.deepEqual(session.ends, [], "сеанс не кончился и tab_gone не ушёл");
    });

    it("другой сайт — прежний уход сразу, без ожидания", () => {
      const session = fakeSession(scenario({ phase: "active", portFrameId: 0, reloadWaits: true, reloadAnswer: "other_site" }));
      session.leave.endForLostFramePort();
      assert.ok(!session.log.includes("beginPageReloadWait"));
      assert.deepEqual(session.ends, [{ kind: "page_gone", reason: "navigated_away" }]);
    });

    it("вкладки нет — прежний конец «закрыли»", () => {
      const session = fakeSession(scenario({ phase: "between_videos", portFrameId: 0, reloadWaits: true, reloadAnswer: "closed" }));
      session.leave.endForLostFramePort();
      assert.deepEqual(session.ends, [{ kind: "page_gone", reason: "closed" }]);
    });

    it("порт фрейма с плеером умер посреди серии (не рекламный путь) — тоже вопрос о сайте", () => {
      const session = fakeSession(scenario({ phase: "active", portFrameId: 5, nextVideoExpected: false, reloadWaits: true, reloadAnswer: "same_site" }));
      session.leave.endForLostFramePort();
      assert.ok(!session.log.includes("askWhyPageLeft"));
      assert.ok(session.log.includes("beginPageReloadWait"));
    });

    it("рекламный фрейм умер и адрес сменился — вопрос о сайте, а не уход", () => {
      const session = fakeSession(scenario({ phase: "between_videos", portFrameId: 5, stayedAnswer: "navigated_away", reloadWaits: true, reloadAnswer: "same_site" }));
      session.leave.endForLostFramePort();
      assert.deepEqual(session.log.slice(-2), ["askWhereTabWent", "beginPageReloadWait"]);
      assert.deepEqual(session.ends, []);
    });

    it("рекламный фрейм умер, адрес тот же — путь 09c2 не меняется: ждём следующий ролик", () => {
      const session = fakeSession(scenario({ phase: "active", portFrameId: 5, nextVideoExpected: true, stayedAnswer: "same_page", reloadWaits: true, reloadAnswer: "same_site" }));
      session.leave.endForLostFramePort();
      assert.deepEqual(session.log, ["forgetFramePort", "silenceWatch.stop", "askWhetherPageStayed", "waitForNextVideo"]);
    });

    it("настройка 0, плеер не принял сеанс или страница возвращается — прежний вопрос «почему ушли»", () => {
      for (const overrides of [{ reloadWaits: false }, { socketAttached: false }, { returning: true }]) {
        const session = fakeSession(scenario({ phase: "active", portFrameId: 0, reloadWaits: true, reloadAnswer: "same_site", ...overrides }));
        session.leave.endForLostFramePort();
        assert.ok(!session.log.includes("askWhereTabWent"), JSON.stringify(overrides));
      }
    });

    it("во время ожидания: потеря порта — ничего; закрытие вкладки — «закрыли»", () => {
      const lost = fakeSession(scenario({ phase: "page_reloading" }));
      lost.leave.endForLostFramePort();
      assert.deepEqual(lost.log, []);
      const closed = fakeSession(scenario({ phase: "page_reloading" }));
      closed.leave.notifyTabRemoved();
      assert.deepEqual(closed.ends, [{ kind: "page_gone", reason: "closed" }]);
    });
  });
});
