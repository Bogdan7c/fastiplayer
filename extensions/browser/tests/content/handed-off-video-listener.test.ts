// Якорь переданного видео, сторона изолированного мира (11a): случайное имя, слушатель захвата, перевод
// сигналов MAIN-мира в «передано (элемент)» / «ушло» для заглушки.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { listenForHandedOffVideo, type AnchorSignalEvent, type HandedOffVideoElement } from "../../src/content/handed-off-video-listener.ts";
import { HANDED_OFF_VIDEO_EVENT_NAME_BYTES, isHandedOffVideoEventName, parseWatchHandedOffVideo, WATCH_HANDED_OFF_VIDEO_KIND } from "../../src/shared/handed-off-video-contract.ts";
import { parseChannelMessage } from "../../src/page/page-command-guard.ts";
import { FakeVideoTarget } from "./support/fake-ui-dom.ts";

/** Окно изолированного мира: какой слушатель поставлен и на какое имя. */
function fakeWindow() {
  const listeners = new Map<string, (event: AnchorSignalEvent) => void>();
  return {
    listeners,
    addCaptureListener: (eventName: string, listener: (event: AnchorSignalEvent) => void) => void listeners.set(eventName, listener),
  };
}

/** Сигнал, как его видит слушатель на `window` в фазе захвата. */
function signal(target: unknown, currentTarget: unknown, path?: readonly unknown[]) {
  const event = {
    target,
    currentTarget,
    stoppedImmediately: false,
    stopImmediatePropagation() {
      event.stoppedImmediately = true;
    },
    ...(path === undefined ? {} : { composedPath: () => path }),
  };
  return event;
}

function recordingSink() {
  const calls: (HandedOffVideoElement | "left")[] = [];
  return { calls, sink: { videoHandedOff: (element: HandedOffVideoElement) => void calls.push(element), videoLeft: () => void calls.push("left") } };
}

const OWN_WINDOW = { name: "isolated window" };
let fillCounter = 0;
/** Генератор, дающий разные байты при каждом вызове. */
const randomFill = (bytes: Uint8Array): void => {
  fillCounter += 1;
  bytes.fill(fillCounter);
};

describe("слушатель якоря переданного видео", () => {
  it("имя случайное (128 бит, разное у разных документов), слушатель стоит на это имя, сообщение — договорной формы", () => {
    const first = fakeWindow();
    const second = fakeWindow();
    const a = listenForHandedOffVideo({ addCaptureListener: first.addCaptureListener, randomFill }, recordingSink().sink);
    const b = listenForHandedOffVideo({ addCaptureListener: second.addCaptureListener, randomFill }, recordingSink().sink);
    assert.equal(a.watchMessage.kind, WATCH_HANDED_OFF_VIDEO_KIND);
    assert.equal(a.watchMessage.event_name.length, HANDED_OFF_VIDEO_EVENT_NAME_BYTES * 2);
    assert.ok(isHandedOffVideoEventName(a.watchMessage.event_name));
    assert.notEqual(a.watchMessage.event_name, b.watchMessage.event_name);
    assert.deepEqual([...first.listeners.keys()], [a.watchMessage.event_name], "слушатель стоит ДО отправки имени и ровно на него");
    // MAIN-мир принимает это сообщение тем же разбором, что и все сообщения порта.
    assert.deepEqual(parseChannelMessage(a.watchMessage), a.watchMessage);
  });

  it("сигнал на элементе — «передано» с этим элементом; на самом window — «ушло»; оба гасятся", () => {
    const win = fakeWindow();
    const { calls, sink } = recordingSink();
    const listener = listenForHandedOffVideo({ addCaptureListener: win.addCaptureListener, randomFill }, sink);
    const onSignal = win.listeners.get(listener.watchMessage.event_name)!;
    const video = new FakeVideoTarget({ left: 10, top: 20, width: 640, height: 360 });
    const handedOff = signal(video, OWN_WINDOW, [video, OWN_WINDOW]);
    onSignal(handedOff);
    const left = signal(OWN_WINDOW, OWN_WINDOW, [OWN_WINDOW]);
    onSignal(left);
    assert.deepEqual(calls, [video, "left"]);
    assert.ok(handedOff.stoppedImmediately && left.stoppedImmediately, "странице сигналы не достаются");
  });

  it("элемент внутри открытого теневого дерева: берётся сам элемент из composedPath, а не перенаправленный хозяин", () => {
    const win = fakeWindow();
    const { calls, sink } = recordingSink();
    const listener = listenForHandedOffVideo({ addCaptureListener: win.addCaptureListener, randomFill }, sink);
    const host = new FakeVideoTarget({ left: 0, top: 0, width: 800, height: 600 });
    const video = new FakeVideoTarget({ left: 10, top: 10, width: 640, height: 360 });
    win.listeners.get(listener.watchMessage.event_name)!(signal(host, OWN_WINDOW, [video, { shadowRoot: true }, host, OWN_WINDOW]));
    assert.deepEqual(calls, [video]);
  });

  it("закрытое дерево (путь скрыт, есть только хозяин) — заглушка над хозяином; без composedPath — target", () => {
    const win = fakeWindow();
    const { calls, sink } = recordingSink();
    const listener = listenForHandedOffVideo({ addCaptureListener: win.addCaptureListener, randomFill }, sink);
    const host = new FakeVideoTarget({ left: 0, top: 0, width: 800, height: 600 });
    win.listeners.get(listener.watchMessage.event_name)!(signal(host, OWN_WINDOW, [host, OWN_WINDOW]));
    win.listeners.get(listener.watchMessage.event_name)!(signal(host, OWN_WINDOW));
    assert.deepEqual(calls, [host, host]);
  });

  it("цель не похожа на элемент — сигнал не принимается и считается", () => {
    const win = fakeWindow();
    const { calls, sink } = recordingSink();
    const listener = listenForHandedOffVideo({ addCaptureListener: win.addCaptureListener, randomFill }, sink);
    win.listeners.get(listener.watchMessage.event_name)!(signal({ text: "node" }, OWN_WINDOW, [{ text: "node" }]));
    assert.deepEqual(calls, []);
    assert.equal(listener.rejectedSignals(), 1);
  });
});

describe("договор имени якоря", () => {
  const NAME = "0123456789abcdef0123456789abcdef";
  it("принимается только точная форма: 32 строчные hex-цифры и ровно два поля", () => {
    assert.deepEqual(parseWatchHandedOffVideo({ kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: NAME }), { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: NAME });
    const rejected: unknown[] = [
      { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: NAME.slice(1) },
      { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: NAME.toUpperCase() },
      { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: `${NAME}0` },
      { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: "message" },
      { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: 7 },
      { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: NAME, extra: 1 },
      { kind: WATCH_HANDED_OFF_VIDEO_KIND },
      { kind: "watch_handed_off_videos", event_name: NAME },
      [WATCH_HANDED_OFF_VIDEO_KIND, NAME],
      null,
    ];
    for (const candidate of rejected) {
      assert.equal(parseWatchHandedOffVideo(candidate), null, JSON.stringify(candidate));
      assert.equal(parseChannelMessage(candidate), null, `MAIN-мир не принимает ${JSON.stringify(candidate)}`);
    }
  });
});
