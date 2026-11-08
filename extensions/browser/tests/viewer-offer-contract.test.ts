// Договор предложения «Ask» (12b): строгая форма каждого сообщения миров и фона.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  parseViewerOfferAnswer,
  parseViewerOfferChoiceMessage,
  parseViewerOfferPageCommand,
  parseWithdrawViewerOfferRequest,
} from "../src/shared/viewer-offer-contract.ts";

describe("договор предложения «Ask»", () => {
  it("команды MAIN-миру: только два вида, ключ — целое ≥ 1, лишних полей нет", () => {
    assert.deepEqual(parseViewerOfferPageCommand({ kind: "hold_for_viewer_offer", element: 3 }), { kind: "hold_for_viewer_offer", element: 3 });
    assert.deepEqual(parseViewerOfferPageCommand({ kind: "withdraw_viewer_offer", element: 3 }), { kind: "withdraw_viewer_offer", element: 3 });
    for (const bad of [{ kind: "hold_for_viewer_offer", element: 0 }, { kind: "hold_for_viewer_offer", element: 1.5 }, { kind: "hold_for_viewer_offer", element: 3, x: 1 }, { kind: "release_held_video", element: 3 }, null, []]) {
      assert.equal(parseViewerOfferPageCommand(bad), undefined, JSON.stringify(bad));
    }
  });

  it("выбор человека: метка документа, ключ и один из четырёх выборов", () => {
    const good = { kind: "viewer_offer_choice", document: "doc-7f3a", element: 4, choice: "never" };
    assert.deepEqual(parseViewerOfferChoiceMessage(good), good);
    assert.equal(parseViewerOfferChoiceMessage({ ...good, choice: "maybe" }), undefined);
    assert.equal(parseViewerOfferChoiceMessage({ ...good, document: "a b" }), undefined);
    assert.equal(parseViewerOfferChoiceMessage({ ...good, site: "evil.example" }), undefined, "сайт фон берёт из вкладки, не из сообщения");
  });

  it("ответ фона и просьба снять: только договорные значения", () => {
    assert.equal(parseViewerOfferAnswer("hand_off"), "hand_off");
    assert.equal(parseViewerOfferAnswer("adopt"), undefined);
    assert.deepEqual(parseWithdrawViewerOfferRequest({ kind: "withdraw_viewer_offer_request", element: 2 }), { kind: "withdraw_viewer_offer_request", element: 2 });
    assert.equal(parseWithdrawViewerOfferRequest({ kind: "withdraw_viewer_offer_request" }), undefined);
  });
});
