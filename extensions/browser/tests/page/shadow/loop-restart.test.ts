// Атрибут `loop` в тени (16b2, стандарт HTML «reaches the end»): ролик с `loop` не кончается, а идёт с начала.
// Живой Shorts через мост без этого стоял в окне на последнем кадре: часы плеера сказали `ended`, а страница
// ждала повтора от браузера. Проверяется, что видят страница (события, `ended`, позиция) и плеер (перемотка, play).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks, recordEvents } from "./fake-media-dom.ts";
import { createShadowHarness, emittedWithoutPulse, playingVideo } from "./shadow-harness.ts";

const MEDIA_EVENTS = ["play", "playing", "pause", "timeupdate", "seeking", "seeked", "ended"];

describe("тень: ролик с loop доиграл (Shorts)", () => {
  it("страница не видит ни pause, ни ended — видит перемотку на начало; плееру — перемотка и play", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 99);
    video.loop = true;
    const page = recordEvents(video, MEDIA_EVENTS);
    const session = harness.start(video);
    harness.tick(session, 99.5, "playing");
    harness.tick(session, 100, "ended");
    await flushMicrotasks();
    assert.equal(video.ended, false, "по стандарту ролик с loop не кончается");
    assert.equal(video.paused, false);
    assert.ok(!page.types().includes("ended") && !page.types().includes("pause"), `события страницы: ${page.types().join(",")}`);
    assert.ok(page.types().includes("seeking"), "страница видит перемотку на начало");
    assert.deepEqual(emittedWithoutPulse(harness).slice(-2), [
      { kind: "page_seek", target_us: 0 },
      { kind: "page_playback", request: { kind: "play" } },
    ]);
    // Плеер исполнил: часы снова с начала — перемотка у страницы завершилась, ролик идёт.
    harness.tick(session, 0.04, "playing");
    harness.tick(session, 0.3, "playing");
    await flushMicrotasks();
    assert.ok(page.types().includes("seeked"));
    assert.ok(video.currentTime < 1, `позиция страницы после повтора: ${video.currentTime}`);
    assert.equal(video.ended, false);
  });

  it("каждый проход ролика с loop даёт свой повтор", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 99);
    video.loop = true;
    const session = harness.start(video);
    harness.tick(session, 100, "ended");
    harness.tick(session, 0.1, "playing");
    harness.tick(session, 99.9, "playing");
    harness.tick(session, 100, "ended");
    await flushMicrotasks();
    const restarts = emittedWithoutPulse(harness).filter((event) => event.kind === "page_seek" && event.target_us === 0);
    assert.equal(restarts.length, 2);
    assert.equal(video.ended, false);
  });

  it("без loop — прежний конец: pause и ended, плееру ничего не шлётся", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 99);
    const page = recordEvents(video, MEDIA_EVENTS);
    const session = harness.start(video);
    harness.tick(session, 100, "ended");
    await flushMicrotasks();
    assert.equal(video.ended, true);
    assert.deepEqual(page.types().slice(-2), ["pause", "ended"]);
    assert.ok(!emittedWithoutPulse(harness).some((event) => event.kind === "page_seek"));
  });

  it("страница сняла loop посреди ролика (Shorts после первого ролика) — ролик кончается как обычно", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 50);
    video.loop = true;
    const session = harness.start(video);
    harness.tick(session, 60, "playing");
    video.loop = false;
    harness.tick(session, 100, "ended");
    await flushMicrotasks();
    assert.equal(video.ended, true);
  });
});
