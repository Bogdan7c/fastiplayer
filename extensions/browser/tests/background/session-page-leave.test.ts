// Уход страницы сеанса (вынесено из сеанса в 16a): порт умер, вкладку закрыли, ушли со страницы.
// Проверяем результат (какие шаги сделал сеанс и чего не трогал), а не вызовы по отдельности.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { TabGoneReason } from "../../src/protocol/index.ts";
import { PROTOCOL_VERSION_1_0 } from "../../src/protocol/index.ts";
import type { HandoffSessionEnd } from "../../src/background/handoff/session-contract.ts";
import { createPageLeave, type PageLeavePorts } from "../../src/background/handoff/session-page-leave.ts";
import type { SessionPhase } from "../../src/background/handoff/session-phase.ts";

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
});
