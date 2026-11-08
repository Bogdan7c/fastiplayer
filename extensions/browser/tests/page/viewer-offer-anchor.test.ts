// Сигнал предложения «Ask» (12b), обе стороны: MAIN-мир сообщает «встало/ушло» только когда изолированный мир
// правда может это услышать, изолированный мир переводит сигнал в вызовы панели.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { listenForViewerOffer } from "../../src/content/viewer-offer-listener.ts";
import { ViewerOfferAnchor } from "../../src/page/viewer-offer-anchor.ts";
import type { AnchorSignalEvent, HandedOffVideoElement } from "../../src/content/handed-off-video-listener.ts";
import { viewerOfferEventNameOf } from "../../src/shared/handed-off-video-contract.ts";
import { FakeVideoTarget } from "../content/support/fake-ui-dom.ts";

const NAME = "00112233445566778899aabbccddeeff";
const OWN_WINDOW = { window: true };

function anchorWith(options: { readonly connected?: boolean; readonly dispatchThrows?: boolean } = {}) {
  const dispatched: { target: object; eventName: string }[] = [];
  const oddities: string[] = [];
  const anchor = new ViewerOfferAnchor({
    events: {
      dispatchComposed: (target, eventName) => {
        if (options.dispatchThrows === true) throw new Error("dispatch replaced");
        dispatched.push({ target, eventName });
      },
    },
    ownWindow: OWN_WINDOW,
    isConnected: () => options.connected ?? true,
    reportOddity: (line) => void oddities.push(line),
  });
  return { anchor, dispatched, oddities };
}

describe("сигнал предложения, MAIN-мир", () => {
  it("встало — событие производного имени на элементе; ушло — на своём окне; повторное «ушло» молчит", () => {
    const { anchor, dispatched } = anchorWith();
    anchor.setEventName(NAME);
    const element = { video: 1 };
    assert.equal(anchor.offerStood(element), true);
    anchor.offerEnded();
    anchor.offerEnded();
    assert.deepEqual(dispatched, [
      { target: element, eventName: viewerOfferEventNameOf(NAME) },
      { target: OWN_WINDOW, eventName: viewerOfferEventNameOf(NAME) },
    ]);
  });

  it("имени ещё нет, элемент вне документа или рассылка сломана — `false`: сторож не держит видео без панели", () => {
    assert.equal(anchorWith().anchor.offerStood({}), false, "имени нет");
    const outside = anchorWith({ connected: false });
    outside.anchor.setEventName(NAME);
    assert.equal(outside.anchor.offerStood({}), false);
    const broken = anchorWith({ dispatchThrows: true });
    broken.anchor.setEventName(NAME);
    assert.equal(broken.anchor.offerStood({}), false);
    assert.equal(broken.oddities.length, 1);
  });

  it("второе имя не принимается: сигнал идёт по первому", () => {
    const { anchor, dispatched } = anchorWith();
    anchor.setEventName(NAME);
    anchor.setEventName("ffffffffffffffffffffffffffffffff");
    anchor.offerStood({});
    assert.equal(dispatched[0]?.eventName, viewerOfferEventNameOf(NAME));
  });
});

describe("сигнал предложения, изолированный мир", () => {
  function listen() {
    const listeners = new Map<string, (event: AnchorSignalEvent) => void>();
    const calls: (HandedOffVideoElement | "left")[] = [];
    const listener = listenForViewerOffer((eventName, handler) => void listeners.set(eventName, handler), NAME, {
      viewerOfferStood: (element) => void calls.push(element),
      viewerOfferLeft: () => void calls.push("left"),
    });
    return { listeners, calls, listener };
  }
  const signal = (target: unknown, currentTarget: unknown) => ({ target, currentTarget, stopped: false, stopImmediatePropagation() { this.stopped = true; } });

  it("слушает производное имя; элемент — «встало», само окно — «ушло», событие гасится", () => {
    const { listeners, calls } = listen();
    const handler = listeners.get(viewerOfferEventNameOf(NAME));
    assert.ok(handler !== undefined);
    const video = new FakeVideoTarget({ left: 0, top: 0, width: 10, height: 10 });
    const stood = signal(video, OWN_WINDOW);
    handler(stood);
    handler(signal(OWN_WINDOW, OWN_WINDOW));
    assert.deepEqual(calls, [video, "left"]);
    assert.equal(stood.stopped, true, "странице сигнал не виден");
  });

  it("цель не похожа на элемент — отбрасывается и считается", () => {
    const { listeners, calls, listener } = listen();
    listeners.get(viewerOfferEventNameOf(NAME))?.(signal({ notAnElement: true }, OWN_WINDOW));
    assert.deepEqual(calls, []);
    assert.equal(listener.rejectedSignals(), 1);
  });
});
