// Тень доносит «ролик ушёл с экрана, и страница его остановила» (16b2, решения 354, 359). Проверяется то,
// что видит фон: событие `video_left_screen` наружу — когда оно приходит, когда нет и сколько раз.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { PageEvent } from "../../../src/shared/handoff-contract.ts";
import { REEL_ON_SCREEN_SHARE, ScreenExitWatch } from "../../../src/page/shadow/screen-exit-watch.ts";
import { createShadowHarness, emittedWithoutPulse, playingVideo, WHOLE_ON_SCREEN } from "./shadow-harness.ts";

const LEFT_SCREEN: PageEvent = { kind: "video_left_screen", exit: "scrolled_off_screen" };

function screenExits(events: readonly PageEvent[]): number {
  return events.filter((event) => event.kind === "video_left_screen").length;
}

/** Живой Instagram (18 ноября 2026, без входа): доля прежнего ролика на экране в миг его паузы (замер: 0,12–0,53). */
const INSTAGRAM_SHARES_AT_PAUSE = [0.12, 0.37, 0.41, 0.53] as const;
/** Полоска прежнего ролика, которая остаётся на экране после прилипания прокрутки (замер: ~15 px из 860). */
const PEEKING_STRIP_SHARE = 0.02;

describe("как ролик ушёл: прокруткой или вынут из документа (16b3, F4)", () => {
  it("пауза элемента в документе за краем окна — `scrolled_off_screen`: доказательство шага ленты", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0);
    video.pause();
    assert.deepEqual(emittedWithoutPulse(harness).at(-1), { kind: "video_left_screen", exit: "scrolled_off_screen" });
  });

  it("страница вынула элемент, потом пауза (уход Instagram и TikTok, шаг TikTok) — `removed_from_document`", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    video.remove();
    harness.visibleShares.set(video, 0);
    video.pause();
    assert.deepEqual(emittedWithoutPulse(harness).at(-1), { kind: "video_left_screen", exit: "removed_from_document" });
    assert.equal(screenExits(harness.emitted), 1, "донесение по-прежнему одно на уход");
  });

  it("сначала пауза в документе, потом элемент вынут — уход уже донесён прокруткой, тик второго не шлёт", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0.3);
    video.pause();
    video.remove();
    harness.visibleShares.set(video, 0);
    harness.tick(session, 10, "paused");
    const exits = harness.emitted.filter((event) => event.kind === "video_left_screen");
    assert.deepEqual(exits, [{ kind: "video_left_screen", exit: "scrolled_off_screen" }]);
  });

  it("вынутый элемент, остановленный до удаления, на тике — тоже `removed_from_document`", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    video.pause();
    video.remove();
    harness.visibleShares.set(video, 0);
    harness.tick(session, 10, "paused");
    assert.deepEqual(harness.emitted.filter((event) => event.kind === "video_left_screen"), [{ kind: "video_left_screen", exit: "removed_from_document" }]);
  });
});

describe("тень: ролик ленты ушёл с экрана", () => {
  for (const share of INSTAGRAM_SHARES_AT_PAUSE) {
    it(`живой Instagram: пауза посреди прокрутки, ролик ещё виден на ${share} — донесение сразу за паузой`, () => {
      const harness = createShadowHarness();
      const video = playingVideo(harness.dom);
      const session = harness.start(video);
      harness.tick(session, 10, "playing");
      harness.visibleShares.set(video, share);
      video.pause();
      assert.deepEqual(emittedWithoutPulse(harness), [{ kind: "page_playback", request: { kind: "pause" } }, LEFT_SCREEN]);
    });
  }

  it("после прилипания от ролика осталась полоска (площадь не 0) — уход уже донесён, второго донесения нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0.41);
    video.pause();
    harness.visibleShares.set(video, PEEKING_STRIP_SHARE);
    harness.tick(session, 10, "paused");
    // Страница зовёт pause() у прежнего ролика ещё раз, когда тот уходит совсем (следующая прокрутка).
    harness.visibleShares.set(video, 0);
    video.pause();
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 1);
  });

  it("пауза ролика, чуть задетого краем окна (виден почти целиком), — донесения нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0.9);
    video.pause();
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 0);
  });

  it("TikTok: ролик вынут из документа (доля 0), страница ставит паузу — донесение сразу за паузой", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0);
    video.pause();
    assert.deepEqual(emittedWithoutPulse(harness), [{ kind: "page_playback", request: { kind: "pause" } }, LEFT_SCREEN]);
  });

  it("ролик ушёл с экрана, но играет (листают к комментариям) — донесения нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.visibleShares.set(video, 0);
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
    harness.visibleShares.set(video, 0);
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 1);
  });

  it("один уход — одно донесение, сколько бы ни шло тиков", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.visibleShares.set(video, 0);
    video.pause();
    for (let second = 10; second < 20; second += 1) harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 1);
  });

  it("вернулись к ролику и снова ушли — второе донесение", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.visibleShares.set(video, 0);
    video.pause();
    harness.visibleShares.set(video, WHOLE_ON_SCREEN);
    void video.play();
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0);
    video.pause();
    assert.equal(screenExits(harness.emitted), 2);
  });

  it("ролик на паузе вернулся на экран и ушёл снова — тоже второе донесение", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.visibleShares.set(video, 0);
    video.pause();
    harness.visibleShares.set(video, WHOLE_ON_SCREEN);
    harness.tick(session, 10, "paused");
    harness.visibleShares.set(video, 0);
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 2);
  });
});

