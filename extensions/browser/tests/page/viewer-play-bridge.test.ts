// Режим 2 (сессия 12a) на настоящей сборке MAIN-мира: охрана и кредит клика приходят по личному порту,
// сторож задерживает запуск человека и доносит `viewer_play`; контроллер передачи этих команд не видит.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { parseIncomingPageMessage, parseChannelMessage } from "../../src/page/page-command-guard.ts";
import { parsePageEvent } from "../../src/shared/page-message-validation.ts";
import { AUDIO_MP4, VIDEO_MP4 } from "./support/media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"';
import { handshake, startRealShadowHarness } from "./support/bridge-harness.ts";

/** Страница с MSE-видео на паузе, у которого уже есть метаданные: кадр ещё не показан. */
function pausedFilmPage() {
  const harness = startRealShadowHarness();
  const video = harness.page.addVideo();
  video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
  const mediaSource = harness.page.attachMediaSource(video);
  mediaSource.duration = 5_400;
  mediaSource.addSourceBuffer(VIDEO_MIME).appendBuffer(VIDEO_MP4.bytes().slice(0, 733));
  mediaSource.addSourceBuffer(AUDIO_MIME).appendBuffer(AUDIO_MP4.bytes().slice());
  video.testSetState({ paused: true, readyState: 4 });
  return { ...harness, video };
}

const heldReports = (received: readonly unknown[]): PageEvent[] =>
  received.filter((message): message is PageEvent => (message as { kind?: string }).kind === "next_video_held");

test("кредит клика + охрана по порту: `play()` страницы задержан, донесение несёт `viewer_play` и проходит проверку изолированного мира", async () => {
  const { page, video } = pausedFilmPage();
  const isolated = handshake(page);
  try {
    isolated.send({ kind: "auto_handoff_watch", state: "on" });
    isolated.send({ kind: "grant_play_credit", lease_ms: 30_000 });
    await isolated.settle();
    const promise = (video as unknown as { play(): Promise<void> }).play();
    await isolated.settle();
    const reports = heldReports(isolated.received);
    assert.equal(reports.length, 1, "фон получил ровно одно донесение");
    const report = reports[0];
    assert.ok(report?.kind === "next_video_held");
    assert.equal(report.trigger, "viewer_play");
    assert.notEqual(parsePageEvent(JSON.parse(JSON.stringify(report))), undefined, "изолированный мир принимает форму донесения");
    assert.equal(video.paused === false, true, "страница видит, что видео играет");
    await promise;
  } finally {
    isolated.close();
  }
});

test("охрана выключена (по умолчанию): кредит клика в одиночку ничего не задерживает", async () => {
  const { page, video } = pausedFilmPage();
  const isolated = handshake(page);
  try {
    isolated.send({ kind: "grant_play_credit", lease_ms: 30_000 });
    await isolated.settle();
    await (video as unknown as { play(): Promise<void> }).play();
    await isolated.settle();
    assert.deepEqual(heldReports(isolated.received), []);
  } finally {
    isolated.close();
  }
});

test("охрана, включённая и снятая командой: после `off` сторож снова прозрачен", async () => {
  const { page, video } = pausedFilmPage();
  const isolated = handshake(page);
  try {
    isolated.send({ kind: "auto_handoff_watch", state: "on" });
    isolated.send({ kind: "grant_play_credit", lease_ms: 30_000 });
    isolated.send({ kind: "auto_handoff_watch", state: "off" });
    await isolated.settle();
    await (video as unknown as { play(): Promise<void> }).play();
    await isolated.settle();
    assert.deepEqual(heldReports(isolated.received), []);
  } finally {
    isolated.close();
  }
});

test("MAIN-мир разбирает команды режима 2 строго: лишние поля и числа вне диапазона — мусор", () => {
  assert.deepEqual(parseChannelMessage({ kind: "auto_handoff_watch", state: "on" }), { kind: "auto_handoff_watch", state: "on" });
  assert.deepEqual(parseChannelMessage({ kind: "grant_play_credit", lease_ms: 1_000 }), { kind: "grant_play_credit", lease_ms: 1_000 });
  assert.equal(parseChannelMessage({ kind: "auto_handoff_watch", state: "maybe" }), null);
  assert.equal(parseChannelMessage({ kind: "auto_handoff_watch", state: "on", extra: 1 }), null);
  assert.equal(parseChannelMessage({ kind: "grant_play_credit", lease_ms: 0 }), null);
  assert.equal(parseChannelMessage({ kind: "grant_play_credit", lease_ms: 600_001 }), null);
  assert.equal(parseIncomingPageMessage({ kind: "auto_handoff_watch", state: "on" }), null, "контроллеру передачи эти команды не принадлежат");
});
