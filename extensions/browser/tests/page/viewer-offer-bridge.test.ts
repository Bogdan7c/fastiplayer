// Предложение «Ask» (12b) на настоящей сборке MAIN-мира: команды приходят по личному порту в строгой форме,
// сторож держит видео под предложением, сигнал предложения встаёт на самом элементе и уходит на окне.
import assert from "node:assert/strict";
import { test } from "node:test";
import { viewerOfferEventNameOf, WATCH_HANDED_OFF_VIDEO_KIND } from "../../src/shared/handed-off-video-contract.ts";
import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { AUDIO_MP4, VIDEO_MP4 } from "./support/media-samples.ts";
import { handshake, startRealShadowHarness } from "./support/bridge-harness.ts";
import { WATCH_ON } from "../support/auto-handoff-watch-fixtures.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"';
const EVENT_NAME = "00112233445566778899aabbccddeeff";
const OFFER_EVENT = viewerOfferEventNameOf(EVENT_NAME);

/** Страница с MSE-фильмом на паузе (метаданные есть), охрана режима 2 включена, у человека кредит клика. */
async function offeredFilmPage() {
  const harness = startRealShadowHarness();
  const video = harness.page.addVideo();
  video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
  const mediaSource = harness.page.attachMediaSource(video);
  mediaSource.duration = 5_400;
  mediaSource.addSourceBuffer(VIDEO_MIME).appendBuffer(VIDEO_MP4.bytes().slice(0, 733));
  mediaSource.addSourceBuffer(AUDIO_MIME).appendBuffer(AUDIO_MP4.bytes().slice());
  video.testSetState({ paused: true, readyState: 4 });
  const onVideo: Event[] = [];
  const onWindow: Event[] = [];
  video.addEventListener(OFFER_EVENT, (event) => void onVideo.push(event));
  harness.page.win.addEventListener(OFFER_EVENT, (event) => {
    if (event.target === harness.page.win) onWindow.push(event);
  });
  const isolated = handshake(harness.page);
  isolated.send({ kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: EVENT_NAME });
  isolated.send({ kind: "auto_handoff_watch", ...WATCH_ON });
  isolated.send({ kind: "grant_play_credit", lease_ms: 30_000 });
  await isolated.settle();
  const play = (video as unknown as { play(): Promise<void> }).play();
  await isolated.settle();
  const held = isolated.received.find((message): message is Extract<PageEvent, { kind: "next_video_held" }> => (message as { kind?: string }).kind === "next_video_held");
  assert.ok(held !== undefined, "фону ушло донесение");
  isolated.send({ kind: "hold_for_viewer_offer", element: held.element });
  await isolated.settle();
  await play;
  return { video, isolated, onVideo, onWindow, element: held.element };
}

test("«держи под предложением» по порту: страница видит паузу, сигнал встал на самом элементе; «отпусти» — игра и «ушло»", async () => {
  const { video, isolated, onVideo, onWindow, element } = await offeredFilmPage();
  try {
    assert.equal(video.paused, true, "плеер сайта видит паузу");
    assert.equal(onVideo.length, 1);
    assert.equal(onVideo[0]?.composed, true);
    assert.equal(onWindow.length, 0);
    isolated.send({ kind: "release_held_video", element });
    await isolated.settle();
    assert.equal(video.paused, false, "«Play here»: видео играет в браузере");
    assert.equal(onWindow.length, 1, "панель уходит");
  } finally {
    isolated.close();
  }
});

test("«сними предложение» по порту: видео на паузе, сигнал «ушло»; лишнее поле — не наша команда", async () => {
  const { video, isolated, onWindow, element } = await offeredFilmPage();
  try {
    isolated.send({ kind: "withdraw_viewer_offer", element, extra: true });
    await isolated.settle();
    assert.equal(onWindow.length, 0, "команда не по форме отброшена");
    isolated.send({ kind: "withdraw_viewer_offer", element });
    await isolated.settle();
    assert.equal(onWindow.length, 1);
    assert.equal(video.paused, true);
  } finally {
    isolated.close();
  }
});
