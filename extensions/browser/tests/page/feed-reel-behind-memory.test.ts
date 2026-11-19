// Лента коротких видео, весь MAIN-мир (настоящие мост, слежение за MSE, журнал, свидетель просмотра и
// контроллер передачи на фейковой странице): ролик, пролистанный последним, бережёт память так же поздно, как
// начало ближайшего ролика впереди (16b3b, решение 377).
//
// Строка стенда `feed-back-live-many-pressure` (Chromium и Firefox): ролик A передан и пролистан, B передан,
// страница качает дальние ролики сверх потолка, человек листает назад к живому A. До решения 377 пролистанный A
// считался «пройденным» (ступень 1) и отдавал память первым: повтор журнала A — только init (734 байта из
// 31 820), страница свои данные второй раз не шлёт, и окно ждало у 3-й секунды без предела.
import assert from "node:assert/strict";
import { test } from "node:test";

import { handshake, startHarness, type BridgeHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { VIDEO_64_6S_MP4, concat } from "./support/media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const REEL_FILE = VIDEO_64_6S_MP4.bytes();
const INIT_END = VIDEO_64_6S_MP4.initEnd;
const SEGMENT_EDGES: readonly number[] = [...VIDEO_64_6S_MP4.mediaStarts, REEL_FILE.length];
/** Сегмент под 3-й секундой: сегменты файла начинаются на 0; 1,2; 2,4; 3,6; 4,8 с — это третий (2,4 с). */
const SEGMENT_UNDER_THIRD_SECOND = REEL_FILE.subarray(SEGMENT_EDGES[2] ?? 0, SEGMENT_EDGES[3] ?? 0);
/** Сколько дальних роликов страница качает сверх потолка. */
const FAR_REEL_COUNT = 3;
/** Потолок документа, как в точном сценарии строки стенда: два ролика целиком и немного сверху. */
const DOCUMENT_CAP_BYTES = REEL_FILE.length * 2 + 4_000;

/** Окно страницы 1280×720: ролик на экране, выше экрана (пролистан), ниже экрана (впереди). */
const ON_SCREEN = { left: 320, top: 0, right: 960, bottom: 720 };
const ONE_SCREEN_ABOVE = { left: 320, top: -740, right: 960, bottom: -20 };
const TWO_SCREENS_ABOVE = { left: 320, top: -1480, right: 960, bottom: -760 };
const screensBelow = (screens: number): typeof ON_SCREEN => ({ left: 320, top: 740 * screens, right: 960, bottom: 740 * screens + 720 });

type Page = BridgeHarness["page"];
type FakeVideo = ReturnType<Page["addVideo"]>;

interface Reel {
  readonly video: FakeVideo;
  readonly mediaSource: ReturnType<Page["attachMediaSource"]>;
  readonly buffer: ReturnType<ReturnType<Page["attachMediaSource"]>["addSourceBuffer"]>;
}

/** Ролик ленты в документе: свой `<video>` и свой MediaSource с одним SourceBuffer. */
function addReel(page: Page, rect: typeof ON_SCREEN): Reel {
  const video = page.addVideo();
  video.rect = rect;
  const mediaSource = page.attachMediaSource(video);
  mediaSource.duration = 6;
  return { video, mediaSource, buffer: mediaSource.addSourceBuffer(VIDEO_MIME) };
}

/**
 * Страница скачала ролик по сегментам (init, потом каждый медиасегмент своим куском), как живые ленты. Одним
 * куском нельзя: init журнала — вид на тот же буфер, и ролик, отдавший всё, держал бы память всего файла.
 */
function downloadBySegments(reel: Reel): void {
  for (let edge = 0; edge + 1 < SEGMENT_EDGES.length; edge += 1) {
    if (edge === 0) reel.buffer.appendBuffer(REEL_FILE.slice(0, INIT_END));
    reel.buffer.appendBuffer(REEL_FILE.slice(SEGMENT_EDGES[edge] ?? 0, SEGMENT_EDGES[edge + 1] ?? 0));
  }
}

/** Дальние ролики ниже экрана: страница качает их целиком, документ переходит потолок. */
function downloadFarReels(page: Page): void {
  const farReels = Array.from({ length: FAR_REEL_COUNT }, (_, index) => addReel(page, screensBelow(index + 2)));
  for (const reel of farReels) downloadBySegments(reel);
}

/** Передать элемент плееру: байты повтора и цели `page_seek`. */
async function handOff(harness: BridgeHarness, isolated: IsolatedSide, video: FakeVideo, requestId: number): Promise<{ sentBytes: Uint8Array; seekTargets: number[] }> {
  isolated.received.length = 0;
  const element = harness.bridge.registry.keyOf(video as unknown as Element);
  isolated.send({ kind: "begin_handoff", request_id: requestId, document: harness.bridge.documentTag, element });
  await isolated.settle();
  const outcome = isolated.received.find((message) => message.kind === "handoff_began");
  assert.deepEqual(outcome, { kind: "handoff_began", request_id: requestId, outcome: "started" });
  return {
    sentBytes: concat(isolated.received.flatMap((message) => (message.kind === "bytes" ? [message.bytes] : []))),
    seekTargets: isolated.received.flatMap((message) => (message.kind === "page_seek" ? [message.target_us] : [])),
  };
}

/** Шаг ленты при переезде (`LEAVE_PREVIOUS_FEED_REEL`): тень снята на её позиции, ролик на паузе. */
async function leaveFeedReel(isolated: IsolatedSide): Promise<void> {
  isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "exactly", resume: "paused" } });
  await isolated.settle();
}

