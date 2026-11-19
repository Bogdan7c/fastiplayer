// Лента коротких видео, весь MAIN-мир (настоящие мост, слежение за MSE, журнал, свидетель просмотра и
// контроллер передачи на фейковой странице): страница сбросила пройденные ролики пачкой, а начала дальних
// роликов переполнили «память без копий» (16b2, находка 3, решение 364).
//
// Замер во вкладке владельца (Instagram, 19 ноября 2026): лента держит живыми около 20 MediaSource, в потолок
// упирается сразу, пройденные ролики сбрасывает пачками. После сброса самый старый живой MediaSource —
// текущий ролик, за ним следующий. Потолок, отдававший память по возрасту, съедал ровно их: передача ждала
// данных у текущей секунды 10 с, а после прокрутки следующий ролик приходил без начала.
import assert from "node:assert/strict";
import { test } from "node:test";

import { handshake, startHarness, type BridgeHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { VIDEO_64_6S_MP4, concat } from "./support/media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
/** `HTMLMediaElement.HAVE_ENOUGH_DATA`: ролик загружен заранее. */
const HAVE_ENOUGH_DATA = 4;

const REEL_FILE = VIDEO_64_6S_MP4.bytes();
const INIT_END = VIDEO_64_6S_MP4.initEnd;
const SEGMENT_EDGES: readonly number[] = [...VIDEO_64_6S_MP4.mediaStarts, REEL_FILE.length];
/** Заранее загруженное начало ролика: init и первый медиасегмент. */
const START_BYTES = SEGMENT_EDGES[1] ?? 0;
/** Сколько дальних роликов страница грузит заранее. */
const FAR_REEL_COUNT = 3;
/**
 * Потолок документа в тесте: влезают текущий ролик целиком, начало следующего, init дальних роликов и
 * ещё один сегмент. Дальние ролики, скачанные целиком, не влезают.
 */
const DOCUMENT_CAP_BYTES = REEL_FILE.length + START_BYTES + FAR_REEL_COUNT * INIT_END + 8_000;

/** Окно страницы 1280×720: текущий ролик на экране, следующий — на экран ниже, пройденные — выше. */
const ON_SCREEN = { left: 320, top: 0, right: 960, bottom: 720 };
const NEXT_BELOW = { left: 320, top: 740, right: 960, bottom: 1460 };
const PASSED_ABOVE = { left: 320, top: -1480, right: 960, bottom: -760 };

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
  return openSource(page, video);
}

/** Дальний ролик: страница готовит его элемент заранее и в документ ещё не вставила. */
function addReelOutsideDocument(page: Page): Reel {
  const video = new page.FakeVideoElement();
  video.connected = false;
  return openSource(page, video);
}

function openSource(page: Page, video: FakeVideo): Reel {
  const mediaSource = page.attachMediaSource(video);
  mediaSource.duration = 6;
  return { video, mediaSource, buffer: mediaSource.addSourceBuffer(VIDEO_MIME) };
}

/** Кусок файла в собственном буфере, как отдельная загрузка страницы. */
function download(reel: Reel, start: number, end: number): void {
  reel.buffer.appendBuffer(REEL_FILE.slice(start, end));
}

function downloadStart(reel: Reel): void {
  download(reel, 0, INIT_END);
  download(reel, INIT_END, START_BYTES);
}

/** Медиасегмент номер `segment` (с нуля). */
function downloadSegment(reel: Reel, segment: number): void {
  download(reel, SEGMENT_EDGES[segment] ?? 0, SEGMENT_EDGES[segment + 1] ?? 0);
}

/**
 * Страница в форме замера: два пройденных ролика, текущий играет на `playheadSeconds`, следующий загружен
 * заранее; затем пройденные сброшены пачкой, а три дальних ролика скачаны по кругу сверх потолка.
 */
function buildFeedAfterTheDrop(harness: BridgeHarness): { playing: Reel; next: Reel } {
  const { page } = harness;
  const passedReels = [addReel(page, PASSED_ABOVE), addReel(page, PASSED_ABOVE)];
  for (const reel of passedReels) {
    downloadStart(reel);
    // Ролик смотрели и пролистали: страница остановила его.
    reel.video.testSetState({ paused: false, currentTime: 3 });
    reel.video.testSetState({ paused: true });
  }
  const playing = addReel(page, ON_SCREEN);
  const next = addReel(page, NEXT_BELOW);
  downloadStart(playing);
  downloadStart(next);
  playing.video.testSetState({ paused: false, currentTime: 0.5 });
  next.video.testSetState({ paused: true, currentTime: 0, readyState: HAVE_ENOUGH_DATA });
  for (let segment = 1; segment < SEGMENT_EDGES.length - 1; segment += 1) downloadSegment(playing, segment);

  // Страница сбрасывает пройденные ролики пачкой: элементы убраны, MediaSource закрыты.
  for (const reel of passedReels) {
    reel.video.connected = false;
    page.document.children.splice(page.document.children.indexOf(reel.video), 1);
    reel.mediaSource.testClose();
  }
  // И грузит дальние ролики: начала по кругу, потом остальное — документ переходит потолок.
  const farReels = Array.from({ length: FAR_REEL_COUNT }, () => addReelOutsideDocument(page));
  for (const reel of farReels) download(reel, 0, INIT_END);
  for (let segment = 0; segment < SEGMENT_EDGES.length - 1; segment += 1) {
    for (const reel of farReels) downloadSegment(reel, segment);
  }
  return { playing, next };
}

