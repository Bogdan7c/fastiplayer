// Запись Media Session страницы поверх настоящего стража: видно, какие обработчики страница
// поставила и дошло ли до них нажатие зрителя; поведение страницы и стража не меняется.

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { installMediaSessionGuard } from "../../src/page/media-session-guard.ts";
import { installMediaSessionRecording, type MediaSessionRecordingScope } from "../../src/page/stream-recorder/media-session-recording.ts";
import { StreamRecorder } from "../../src/page/stream-recorder/recorder.ts";
import { createFakeMediaSessionWindow } from "../page/shadow/fake-media-session.ts";
import { withoutTime } from "./support/recording-scope.ts";

function startRig() {
  const { win, mediaSession } = createFakeMediaSessionWindow();
  const guard = installMediaSessionGuard(win);
  const recorder = new StreamRecorder({ ctx: "main", now: () => 100, startMs: 100 });
  const oddities: string[] = [];
  const installed = installMediaSessionRecording(win as unknown as MediaSessionRecordingScope, recorder, (what) => oddities.push(what));
  return { mediaSession, guard, recorder, installed, oddities, page: win.navigator.mediaSession as unknown as MediaSession };
}

describe("запись Media Session", () => {
  it("обработчик страницы: поставлен, вызван стражем, вызов записан и дошёл до страницы с её аргументами", () => {
    const rig = startRig();
    const received: unknown[] = [];
    rig.page.setActionHandler("play", (details) => received.push(details));
    assert.deepEqual(rig.guard.invokePageAction("play"), { kind: "invoked" });
    assert.deepEqual(received, [{ action: "play" }], "страница получила нажатие как от браузера");
    assert.deepEqual(withoutTime(rig.recorder.sortedEvents()), [
      { ctx: "main", op: "media_session_handler", action: "play", set: true },
      { ctx: "main", op: "media_session_action", action: "play" },
    ]);
  });

  it("обработчика нет: страж честно отвечает no_handler, вызова в записи нет", () => {
    const rig = startRig();
    assert.deepEqual(rig.guard.invokePageAction("pause"), { kind: "no_handler" });
    assert.deepEqual(rig.recorder.sortedEvents(), []);
  });

  it("страница сняла обработчик: снятие записано, страж его больше не зовёт", () => {
    const rig = startRig();
    rig.page.setActionHandler("play", () => {});
    rig.page.setActionHandler("play", null);
    assert.deepEqual(rig.guard.invokePageAction("play"), { kind: "no_handler" });
    assert.deepEqual(
      withoutTime(rig.recorder.sortedEvents()).map((event) => event["set"]),
      [true, false],
    );
  });

  it("обработчик бросил: ошибка идёт к стражу как раньше, вызов записан", () => {
    const rig = startRig();
    const failure = new Error("page handler failed");
    rig.page.setActionHandler("pause", () => {
      throw failure;
    });
    assert.deepEqual(rig.guard.invokePageAction("pause"), { kind: "handler_failed", problem: failure });
    assert.equal(rig.recorder.sortedEvents().at(-1)?.["op"], "media_session_action");
  });

  it("браузер отверг действие: ошибка уходит странице, в записи ничего", () => {
    const rig = startRig();
    assert.throws(() => rig.page.setActionHandler("no-such-action" as MediaSessionAction, () => {}), TypeError);
    assert.deepEqual(rig.recorder.sortedEvents(), []);
  });

  it("нет Media Session в браузере: запись не ставится и не падает", () => {
    const recorder = new StreamRecorder({ ctx: "main", now: () => 100, startMs: 100 });
    assert.equal(installMediaSessionRecording({ navigator: {} }, recorder, () => {}), false);
  });
});
