// Якорь переданного видео, сторона MAIN-мира (11a, решение 236): сигнал на элементе, когда тень встала,
// сигнал на окне, когда передача ушла; оригиналы событий, которые страница не перехватит.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { HandedOffVideoAnchor, captureAnchorEventOriginals } from "../../src/page/handed-off-video-anchor.ts";
import { WATCH_HANDED_OFF_VIDEO_KIND } from "../../src/shared/handed-off-video-contract.ts";
import { handshake, startHarness, type BridgeHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { addPlayingMseVideo, describeAndBegin } from "./support/handoff-steps.ts";

const EVENT_NAME = "00112233445566778899aabbccddeeff";
const WATCH = { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: EVENT_NAME };

/** Кто и что услышал по имени якоря. */
function listenAnchor(target: EventTarget) {
  const heard: Event[] = [];
  target.addEventListener(EVENT_NAME, (event) => void heard.push(event));
  return heard;
}

async function withHarness(body: (harness: BridgeHarness, isolated: IsolatedSide) => Promise<void>): Promise<void> {
  const harness = startHarness();
  const isolated = handshake(harness.page);
  try {
    await body(harness, isolated);
  } finally {
    isolated.close();
  }
}

describe("якорь переданного видео в сборке MAIN-мира", () => {
  it("тень встала — composed-событие на самом элементе; передача ушла — событие на окне", async () => {
    await withHarness(async (harness, isolated) => {
      const { video } = addPlayingMseVideo(harness);
      const onVideo = listenAnchor(video);
      const onWindow = listenAnchor(harness.page.win);
      isolated.send(WATCH);
      await describeAndBegin(isolated);
      assert.equal(harness.shadow.started.length, 1, "тень встала");
      assert.equal(onVideo.length, 1);
      assert.equal(onVideo[0]?.composed, true, "выходит из теневых деревьев");
      assert.equal(onVideo[0]?.target, video);
      assert.equal(onWindow.filter((event) => event.target === harness.page.win).length, 0, "«ушло» ещё не было");
      isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
      await isolated.settle();
      assert.equal(onWindow.filter((event) => event.target === harness.page.win).length, 1);
      assert.equal(onVideo.length, 1, "элементу второго сигнала нет");
    });
  });

  it("подмены страницы (dispatchEvent прототипа и элемента, конструктор Event) сигнал не перехватывают", async () => {
    await withHarness(async (harness, isolated) => {
      const { video } = addPlayingMseVideo(harness);
      const onVideo = listenAnchor(video);
      const intercepted: unknown[] = [];
      const eventTargetPrototype = harness.page.win.EventTarget.prototype as { dispatchEvent: unknown };
      const original = eventTargetPrototype.dispatchEvent;
      eventTargetPrototype.dispatchEvent = (event: unknown) => intercepted.push(event);
      (video as unknown as { dispatchEvent: unknown }).dispatchEvent = (event: unknown) => intercepted.push(event);
      const originalEvent = harness.page.win.Event;
      Object.assign(harness.page.win, { Event: class { constructor() { throw new Error("page Event"); } } });
      try {
        isolated.send(WATCH);
        await describeAndBegin(isolated);
      } finally {
        eventTargetPrototype.dispatchEvent = original;
        Object.assign(harness.page.win, { Event: originalEvent });
      }
      assert.equal(onVideo.length, 1, "сигнал дошёл мимо подмен");
      assert.deepEqual(intercepted, [], "подмены страницы ничего не видели");
    });
  });

  it("имя пришло позже начала передачи — сигнал уходит по приходу имени", async () => {
    await withHarness(async (harness, isolated) => {
      const { video } = addPlayingMseVideo(harness);
      const onVideo = listenAnchor(video);
      await describeAndBegin(isolated);
      assert.equal(onVideo.length, 0);
      isolated.send(WATCH);
      await isolated.settle();
      assert.equal(onVideo.length, 1);
    });
  });

  it("имя не по форме не принимается: якоря нет, а второе имя не перебивает первое", async () => {
    await withHarness(async (harness, isolated) => {
      const { video } = addPlayingMseVideo(harness);
      const onVideo = listenAnchor(video);
      isolated.send({ kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: "short" });
      isolated.send({ kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: EVENT_NAME, extra: true });
      await describeAndBegin(isolated);
      assert.equal(onVideo.length, 0, "мусорные имена не взяты");
      assert.equal(harness.bridge.anchor?.isAnnounced, false);
      isolated.send(WATCH);
      const otherName = "ffeeddccbbaa99887766554433221100";
      const onOther: Event[] = [];
      video.addEventListener(otherName, (event) => void onOther.push(event));
      isolated.send({ kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: otherName });
      await isolated.settle();
      assert.equal(onVideo.length, 1);
      assert.equal(onOther.length, 0, "первое имя в силе");
    });
  });
});

describe("якорь отдельно", () => {
  function anchorWith() {
    const win = new EventTarget();
    const dispatched: { target: object; name: string }[] = [];
    const oddities: string[] = [];
    const anchor = new HandedOffVideoAnchor({
      events: { dispatchComposed: (target, name) => void dispatched.push({ target, name }) },
      ownWindow: win,
      isConnected: () => true,
      reportOddity: (message) => void oddities.push(message),
    });
    return { anchor, win, dispatched, oddities };
  }

  it("о неназванной передаче «ушло» не шлётся; без имени не шлётся ничего", () => {
    const { anchor, dispatched } = anchorWith();
    const element = {};
    anchor.handoffStood(element);
    anchor.handoffEnded();
    assert.deepEqual(dispatched, []);
  });

  it("новая передача без конца старой: сначала «ушло» старой, потом «передано» новой", () => {
    const { anchor, win, dispatched } = anchorWith();
    const first = { name: "first" };
    const second = { name: "second" };
    anchor.setEventName(EVENT_NAME);
    anchor.handoffStood(first);
    anchor.handoffStood(second);
    assert.deepEqual(dispatched.map((entry) => entry.target), [first, win, second]);
  });

  it("элемент вне документа в момент старта — сигнал ждёт его возвращения и уходит на тике часов плеера, один раз", () => {
    const win = new EventTarget();
    const dispatched: object[] = [];
    let connected = false;
    const anchor = new HandedOffVideoAnchor({
      events: { dispatchComposed: (target) => void dispatched.push(target) },
      ownWindow: win,
      isConnected: () => connected,
      reportOddity: () => {},
    });
    const element = { name: "video" };
    anchor.setEventName(EVENT_NAME);
    anchor.handoffStood(element);
    anchor.clockTick();
    assert.deepEqual(dispatched, [], "путь события не дошёл бы до окна — не шлём");
    connected = true;
    anchor.clockTick();
    anchor.clockTick();
    assert.deepEqual(dispatched, [element]);
    anchor.handoffEnded();
    assert.deepEqual(dispatched, [element, win]);
  });

  it("сбой рассылки не бросает в страницу, а оставляет след", () => {
    const oddities: string[] = [];
    const anchor = new HandedOffVideoAnchor({
      events: { dispatchComposed: () => { throw new Error("boom"); } },
      ownWindow: {},
      isConnected: () => true,
      reportOddity: (message) => void oddities.push(message),
    });
    anchor.setEventName(EVENT_NAME);
    assert.doesNotThrow(() => anchor.handoffStood({}));
    assert.equal(anchor.isAnnounced, false);
    assert.equal(oddities.length, 1);
  });

  it("оригиналы снимаются с прототипа окна: composed-событие с нужным именем", () => {
    const target = new EventTarget();
    const heard: Event[] = [];
    target.addEventListener(EVENT_NAME, (event) => void heard.push(event));
    captureAnchorEventOriginals({ EventTarget, Event }).dispatchComposed(target, EVENT_NAME);
    assert.equal(heard.length, 1);
    assert.equal(heard[0]?.composed, true);
    assert.equal(heard[0]?.type, EVENT_NAME);
  });
});
