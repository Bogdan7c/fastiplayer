// Рукопожатие «курьер ⇄ MAIN-мир»: оба порядка запуска, подделки, отсутствие ответа, следы в странице.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { startCodecCarrier } from "../../src/content/codec-carrier.ts";
import { installCodecPolicy } from "../../src/page/codec-policy/index.ts";
import { startCarrierRendezvous } from "../../src/page/codec-policy/carrier-rendezvous.ts";
import { createPolicyCell } from "../../src/page/codec-policy/policy-cell.ts";
import { CODEC_POLICY_ASK_EVENT, CODEC_POLICY_TELL_EVENT, refuse } from "../../src/shared/codec-policy.ts";

const AV1 = 'video/mp4; codecs="av01.0.05M.08"';

const tellEvent = (type: string, payload: string): Event => new CustomEvent(type, { detail: payload, cancelable: true });
const plainEvent = (type: string): Event => new Event(type);

describe("рукопожатие: порядок запуска не важен", () => {
  it("курьер стартовал ПЕРВЫМ: страница спросила, курьер ответил -> политика принята", () => {
    const doc = new EventTarget();
    const cell = createPolicyCell();
    startCodecCarrier(doc, ["av1"], tellEvent);
    assert.deepEqual(cell.current(), { kind: "transparent" }, "до страницы слушать некому");
    startCarrierRendezvous(doc, cell, plainEvent);
    assert.deepEqual(cell.current(), refuse(["av1"]));
  });

  it("страница стартовала ПЕРВОЙ: курьер сказал на старте -> политика принята", () => {
    const doc = new EventTarget();
    const cell = createPolicyCell();
    startCarrierRendezvous(doc, cell, plainEvent);
    assert.deepEqual(cell.current(), { kind: "transparent" });
    startCodecCarrier(doc, ["av1", "vp9"], tellEvent);
    assert.deepEqual(cell.current(), refuse(["av1", "vp9"]));
  });

  it("курьера нет вовсе -> политика прозрачна (ответов нет)", () => {
    const cell = createPolicyCell();
    startCarrierRendezvous(new EventTarget(), cell, plainEvent);
    assert.deepEqual(cell.current(), { kind: "transparent" });
  });
});

describe("рукопожатие: подделки страницы", () => {
  it("второе «tell» после принятия игнорируется (слушатель снят)", () => {
    const doc = new EventTarget();
    const cell = createPolicyCell();
    startCodecCarrier(doc, ["av1"], tellEvent);
    startCarrierRendezvous(doc, cell, plainEvent);
    doc.dispatchEvent(tellEvent(CODEC_POLICY_TELL_EVENT, "hevc"));
    assert.deepEqual(cell.current(), refuse(["av1"]));
  });

  it("невалидное «tell» (не строка, мусор, не канон) пропускается, настоящее после него принимается", () => {
    const doc = new EventTarget();
    const cell = createPolicyCell();
    startCarrierRendezvous(doc, cell, plainEvent);
    doc.dispatchEvent(new CustomEvent(CODEC_POLICY_TELL_EVENT, { detail: { families: ["av1"] } }));
    doc.dispatchEvent(new CustomEvent(CODEC_POLICY_TELL_EVENT, { detail: "vp9,av1" }));
    doc.dispatchEvent(new CustomEvent(CODEC_POLICY_TELL_EVENT, { detail: "" }));
    doc.dispatchEvent(new Event(CODEC_POLICY_TELL_EVENT));
    assert.deepEqual(cell.current(), { kind: "transparent" });
    doc.dispatchEvent(tellEvent(CODEC_POLICY_TELL_EVENT, "av1"));
    assert.deepEqual(cell.current(), refuse(["av1"]));
  });

  it("принятое «tell» гасится: слушатели позже в цепочке его не видят", () => {
    const doc = new EventTarget();
    const cell = createPolicyCell();
    const seenByPage: string[] = [];
    startCarrierRendezvous(doc, cell, plainEvent);
    doc.addEventListener(CODEC_POLICY_TELL_EVENT, () => seenByPage.push("tell"), { capture: true });
    startCodecCarrier(doc, ["av1"], tellEvent);
    assert.deepEqual(seenByPage, []);
  });

  it("курьер отвечает на «ask» один раз: страница не может опрашивать его бесконечно", () => {
    const doc = new EventTarget();
    let tells = 0;
    startCodecCarrier(doc, ["av1"], tellEvent);
    doc.addEventListener(CODEC_POLICY_TELL_EVENT, () => (tells += 1), { capture: true });
    doc.dispatchEvent(new Event(CODEC_POLICY_ASK_EVENT));
    doc.dispatchEvent(new Event(CODEC_POLICY_ASK_EVENT));
    assert.equal(tells, 1);
  });
});

