// Тень доносит «ролик ушёл с экрана, и страница его остановила» (16b2, решения 354, 359). Проверяется то,
// что видит фон: событие `video_left_screen` наружу — когда оно приходит, когда нет и сколько раз.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { PageEvent } from "../../../src/shared/handoff-contract.ts";
import { ScreenExitWatch } from "../../../src/page/shadow/screen-exit-watch.ts";
import { createShadowHarness, emittedWithoutPulse, ON_SCREEN_AREA_PX, playingVideo } from "./shadow-harness.ts";

const LEFT_SCREEN: PageEvent = { kind: "video_left_screen" };

function screenExits(events: readonly PageEvent[]): number {
  return events.filter((event) => event.kind === "video_left_screen").length;
}

describe("тень: ролик ленты ушёл с экрана", () => {
  it("Instagram: ролик уже за краем окна, страница ставит паузу — донесение сразу за паузой", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleAreas.set(video, 0);
    video.pause();
    assert.deepEqual(emittedWithoutPulse(harness), [{ kind: "page_playback", request: { kind: "pause" } }, LEFT_SCREEN]);
  });

  it("ролик ушёл с экрана, но играет (листают к комментариям) — донесения нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.visibleAreas.set(video, 0);
    harness.tick(session, 10, "playing");
    harness.tick(session, 11, "playing");
    assert.equal(screenExits(harness.emitted), 0);
  });

  it("пауза на экране — донесения нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    video.pause();
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 0);
  });

  it("сначала пауза, потом ролик унесли с экрана — донесение на ближайшем тике часов", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    video.pause();
    assert.equal(screenExits(harness.emitted), 0);
    harness.visibleAreas.set(video, 0);
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 1);
  });

  it("один уход — одно донесение, сколько бы ни шло тиков", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.visibleAreas.set(video, 0);
    video.pause();
    for (let second = 10; second < 20; second += 1) harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 1);
  });

  it("вернулись к ролику и снова ушли — второе донесение", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.visibleAreas.set(video, 0);
    video.pause();
    harness.visibleAreas.set(video, ON_SCREEN_AREA_PX);
    void video.play();
    harness.tick(session, 10, "playing");
    harness.visibleAreas.set(video, 0);
    video.pause();
    assert.equal(screenExits(harness.emitted), 2);
  });

  it("ролик на паузе вернулся на экран и ушёл снова — тоже второе донесение", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.visibleAreas.set(video, 0);
    video.pause();
    harness.visibleAreas.set(video, ON_SCREEN_AREA_PX);
    harness.tick(session, 10, "paused");
    harness.visibleAreas.set(video, 0);
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 2);
  });
});

describe("правило ухода с экрана (ScreenExitWatch)", () => {
  it("донесение только для остановленного ролика с нулевой площадью, один раз на уход", () => {
    const watch = new ScreenExitWatch();
    assert.equal(watch.notePaused(ON_SCREEN_AREA_PX), "nothing");
    assert.equal(watch.notePaused(0), "report_left_screen");
    assert.equal(watch.notePaused(0), "nothing");
    watch.notePlaying();
    assert.equal(watch.notePaused(0), "report_left_screen");
  });
});