/** Передать элемент плееру и вернуть байты, которые ушли в повторе, и цели `page_seek`. */
async function handOff(harness: BridgeHarness, isolated: IsolatedSide, video: FakeVideo): Promise<{ sentBytes: Uint8Array; seekTargets: number[] }> {
  isolated.received.length = 0;
  isolated.send({ kind: "begin_handoff", request_id: 2, document: harness.bridge.documentTag, element: harness.bridge.registry.keyOf(video as unknown as Element) });
  await isolated.settle();
  const outcome = isolated.received.find((message) => message.kind === "handoff_began");
  assert.deepEqual(outcome, { kind: "handoff_began", request_id: 2, outcome: "started" });
  return {
    sentBytes: concat(isolated.received.flatMap((message) => (message.kind === "bytes" ? [message.bytes] : []))),
    seekTargets: isolated.received.flatMap((message) => (message.kind === "page_seek" ? [message.target_us] : [])),
  };
}

test("лента: страница сбросила пройденные ролики и скачала дальние сверх потолка — следующий ролик уходит плееру с самого начала", async () => {
  const harness = startHarness({ memoryBudgetBytes: DOCUMENT_CAP_BYTES });
  const { next } = buildFeedAfterTheDrop(harness);

  const isolated = handshake(harness.page);
  try {
    // Лента листнулась: фон переводит окно на следующий ролик.
    const { sentBytes, seekTargets } = await handOff(harness, isolated, next.video);
    assert.ok(seekTargets.length > 0 && seekTargets.every((target) => target === 0), `page_seek targets ${JSON.stringify(seekTargets)}`);
    assert.deepEqual(sentBytes, REEL_FILE.subarray(0, START_BYTES), "повтор следующего ролика — его init и первый сегмент целиком");
  } finally {
    isolated.close();
  }
});

test("лента: после сброса пройденных роликов текущий ролик отдаётся плееру с данными под его текущей секундой", async () => {
  const harness = startHarness({ memoryBudgetBytes: DOCUMENT_CAP_BYTES });
  const { playing } = buildFeedAfterTheDrop(harness);

  const isolated = handshake(harness.page);
  try {
    const { sentBytes, seekTargets } = await handOff(harness, isolated, playing.video);
    // Страница стоит на 0,5 с: это внутри первого сегмента, и он обязан быть в повторе.
    assert.ok(seekTargets.length > 0 && seekTargets.every((target) => target === 500_000), `page_seek targets ${JSON.stringify(seekTargets)}`);
    assert.deepEqual(sentBytes, REEL_FILE, "повтор текущего ролика — весь ролик с нуля");
  } finally {
    isolated.close();
  }
});

/** Что журнал MediaSource ролика отдал бы плееру в повторе (байты по порядку). */
function journalBytes(harness: BridgeHarness, reel: Reel): Uint8Array {
  const journal = harness.bridge.tracker.recordOf(reel.mediaSource)?.journal;
  assert.ok(journal !== undefined, "the media source is tracked");
  return concat(journal.replay().flatMap((item) => (item.kind === "bytes" ? [item.view] : [])));
}

test("лента: ролик, который уже идёт в окне плеера, отдаёт память раньше начал ещё не игравших роликов", async () => {
  // Влезает ровно то, что страница скачала к передаче; дальше следующий ролик докачивает два сегмента.
  const farBelow = (screens: number): typeof ON_SCREEN => ({ left: 320, top: 740 * screens, right: 960, bottom: 740 * screens + 720 });
  const cap = REEL_FILE.length + 3 * START_BYTES + 1_000;
  const harness = startHarness({ memoryBudgetBytes: cap });
  const { page } = harness;
  // MediaSource страница заводит не строго по порядку ленты: здесь текущий ролик — самый новый из четырёх.
  const farReels = [addReel(page, farBelow(2)), addReel(page, farBelow(3))];
  const next = addReel(page, NEXT_BELOW);
  const handedOff = addReel(page, ON_SCREEN);
  for (const reel of [handedOff, next, ...farReels]) downloadStart(reel);
  for (let segment = 1; segment < SEGMENT_EDGES.length - 1; segment += 1) downloadSegment(handedOff, segment);
  handedOff.video.testSetState({ paused: false, currentTime: 0.5 });
  for (const reel of [next, ...farReels]) reel.video.testSetState({ paused: true, currentTime: 0, readyState: HAVE_ENOUGH_DATA });

  const isolated = handshake(harness.page);
  try {
    // Ролик отдан: плеер получил весь его журнал, дальше берёт живой поток.
    const { sentBytes } = await handOff(harness, isolated, handedOff.video);
    assert.deepEqual(sentBytes, REEL_FILE);
    // Страница докачивает следующий ролик: документ переходит потолок на два сегмента.
    downloadSegment(next, 1);
    downloadSegment(next, 2);

    // Память отдал ролик, который уже в плеере: два его первых сегмента. Начала остальных целы.
    assert.deepEqual(journalBytes(harness, handedOff), concat([REEL_FILE.subarray(0, INIT_END), REEL_FILE.subarray(SEGMENT_EDGES[2] ?? 0)]));
    assert.deepEqual(journalBytes(harness, next), REEL_FILE.subarray(0, SEGMENT_EDGES[3] ?? 0), "следующий ролик цел");
    for (const reel of farReels) assert.deepEqual(journalBytes(harness, reel), REEL_FILE.subarray(0, START_BYTES), "начало дальнего ролика цело");
  } finally {
    isolated.close();
  }
});
