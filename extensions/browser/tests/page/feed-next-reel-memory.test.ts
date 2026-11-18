// Лента коротких видео, весь MAIN-мир (настоящие мост, слежение за MSE, журнал и контроллер передачи на
// фейковой странице): следующий ролик обязан уйти плееру С НАЧАЛА, даже когда документ дошёл до потолка
// «памяти без копий» (16b2, находка живой проверки Instagram: «после прокрутки нет мгновенного воспроизведения»).
//
// Порядок живой страницы: при открытии она качает начало текущего и следующего роликов по кругу, потом
// докачивает текущий. Самые старые записи документа — начала роликов. Потолок, который выкидывал «самую старую
// запись документа», оставлял следующий ролик без начала: страница его второй раз не присылает, окно плеера
// 10 с ждало данных у нуля и вставало посреди ролика.
import assert from "node:assert/strict";
import { test } from "node:test";

import { handshake, startHarness, type BridgeHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { VIDEO_64_6S_MP4, concat } from "./support/media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
/** `HTMLMediaElement.HAVE_ENOUGH_DATA`: следующий ролик ленты загружен заранее. */
const HAVE_ENOUGH_DATA = 4;

const REEL_FILE = VIDEO_64_6S_MP4.bytes();
const INIT_END = VIDEO_64_6S_MP4.initEnd;
const [FIRST_SEGMENT_START, SECOND_SEGMENT_START] = VIDEO_64_6S_MP4.mediaStarts;
/** Заранее загруженное начало ролика: init и первый медиасегмент. */
const PRELOADED_BYTES = REEL_FILE.subarray(0, SECOND_SEGMENT_START);
/**
 * Потолок документа в тесте: влезают начала обоих роликов и ещё один сегмент, а весь докачанный текущий
 * ролик — нет (он целиком весит больше 31 КБ).
 */
const DOCUMENT_CAP_BYTES = 2 * SECOND_SEGMENT_START + 8_000;

/** Ролик ленты: свой `<video>` и свой MediaSource с одним SourceBuffer. */
function addReel(harness: Pick<BridgeHarness, "page">, rect: { left: number; top: number; right: number; bottom: number }) {
  const video = harness.page.addVideo();
  video.rect = rect;
  const mediaSource = harness.page.attachMediaSource(video);
  mediaSource.duration = 6;
  return { video, buffer: mediaSource.addSourceBuffer(VIDEO_MIME) };
}

/** Кусок файла в собственном буфере, как отдельная загрузка страницы. */
function download(start: number, end: number): Uint8Array {
  return REEL_FILE.slice(start, end);
}

/** Ключ кандидата, который страница описала стоящим на паузе (следующий ролик ленты). */
async function keyOfPausedCandidate(isolated: IsolatedSide): Promise<{ document: string; element: number }> {
  const before = isolated.received.length;
  isolated.send({ kind: "describe_candidates", request_id: 1 });
  await isolated.waitFor(before + 1);
  const reply = isolated.received[before];
  assert.ok(reply !== undefined && reply.kind === "candidates");
  const paused = reply.candidates.filter((candidate) => !candidate.playing);
  assert.equal(paused.length, 1, "ровно один ролик стоит на паузе");
  const [candidate] = paused;
  assert.ok(candidate !== undefined);
  return { document: candidate.document, element: candidate.element };
}

test("лента: текущий ролик докачался сверх потолка памяти документа — следующий ролик уходит плееру с самого начала", async () => {
  const harness = startHarness({ memoryBudgetBytes: DOCUMENT_CAP_BYTES });
  const playingReel = addReel(harness, { left: 0, top: 0, right: 640, bottom: 360 });
  const nextReel = addReel(harness, { left: 0, top: 380, right: 640, bottom: 740 });
  // Открытие страницы: init и первый сегмент каждого ролика, по кругу.
  playingReel.buffer.appendBuffer(download(0, INIT_END));
  nextReel.buffer.appendBuffer(download(0, INIT_END));
  playingReel.buffer.appendBuffer(download(FIRST_SEGMENT_START, SECOND_SEGMENT_START));
  nextReel.buffer.appendBuffer(download(FIRST_SEGMENT_START, SECOND_SEGMENT_START));
  playingReel.video.testSetState({ paused: false, currentTime: 0.5 });
  nextReel.video.testSetState({ paused: true, currentTime: 0, readyState: HAVE_ENOUGH_DATA });
  // Текущий ролик играет и докачивается до конца: документ переходит потолок.
  const laterSegments = [...VIDEO_64_6S_MP4.mediaStarts.slice(1), REEL_FILE.length];
  for (let index = 0; index + 1 < laterSegments.length; index += 1) {
    playingReel.buffer.appendBuffer(download(laterSegments[index] ?? 0, laterSegments[index + 1] ?? 0));
  }

  const isolated = handshake(harness.page);
  try {
    // Лента листнулась: фон переводит окно на следующий ролик.
    const next = await keyOfPausedCandidate(isolated);
    isolated.received.length = 0;
    isolated.send({ kind: "begin_handoff", request_id: 2, ...next });
    await isolated.settle();

    const outcome = isolated.received.find((message) => message.kind === "handoff_began");
    assert.deepEqual(outcome, { kind: "handoff_began", request_id: 2, outcome: "started" });
    // Страница стоит на нуле следующего ролика, и плеер получает его байты с нуля: init и первый сегмент.
    const seekTargets = isolated.received.flatMap((message) => (message.kind === "page_seek" ? [message.target_us] : []));
    assert.ok(seekTargets.length > 0 && seekTargets.every((target) => target === 0), `page_seek targets ${JSON.stringify(seekTargets)}`);
    const sentBytes = concat(isolated.received.flatMap((message) => (message.kind === "bytes" ? [message.bytes] : [])));
    assert.deepEqual(sentBytes, PRELOADED_BYTES, "повтор следующего ролика — его init и первый сегмент целиком");
  } finally {
    isolated.close();
  }
});
