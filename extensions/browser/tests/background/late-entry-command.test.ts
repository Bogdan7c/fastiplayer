// Команды позднего входа (сессия 10d): перезагрузку может попросить только страница самого экстеншена.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createLateEntryCommandListener, parseLateEntryRequest } from "../../src/background/entries/late-entry-command.ts";
import { LATE_ENTRY_STATE_KIND, RELOAD_AND_OPEN_KIND, type LateEntryStateReply } from "../../src/shared/late-entry-contract.ts";

const IDENTITY = { extensionId: "ourid", extensionBaseUrl: "chrome-extension://ourid/" };
const POPUP = { id: "ourid", url: "chrome-extension://ourid/popup.html" };

function world() {
  const running = new Set<number>();
  const started: number[] = [];
  const replies: LateEntryStateReply[] = [];
  const listener = createLateEntryCommandListener(IDENTITY, {
    start: (tabId) => {
      started.push(tabId);
      running.add(tabId);
      return true;
    },
    isRunning: (tabId) => running.has(tabId),
  });
  return { listener, started, replies, respond: (reply: LateEntryStateReply) => void replies.push(reply) };
}

describe("команды позднего входа", () => {
  it("кнопка попапа запускает работу на этой вкладке и отвечает сразу", () => {
    const { listener, started, replies, respond } = world();
    assert.equal(listener({ kind: RELOAD_AND_OPEN_KIND, tab_id: 4 }, POPUP, respond), false, "ответ синхронный: канал не держим");
    assert.deepEqual(started, [4]);
    assert.deepEqual(replies, [{ running: true }]);
  });

  it("вопрос о состоянии ничего не запускает", () => {
    const { listener, started, replies, respond } = world();
    listener({ kind: LATE_ENTRY_STATE_KIND, tab_id: 4 }, POPUP, respond);
    assert.deepEqual(started, []);
    assert.deepEqual(replies, [{ running: false }]);
  });

  it("контент-скрипт и чужой экстеншен перезагрузить вкладку не могут и ответа не получают", () => {
    const { listener, started, replies, respond } = world();
    for (const sender of [{ id: "ourid", url: "https://films.example/" }, { id: "other", url: "chrome-extension://other/popup.html" }]) {
      assert.equal(listener({ kind: RELOAD_AND_OPEN_KIND, tab_id: 4 }, sender, respond), false);
    }
    assert.deepEqual(started, []);
    assert.deepEqual(replies, []);
  });

  it("форма строгая: лишнее поле, дробная и отрицательная вкладка, чужой вид", () => {
    assert.equal(parseLateEntryRequest({ kind: RELOAD_AND_OPEN_KIND, tab_id: 4, extra: 1 }), undefined);
    assert.equal(parseLateEntryRequest({ kind: RELOAD_AND_OPEN_KIND, tab_id: 1.5 }), undefined);
    assert.equal(parseLateEntryRequest({ kind: RELOAD_AND_OPEN_KIND, tab_id: -1 }), undefined);
    assert.equal(parseLateEntryRequest({ kind: "hand_off_tab", tab_id: 4 }), undefined);
    assert.equal(parseLateEntryRequest([RELOAD_AND_OPEN_KIND]), undefined);
    assert.deepEqual(parseLateEntryRequest({ kind: LATE_ENTRY_STATE_KIND, tab_id: 0 }), { kind: LATE_ENTRY_STATE_KIND, tab_id: 0 });
  });
});