describe("тень: порог «меньше трёх четвертей» действует только на вызов pause() страницы", () => {
  it("серия доиграла, а плеер наполовину прокручен к комментариям — это не лента: донесения нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0.5);
    harness.tick(session, 10, "ended");
    harness.tick(session, 10, "ended");
    assert.equal(screenExits(harness.emitted), 0);
  });

  it("зритель поставил паузу в окне плеера, ролик наполовину на экране — донесения нет", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0.5);
    harness.tick(session, 10, "paused");
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 0);
  });

  it("ролик доиграл, и на экране его нет совсем — донесение на тике, как и раньше", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    harness.visibleShares.set(video, 0);
    harness.tick(session, 10, "ended");
    assert.equal(screenExits(harness.emitted), 1);
  });

  it("страница поставила паузу на экране, потом ролик прокрутили наполовину — донесения нет, пока он не уйдёт совсем", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom);
    const session = harness.start(video);
    harness.tick(session, 10, "playing");
    video.pause();
    harness.visibleShares.set(video, 0.5);
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 0);
    harness.visibleShares.set(video, 0);
    harness.tick(session, 10, "paused");
    assert.equal(screenExits(harness.emitted), 1);
  });
});

describe("правило ухода с экрана (ScreenExitWatch)", () => {
  it("пауза страницы: донесение, когда ролика на экране меньше порога, один раз на уход", () => {
    const watch = new ScreenExitWatch();
    assert.equal(watch.notePausedByPage(WHOLE_ON_SCREEN), "nothing");
    assert.equal(watch.notePausedByPage(0.4), "report_left_screen");
    assert.equal(watch.notePausedByPage(0), "nothing");
    watch.notePlaying();
    assert.equal(watch.notePausedByPage(0), "report_left_screen");
  });

  it("порог паузы страницы: ровно на пороге ролик ещё на экране, чуть меньше — уходит", () => {
    const onThreshold = new ScreenExitWatch();
    assert.equal(onThreshold.notePausedByPage(REEL_ON_SCREEN_SHARE), "nothing");
    const justBelow = new ScreenExitWatch();
    assert.equal(justBelow.notePausedByPage(REEL_ON_SCREEN_SHARE - 0.01), "report_left_screen");
  });

  it("порог оставляет запас до замеров живой ленты: прежний ролик в миг паузы виден не больше чем на 0,57", () => {
    assert.ok(REEL_ON_SCREEN_SHARE > 0.57 && REEL_ON_SCREEN_SHARE < 1);
  });

  it("тик часов: уход — только когда ролика на экране нет совсем", () => {
    const watch = new ScreenExitWatch();
    assert.equal(watch.noteStoppedOnTick(WHOLE_ON_SCREEN), "nothing");
    assert.equal(watch.noteStoppedOnTick(0.5), "nothing");
    assert.equal(watch.noteStoppedOnTick(0.02), "nothing");
    assert.equal(watch.noteStoppedOnTick(0), "report_left_screen");
    assert.equal(watch.noteStoppedOnTick(0), "nothing");
  });

  it("полоска уходящего ролика на тике возвратом не считается: об уходе, донесённом на паузе, второй раз не доносим", () => {
    const watch = new ScreenExitWatch();
    assert.equal(watch.notePausedByPage(0.41), "report_left_screen");
    assert.equal(watch.noteStoppedOnTick(0.02), "nothing");
    assert.equal(watch.noteStoppedOnTick(0), "nothing");
    assert.equal(watch.notePausedByPage(0), "nothing");
  });

  it("ролик на паузе вернулся на экран выше порога — следующий уход доносится снова", () => {
    const watch = new ScreenExitWatch();
    assert.equal(watch.notePausedByPage(0.3), "report_left_screen");
    assert.equal(watch.noteStoppedOnTick(WHOLE_ON_SCREEN), "nothing");
    assert.equal(watch.notePausedByPage(0.3), "report_left_screen");
    assert.equal(watch.noteStoppedOnTick(WHOLE_ON_SCREEN), "nothing");
    assert.equal(watch.noteStoppedOnTick(0), "report_left_screen");
  });
});
