// Лента коротких видео, весь MAIN-мир (настоящие мост, тень и сторож на фейковой странице): гонка живого
// Instagram (16b2, находка живой проверки, решение 361).
//
// Страница одной задачей зовёт `play()` следующего ролика и `pause()` нашего, пока тот ещё наполовину на
// экране. Сторож взведён хвостом прежнего взвода и задерживает следующий ролик раньше, чем фон узнал об
// уходе нашего; фон отвечает «не сейчас». Ролик обязан быть задержан снова и предложен фону ещё раз —
// иначе он играл бы в браузере со звуком и без заглушки, а окно плеера стояло бы на паузе.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { handshake, startRealShadowHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { addPlayingMseVideo, describeAndBegin } from "./support/handoff-steps.ts";

/** Срок взвода сторожа, как у фона (`NEXT_VIDEO_ARM_LEASE_MS`). */
const ARM_LEASE_MS = 3_000;
/** `HTMLMediaElement.HAVE_ENOUGH_DATA`: следующий ролик ленты загружен заранее. */
const HAVE_ENOUGH_DATA = 4;

/** На странице два видео с ключами 1 и 2: ключ того, которое не передано. */
function otherKeyOf(handedOffKey: number): number {
  return handedOffKey === 1 ? 2 : 1;
}

function heldReportsOf(received: IsolatedSide["received"], element: number): number {
  return received.filter((message) => (message as PageEvent).kind === "next_video_held" && (message as { element?: number }).element === element).length;
}

function eventKinds(received: IsolatedSide["received"]): string[] {
  return received.map((message) => (message as { kind: string }).kind).filter((kind) => kind !== "shadow_alive");
}

test("лента Instagram: следующий ролик задержан до ухода нашего, отказ фона опоздал — ролик задержан снова и не играет в браузере", async () => {
  const harness = startRealShadowHarness();
  const { video: ourReel } = addPlayingMseVideo(harness);
  const { video: nextReel } = addPlayingMseVideo(harness);
  // Следующий ролик стоит ниже нашего, загружен, но не запущен.
  nextReel.rect = { left: 0, top: 380, right: 640, bottom: 740 };
  nextReel.testSetState({ paused: true, currentTime: 0, readyState: HAVE_ENOUGH_DATA });
  const isolated = handshake(harness.page);
  try {
    const ourKey = await describeAndBegin(isolated);
    await isolated.settle();
    const nextKey = otherKeyOf(ourKey);
    // Хвост прежнего взвода: фон взводил сторож на прошлом шаге ленты и уже перестал, срок ещё идёт.
    isolated.send({ kind: "arm_next_video", lease_ms: ARM_LEASE_MS });
    await isolated.settle();
    isolated.received.length = 0;

    // Одна задача страницы: play() следующего, pause() нашего (в окне 1280×720 от него осталось 41 %).
    void nextReel.play().catch(() => undefined);
    ourReel.rect = { left: 0, top: -210, right: 640, bottom: 150 };
    ourReel.pause();
    await isolated.settle();
    assert.equal(heldReportsOf(isolated.received, nextKey), 1, "следующий ролик задержан в самом вызове play()");
    assert.ok(eventKinds(isolated.received).includes("video_left_screen"), "тень донесла уход нашего ролика");

    // Фон успел ответить на донесение о задержанном ролике раньше, чем узнал об уходе нашего: «отпустить».
    isolated.send({ kind: "release_held_video", element: nextKey });
    await isolated.settle();
    assert.equal(heldReportsOf(isolated.received, nextKey), 2, "фон спрошен о том же ролике заново");
  } finally {
    isolated.close();
  }
});

test("лента: без ухода нашего ролика отказ фона остаётся отказом — превью рядом не переспрашивается", async () => {
  const harness = startRealShadowHarness();
  addPlayingMseVideo(harness);
  const { video: preview } = addPlayingMseVideo(harness);
  preview.rect = { left: 700, top: 0, right: 1020, bottom: 180 };
  preview.testSetState({ paused: true, currentTime: 0, readyState: HAVE_ENOUGH_DATA });
  const isolated = handshake(harness.page);
  try {
    const ourKey = await describeAndBegin(isolated);
    await isolated.settle();
    const previewKey = otherKeyOf(ourKey);
    isolated.send({ kind: "arm_next_video", lease_ms: ARM_LEASE_MS });
    await isolated.settle();
    isolated.received.length = 0;

    void preview.play().catch(() => undefined);
    await isolated.settle();
    assert.equal(heldReportsOf(isolated.received, previewKey), 1);
    isolated.send({ kind: "release_held_video", element: previewKey });
    await isolated.settle();
    // Продление взвода обходит видео документа: отпущенное превью больше не задерживается.
    isolated.send({ kind: "arm_next_video", lease_ms: ARM_LEASE_MS });
    await isolated.settle();
    assert.equal(heldReportsOf(isolated.received, previewKey), 1);
    assert.ok(!eventKinds(isolated.received).includes("video_left_screen"));
  } finally {
    isolated.close();
  }
});