describe("рукопожатие: курьер не остаётся слушать (находка 10 ревью 09b)", () => {
  it("курьер стартовал ПОСЛЕ страницы: «tell» принят — позднее «ask» любого скрипта страницы политику не выдаёт", () => {
    const doc = new EventTarget();
    const cell = createPolicyCell();
    startCarrierRendezvous(doc, cell, plainEvent);
    startCodecCarrier(doc, ["av1"], tellEvent);
    assert.deepEqual(cell.current(), refuse(["av1"]));
    const leaked: unknown[] = [];
    doc.addEventListener(CODEC_POLICY_TELL_EVENT, (event) => leaked.push((event as CustomEvent).detail), { capture: true });
    doc.dispatchEvent(new Event(CODEC_POLICY_ASK_EVENT));
    assert.deepEqual(leaked, [], "скрипт страницы не узнал политику пользователя");
  });

  it("«tell» никто не принял (MAIN-мира ещё нет) — курьер ждёт первого «ask», как раньше", () => {
    const doc = new EventTarget();
    let tells = 0;
    startCodecCarrier(doc, ["av1"], tellEvent);
    doc.addEventListener(CODEC_POLICY_TELL_EVENT, () => (tells += 1), { capture: true });
    doc.dispatchEvent(new Event(CODEC_POLICY_ASK_EVENT));
    assert.equal(tells, 1);
  });

  it("неотменяемое «tell» (старый создатель события) — курьер честно продолжает ждать «ask»", () => {
    const doc = new EventTarget();
    const cell = createPolicyCell();
    startCarrierRendezvous(doc, cell, plainEvent);
    startCodecCarrier(doc, ["av1"], (type, payload) => new CustomEvent(type, { detail: payload }));
    assert.deepEqual(cell.current(), refuse(["av1"]), "политика всё равно принята");
    let tells = 0;
    doc.addEventListener(CODEC_POLICY_TELL_EVENT, () => (tells += 1), { capture: true });
    doc.dispatchEvent(new Event(CODEC_POLICY_ASK_EVENT));
    assert.equal(tells, 1, "без отмены курьер не знает, что принято");
  });
});

describe("полная установка в окне (installCodecPolicy)", () => {
  function windowWithBrowser() {
    class FakeMediaSource {
      static isTypeSupported(_type: string): boolean {
        return true;
      }
    }
    const document = new EventTarget();
    return { win: { document, CustomEvent, MediaSource: FakeMediaSource }, FakeMediaSource };
  }

  it("курьер сказал «отказать AV1» -> уже первый вопрос страницы получает отказ на AV1", () => {
    const { win, FakeMediaSource } = windowWithBrowser();
    startCodecCarrier(win.document, ["av1"], tellEvent);
    const handle = installCodecPolicy(win, () => {});
    assert.equal(FakeMediaSource.isTypeSupported(AV1), false);
    assert.deepEqual(handle.policy.current(), refuse(["av1"]));
  });

  it("курьера нет -> AV1 поддержан, HLS — нет, политика прозрачна", () => {
    const { win, FakeMediaSource } = windowWithBrowser();
    const handle = installCodecPolicy(win, () => {});
    assert.equal(FakeMediaSource.isTypeSupported(AV1), true);
    assert.equal(FakeMediaSource.isTypeSupported("application/x-mpegurl"), false);
    assert.deepEqual(handle.policy.current(), { kind: "transparent" });
  });

  it("установка не создаёт глобалов", () => {
    const before = new Set(Object.getOwnPropertyNames(globalThis));
    const { win } = windowWithBrowser();
    installCodecPolicy(win, () => {});
    assert.deepEqual(Object.getOwnPropertyNames(globalThis).filter((name) => !before.has(name)), []);
  });
});