/** Что журнал MediaSource ролика отдал бы плееру в повторе (байты по порядку). */
function journalBytes(harness: BridgeHarness, reel: Reel): Uint8Array {
  const journal = harness.bridge.tracker.recordOf(reel.mediaSource)?.journal;
  assert.ok(journal !== undefined, "the media source is tracked");
  return concat(journal.replay().flatMap((item) => (item.kind === "bytes" ? [item.view] : [])));
}

/** Есть ли в байтах `haystack` подряд байты `needle`. */
function containsBytes(haystack: Uint8Array, needle: Uint8Array): boolean {
  outer: for (let start = 0; start + needle.length <= haystack.length; start += 1) {
    for (let index = 0; index < needle.length; index += 1) {
      if (haystack[start + index] !== needle[index]) continue outer;
    }
    return true;
  }
  return false;
}

test("назад к живому ролику под давлением памяти: повтор пролистанного ролика покрывает его 3-ю секунду (решение 377)", async () => {
  const harness = startHarness({ memoryBudgetBytes: DOCUMENT_CAP_BYTES });
  const { page } = harness;
  const reelA = addReel(page, ON_SCREEN);
  const reelB = addReel(page, screensBelow(1));
  downloadBySegments(reelA);
  downloadBySegments(reelB);
  reelA.video.testSetState({ paused: false, currentTime: 3 });

  const isolated = handshake(page);
  try {
    // Ролик A передан на 3-й секунде; лента листнулась: тень снята на её позиции, A на паузе выше экрана.
    await handOff(harness, isolated, reelA.video, 2);
    await leaveFeedReel(isolated);
    reelA.video.testSetState({ paused: true, currentTime: 3 });
    reelA.video.rect = ONE_SCREEN_ABOVE;
    reelB.video.rect = ON_SCREEN;
    reelB.video.testSetState({ paused: false, currentTime: 0 });
    await handOff(harness, isolated, reelB.video, 3);
    // Страница качает три дальних ролика сверх потолка.
    downloadFarReels(page);

    // Назад к A: B уходит вниз, A снова на экране, переезд берёт живой A.
    await leaveFeedReel(isolated);
    reelB.video.testSetState({ paused: true });
    reelB.video.rect = screensBelow(1);
    reelA.video.rect = ON_SCREEN;
    const { sentBytes, seekTargets } = await handOff(harness, isolated, reelA.video, 4);
    assert.ok(seekTargets.length > 0 && seekTargets.every((target) => target === 3_000_000), `page_seek targets ${JSON.stringify(seekTargets)}`);
    assert.ok(sentBytes.length > INIT_END, `повтор A — не один init: ${sentBytes.length} байт`);
    assert.ok(containsBytes(sentBytes, SEGMENT_UNDER_THIRD_SECOND), "в повторе A есть сегмент под его 3-й секундой");
  } finally {
    isolated.close();
  }
});

/**
 * Два пролистанных ролика выше экрана (ближний и дальний), текущий играет, затем дальние ролики впереди качаются
 * сверх потолка. `nearCreatedFirst` — чей MediaSource страница завела раньше: правило не должно зависеть от этого.
 */
function twoReelsBehind(nearCreatedFirst: boolean): { harness: BridgeHarness; near: Reel; far: Reel } {
  const harness = startHarness({ memoryBudgetBytes: DOCUMENT_CAP_BYTES });
  const { page } = harness;
  const first = addReel(page, nearCreatedFirst ? ONE_SCREEN_ABOVE : TWO_SCREENS_ABOVE);
  const second = addReel(page, nearCreatedFirst ? TWO_SCREENS_ABOVE : ONE_SCREEN_ABOVE);
  const [near, far] = nearCreatedFirst ? [first, second] : [second, first];
  for (const reel of [near, far]) {
    downloadBySegments(reel);
    // Ролик смотрели и пролистали: страница остановила его выше экрана.
    reel.video.testSetState({ paused: false, currentTime: 3 });
    reel.video.testSetState({ paused: true });
  }
  const current = addReel(page, ON_SCREEN);
  // У текущего ролика только init: потолок держит два ролика целиком и init остальных, иначе ступень 7
  // забрала бы и у ближнего ролика позади, и проверка смотрела бы на развязку внутри ступени, а не на правило.
  current.buffer.appendBuffer(REEL_FILE.slice(0, INIT_END));
  current.video.testSetState({ paused: false, currentTime: 0.5 });
  // Между уходом обоих роликов с экрана страница ничего не качала: оба станут пройденными в одном наведении порядка.
  downloadFarReels(page);
  return { harness, near, far };
}

for (const nearCreatedFirst of [true, false]) {
  test(`два ролика позади: дальний по-прежнему отдаёт память раньше ближнего (ближний MediaSource ${nearCreatedFirst ? "старше" : "новее"})`, () => {
    const { harness, near, far } = twoReelsBehind(nearCreatedFirst);
    assert.deepEqual(journalBytes(harness, far), REEL_FILE.subarray(0, INIT_END), "дальний пролистанный ролик отдал всё, кроме init");
    assert.deepEqual(journalBytes(harness, near), REEL_FILE, "ближний пролистанный ролик цел");
  });
}
