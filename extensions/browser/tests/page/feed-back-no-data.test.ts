// Лента коротких видео, весь MAIN-мир (настоящие мост, журнал, свидетель просмотра, тень и сторож на фейковой
// странице): назад к ролику, данные которого «память без копий» уже отдала (16b3b, решение 378).
//
// Страница знает это заранее, до передачи: сторож доносит фону `start_data: "lost"`. Фон отвечает «отпустить», и
// ролик обязан заиграть в браузере сам — не остаться на паузе под тенью и не получить `AbortError`, которого
// страница не вызывала. Повтор вопроса фону после ухода нашего ролика с экрана (правило F6) тоже несёт признак.
import assert from "node:assert/strict";
import { test } from "node:test";

import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { handshake, startRealShadowHarness, type IsolatedSide, type RealShadowHarness } from "./support/bridge-harness.ts";
import { VIDEO_64_6S_MP4, concat } from "./support/media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const REEL_FILE = VIDEO_64_6S_MP4.bytes();
const INIT_END = VIDEO_64_6S_MP4.initEnd;
const SEGMENT_EDGES: readonly number[] = [...VIDEO_64_6S_MP4.mediaStarts, REEL_FILE.length];
/** Потолок документа: два ролика целиком и немного сверху (как строка стенда `feed-back-live-many-pressure`). */
const DOCUMENT_CAP_BYTES = REEL_FILE.length * 2 + 4_000;
/** Срок взвода сторожа, как у фона (`NEXT_VIDEO_ARM_LEASE_MS`). */
const ARM_LEASE_MS = 3_000;
/** `HTMLMediaElement.HAVE_ENOUGH_DATA`: у страницы ролик скачан. */
const HAVE_ENOUGH_DATA = 4;

const ON_SCREEN = { left: 320, top: 0, right: 960, bottom: 720 };
const ONE_SCREEN_ABOVE = { left: 320, top: -740, right: 960, bottom: -20 };
const TWO_SCREENS_ABOVE = { left: 320, top: -1480, right: 960, bottom: -760 };
const screensBelow = (screens: number): typeof ON_SCREEN => ({ left: 320, top: 740 * screens, right: 960, bottom: 740 * screens + 720 });

type Page = RealShadowHarness["page"];
type FakeVideo = ReturnType<Page["addVideo"]>;

interface Reel {
  readonly video: FakeVideo;
  readonly mediaSource: ReturnType<Page["attachMediaSource"]>;
}

/** Ролик ленты, скачанный страницей целиком по сегментам. */
function addDownloadedReel(page: Page, rect: typeof ON_SCREEN): Reel {
  const video = page.addVideo();
  video.rect = rect;
  const mediaSource = page.attachMediaSource(video);
  mediaSource.duration = 6;
  const buffer = mediaSource.addSourceBuffer(VIDEO_MIME);
  buffer.appendBuffer(REEL_FILE.slice(0, INIT_END));
  for (let edge = 0; edge + 1 < SEGMENT_EDGES.length; edge += 1) buffer.appendBuffer(REEL_FILE.slice(SEGMENT_EDGES[edge] ?? 0, SEGMENT_EDGES[edge + 1] ?? 0));
  return { video, mediaSource };
}

/** Ролик смотрели до 3-й секунды и пролистали: страница остановила его выше экрана. */
function watchedAndScrolledPast(reel: Reel, rect: typeof ON_SCREEN): void {
  reel.video.testSetState({ paused: false, currentTime: 3 });
  reel.video.testSetState({ paused: true, readyState: HAVE_ENOUGH_DATA });
  reel.video.rect = rect;
}

function journalBytes(harness: RealShadowHarness, reel: Reel): Uint8Array {
  const journal = harness.bridge.tracker.recordOf(reel.mediaSource)?.journal;
  assert.ok(journal !== undefined);
  return concat(journal.replay().flatMap((item) => (item.kind === "bytes" ? [item.view] : [])));
}

function heldReportsOf(isolated: IsolatedSide, element: number): Extract<PageEvent, { kind: "next_video_held" }>[] {
  return isolated.received.filter(
    (message): message is Extract<PageEvent, { kind: "next_video_held" }> => (message as PageEvent).kind === "next_video_held" && (message as { element?: number }).element === element,
  );
}

test("назад на дальний ролик без данных: сторож доносит `lost`, фон отпускает — ролик играет в браузере, без AbortError", async () => {
  const harness = startRealShadowHarness({ memoryBudgetBytes: DOCUMENT_CAP_BYTES });
  const { page } = harness;
  // Два пролистанных ролика (дальний — два экрана выше) и текущий на экране.
  const farBehind = addDownloadedReel(page, ON_SCREEN);
  watchedAndScrolledPast(farBehind, TWO_SCREENS_ABOVE);
  const nearBehind = addDownloadedReel(page, ON_SCREEN);
  watchedAndScrolledPast(nearBehind, ONE_SCREEN_ABOVE);
  const current = addDownloadedReel(page, ON_SCREEN);
  current.video.testSetState({ paused: false, currentTime: 1, readyState: HAVE_ENOUGH_DATA });
  const isolated = handshake(page);
  try {
    const currentKey = harness.bridge.registry.keyOf(current.video as unknown as Element);
    isolated.send({ kind: "begin_handoff", request_id: 2, document: harness.bridge.documentTag, element: currentKey });
    await isolated.settle();
    // Страница качает дальние ролики впереди сверх потолка: дальний ролик позади отдаёт всё (ступень 1).
    for (const screens of [1, 2, 3]) addDownloadedReel(page, screensBelow(screens));
    assert.deepEqual(journalBytes(harness, farBehind), REEL_FILE.subarray(0, INIT_END), "у дальнего ролика позади в журнале один init");

    // Лента листается: фон взводит сторож, наш ролик уходит вниз, страница запускает дальний ролик позади.
    isolated.send({ kind: "arm_next_video", lease_ms: ARM_LEASE_MS });
    await isolated.settle();
    isolated.received.length = 0;
    current.video.rect = screensBelow(1);
    current.video.pause();
    farBehind.video.rect = ON_SCREEN;
    const farKey = harness.bridge.registry.keyOf(farBehind.video as unknown as Element);
    let playOutcome: "pending" | "resolved" | string = "pending";
    farBehind.video.play().then(
      () => (playOutcome = "resolved"),
      (error: unknown) => (playOutcome = `rejected: ${(error as { name?: string }).name ?? String(error)}`),
    );
    await isolated.settle();
    const [firstReport] = heldReportsOf(isolated, farKey);
    assert.equal(firstReport?.start_data, "lost", "страница знает заранее: данных под 3-й секундой у журнала нет");

    // Фон: «отпустить» (решение 378). Сторож может спросить ещё раз (F6) — признак тот же, ответ тот же.
    isolated.send({ kind: "release_held_video", element: farKey });
    await isolated.settle();
    const reports = heldReportsOf(isolated, farKey);
    assert.ok(reports.every((report) => report.start_data === "lost"), "повтор вопроса тоже несёт `lost`");
    if (reports.length > 1) {
      isolated.send({ kind: "release_held_video", element: farKey });
      await isolated.settle();
    }

    assert.equal(farBehind.video.paused, false, "ролик играет в браузере сам, а не стоит под тенью");
    assert.equal(playOutcome, "resolved", "обещание play() страницы исполнено, AbortError нет");
  } finally {
    isolated.close();
  }
});
