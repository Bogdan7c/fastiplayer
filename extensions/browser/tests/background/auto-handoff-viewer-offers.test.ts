// Предложения «Ask» (12b) отдельно от оркестратора: таблица «кнопка → план», память вкладки и слушатель выбора
// (кого фон слушает, а кого нет).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createViewerOfferChoiceListener } from "../../src/background/auto-handoff/viewer-offer-listener.ts";
import { createViewerOffers, planOfViewerChoice } from "../../src/background/auto-handoff/viewer-offers.ts";
import { DEFAULT_AUTO_HANDOFF_SETTINGS } from "../../src/shared/auto-handoff-settings.ts";
import { VIEWER_OFFER_CHOICE_KIND, type ViewerOfferAnswer, type ViewerOfferChoiceMessage } from "../../src/shared/viewer-offer-contract.ts";
import { flushPromises } from "./support.ts";

const DOC = "0123456789abcdef0123456789abcdef";
const OUR_ID = "our-extension";
const choice: ViewerOfferChoiceMessage = { kind: VIEWER_OFFER_CHOICE_KIND, document: DOC, element: 4, choice: "here" };

describe("предложение «Ask»: правила и память", () => {
  it("кнопка → план: Always и Never пишут правило и сразу действуют (решение 255)", () => {
    assert.deepEqual(planOfViewerChoice("fastiplayer"), { rule: undefined, action: "hand_off" });
    assert.deepEqual(planOfViewerChoice("here"), { rule: undefined, action: "play_here" });
    assert.deepEqual(planOfViewerChoice("always"), { rule: "always", action: "hand_off" });
    assert.deepEqual(planOfViewerChoice("never"), { rule: "never", action: "play_here" });
  });

  it("закрытая вкладка забывает предложение; повторное то же предложение прежнее не снимает", () => {
    const withdrawals: number[] = [];
    const offers = createViewerOffers({
      readSettings: async () => DEFAULT_AUTO_HANDOFF_SETTINGS,
      writeSettings: async () => {},
      describeTab: async () => ({ url: "https://site.example/" }),
      startHandoff: () => {},
      withdrawOffer: (_tab, _frame, element) => void withdrawals.push(element),
      note: () => {},
    });
    const ref = { frame_id: 0, document: DOC, element: 4 };
    offers.offerStood(5, ref);
    offers.offerStood(5, { ...ref });
    assert.deepEqual(withdrawals, []);
    offers.forgetTab(5);
    assert.equal(offers.offerOf(5), undefined);
  });

  it("сбой чтения настройки — no_offer и запись в журнал, передачи нет", async () => {
    const notes: string[] = [];
    let started = 0;
    const offers = createViewerOffers({
      readSettings: () => Promise.reject(new Error("storage")),
      writeSettings: async () => {},
      describeTab: async () => ({ url: "https://site.example/" }),
      startHandoff: () => void (started += 1),
      withdrawOffer: () => {},
      note: (message) => void notes.push(message),
    });
    assert.equal(await offers.choose(5, 0, choice), "no_offer");
    assert.equal(started, 0);
    assert.equal(notes.length, 1);
  });
});

describe("слушатель выбора", () => {
  function listen() {
    const calls: { tabId: number; frameId: number; message: ViewerOfferChoiceMessage }[] = [];
    const listener = createViewerOfferChoiceListener(OUR_ID, async (tabId, frameId, message) => {
      calls.push({ tabId, frameId, message });
      return "play_here";
    });
    return { listener, calls };
  }

  it("свой контент-скрипт из вкладки: выбор доходит, ответ асинхронный", async () => {
    const { listener, calls } = listen();
    const answers: ViewerOfferAnswer[] = [];
    assert.equal(listener(choice, { id: OUR_ID, tab: { id: 5 }, frameId: 2 }, (answer) => void answers.push(answer)), true);
    await flushPromises();
    assert.deepEqual(calls, [{ tabId: 5, frameId: 2, message: choice }]);
    assert.deepEqual(answers, ["play_here"]);
  });

  it("чужой экстеншен, страница экстеншена без вкладки и сообщение не по форме — не наше дело", () => {
    const { listener, calls } = listen();
    const ignore = (): void => assert.fail("no answer expected");
    assert.equal(listener(choice, { id: "someone-else", tab: { id: 5 }, frameId: 0 }, ignore), false);
    assert.equal(listener(choice, { id: OUR_ID }, ignore), false);
    assert.equal(listener({ ...choice, choice: "delete_everything" }, { id: OUR_ID, tab: { id: 5 }, frameId: 0 }, ignore), false);
    assert.equal(listener({ ...choice, extra: 1 }, { id: OUR_ID, tab: { id: 5 }, frameId: 0 }, ignore), false);
    assert.equal(listener({ ...choice, document: "not a tag!" }, { id: OUR_ID, tab: { id: 5 }, frameId: 0 }, ignore), false);
    assert.deepEqual(calls, []);
  });
});
