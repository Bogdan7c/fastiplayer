// Страницы «своя шкала времени» (сессия 11a): поток MSE, у которого секунды страницы НЕ совпадают с
// секундами самого ролика. Так делают настоящие сайты:
//   * эфир или длинная запись, открытая с середины: буфер начинается далеко от нуля
//     (`timestampOffset` у всего ролика, страница стоит на 600-й секунде);
//   * реклама, вшитая в тот же поток перед фильмом: первые секунды шкалы — чужие кадры, фильм идёт
//     со сдвигом на длину рекламы.
// Плеер обязан вернуть видео странице на ЕЁ секунде (решение 236: позиция тени — шкала страницы).
//
// Как льётся поток. Ролик стенда короткий (30 с), поэтому страница кладёт всю шкалу сразу: на каждую
// дорожку — init, затем куски шкалы по порядку, перед каждым куском `timestampOffset`. Куски нарезаны
// по границам сегментов, поэтому окно добавления (`appendWindow*`) не нужно. Своей подкачки у страницы
// нет: всё уже в буфере, и сравнение «где страница» не зависит от сети.

import { pageParams, runScenario } from './bench-report.js';
import { checkPlaying, startPlayback, waitFor } from './bench-checks.js';
import { loadCatalog, resolveRef, standardRefs } from './bench-player.js';
import { mountVideo, requireMimeSupport } from './scenario-kit.js';
import { runHandoffPhase } from './handoff-mode.js';

/** Сколько ждём `sourceopen`, мс. */
const SOURCE_OPEN_TIMEOUT_MS = 5000;
/** Допуск совпадения границ сегмента с границей куска, с. */
const SEGMENT_BOUNDARY_EPS_S = 0.05;
/** Бюджет страницы без робота, мс: проверка «реклама сменилась фильмом» ждёт конца рекламы. */
const PLAIN_PAGE_BUDGET_MS = 40000;
/** Сколько ждём, пока часы страницы дойдут до нужной секунды без робота, мс. */
const PLAIN_REACH_WAIT_MS = 15000;

/** Открывает MediaSource на элементе. */
async function openMediaSource(video) {
  const mediaSource = new MediaSource();
  video.src = URL.createObjectURL(mediaSource);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('sourceopen timeout')), SOURCE_OPEN_TIMEOUT_MS);
    mediaSource.addEventListener('sourceopen', () => { clearTimeout(timer); resolve(); }, { once: true });
  });
  return mediaSource;
}

/** Добавляет байты и ждёт `updateend` (ошибка — исключение, а не тишина). */
function appendAndWait(sourceBuffer, bytes) {
  return new Promise((resolve, reject) => {
    const onEnd = () => { cleanup(); resolve(); };
    const onError = () => { cleanup(); reject(new Error('SourceBuffer error event')); };
    const cleanup = () => {
      sourceBuffer.removeEventListener('updateend', onEnd);
      sourceBuffer.removeEventListener('error', onError);
    };
    sourceBuffer.addEventListener('updateend', onEnd);
    sourceBuffer.addEventListener('error', onError);
    sourceBuffer.appendBuffer(bytes);
  });
}

async function fetchBytes(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`HTTP ${response.status} for ${url}`);
  return response.arrayBuffer();
}

/** Сегменты дорожки, лежащие в куске ролика [fromS, toS). */
function segmentsOfPiece(track, piece) {
  return track.segments.filter((segment) => segment.start_s >= piece.fromS - SEGMENT_BOUNDARY_EPS_S
    && segment.start_s < piece.toS - SEGMENT_BOUNDARY_EPS_S);
}

/**
 * Кладёт всю шкалу: `pieces` — куски ролика `{ fromS, toS, offsetS }` (секунды ролика и сдвиг на шкале
 * страницы). Возвращает границы шкалы страницы.
 */
async function feedTimeline(context, video, catalog, assetId, pieces) {
  const refs = standardRefs(catalog, assetId);
  requireMimeSupport(catalog, refs);
  const mediaSource = await openMediaSource(video);
  const timelineStartS = Math.min(...pieces.map((piece) => piece.fromS + piece.offsetS));
  const timelineEndS = Math.max(...pieces.map((piece) => piece.toS + piece.offsetS));
  mediaSource.duration = timelineEndS;
  const base = new URL('/media/', location.href);
  // Дорожки льются параллельно (как у настоящих плееров), куски одной дорожки — строго по порядку.
  await Promise.all(refs.map(async (ref) => {
    const track = resolveRef(catalog, ref);
    const sourceBuffer = mediaSource.addSourceBuffer(track.mime);
    await appendAndWait(sourceBuffer, await fetchBytes(new URL(track.init, base)));
    for (const piece of pieces) {
      sourceBuffer.timestampOffset = piece.offsetS;
      for (const segment of segmentsOfPiece(track, piece)) {
        await appendAndWait(sourceBuffer, await fetchBytes(new URL(segment.url, base)));
      }
    }
  }));
  mediaSource.endOfStream();
  context.log(`timeline fed ${timelineStartS.toFixed(2)}..${timelineEndS.toFixed(2)} s in ${pieces.length} piece(s)`);
  return { timelineStartS, timelineEndS };
}

/**
 * Страница со своей шкалой. `layout` — `{ assetId, pieces, plainCheck }`: что лить и что страница
 * проверяет сама без робота (`plainCheck(context, video, timeline)`).
 */
export async function runTimelinePage(scenarioName, layout) {
  await runScenario(scenarioName, async (context) => {
    const catalog = await loadCatalog();
    const video = mountVideo();
    const timeline = await feedTimeline(context, video, catalog, layout.assetId, layout.pieces);
    // Буфер начинается там, где начинается шкала страницы (а не с нуля).
    video.currentTime = timeline.timelineStartS;
    await startPlayback(context, video);
    await checkPlaying(context, video, 'playing');
    context.check('plays-on-the-page-timeline', video.currentTime >= timeline.timelineStartS,
      `currentTime=${video.currentTime.toFixed(2)}, timeline ${timeline.timelineStartS}..${timeline.timelineEndS}`);
    if (context.params.handoff) {
      // Своей подкачки нет: `player` пуст, лишних MediaSource быть не может, весь буфер уже лежит.
      await runHandoffPhase(context, { video, durationS: timeline.timelineEndS });
      return;
    }
    await layout.plainCheck(context, video, timeline);
  }, { budgetMs: pageParams().handoff ? undefined : PLAIN_PAGE_BUDGET_MS });
}

/** Без робота: часы страницы доходят до секунды `targetS` шкалы и идут дальше. */
export async function checkReaches(context, video, name, targetS) {
  const reached = await waitFor(() => video.currentTime >= targetS, PLAIN_REACH_WAIT_MS);
  context.check(name, reached && !video.paused, `currentTime=${video.currentTime.toFixed(2)}, target ${targetS} s`);
}
