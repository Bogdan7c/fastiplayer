// Общее тело страниц «смена поколения»: реклама (VP9) → фильм (H.264) новым MediaSource на
// ТОМ ЖЕ <video>. Варианты страниц отличаются только тем, КАК страница снимает старый
// источник (`releaseOldSource`) и ЗАДЕРЖИВАЕТ ли данные нового поколения
// (`firstRequestDelayMs`): так ведут себя настоящие плееры (YouTube перед рекламой делает
// `srcObject = null`; медленный CDN отдаёт первый сегмент через секунды).
//
// Проверки одни и те же у всех вариантов (как у `mse-generation-swap`), плюс две для задержки:
// пока данных нет, часы страницы стоят у начала нового ролика и `ended` не приходит.

import { runScenario } from './bench-report.js';
import { checkPlaying, waitFor, sleep } from './bench-checks.js';
import { BenchPlayer, loadCatalog, standardRefs, RELEASE_OLD_SOURCE } from './bench-player.js';
import { mountVideo, requireMimeSupport, observeHandoff } from './scenario-kit.js';

/** Сколько секунд «рекламного» потока играем до замены, с. */
const AD_SECONDS = 5;
/** Период чтения часов страницы, пока данных нового поколения нет, мс. */
const WAITING_SAMPLE_PERIOD_MS = 250;
/** Часы страницы пока ждёт данные не должны уйти дальше этого, с (старт нового ролика ≈ 0). */
const WAITING_CLOCK_MAX_S = 1;
/** Сколько проб часов минимум обязано набраться за задержку: иначе проверка пуста. */
const MIN_WAITING_SAMPLES = 8;

/** Начинает смену поколения и, пока данных нет, следит за часами страницы и `ended`. */
async function swapWatchingTheGap(video, player, movieRefs, options) {
  const waiting = { clockSamples: [], ended: false };
  const onEnded = () => { waiting.ended = true; };
  video.addEventListener('ended', onEnded);
  // `newGeneration` возвращается только когда `play()` дождался данных, поэтому её нельзя
  // ждать до наблюдения: иначе окно задержки пройдёт мимо страницы.
  let swapped = false;
  const swap = player.newGeneration({ refs: movieRefs, positionS: 0, ...options })
    .finally(() => { swapped = true; });
  const generationsBefore = player.generations;
  while (!swapped) {
    // Пока новый MediaSource ещё не привязан к элементу, часы — это часы СТАРОГО ролика;
    // в окно ожидания данных нового они не входят.
    if (player.generations > generationsBefore) waiting.clockSamples.push(video.currentTime);
    await sleep(WAITING_SAMPLE_PERIOD_MS);
  }
  await swap;
  video.removeEventListener('ended', onEnded);
  return waiting;
}

/** Проверки окна ожидания данных (только для вариантов с задержкой). */
function checkWaitingGap(ctx, waiting, delayMs) {
  const worst = Math.max(...waiting.clockSamples, 0);
  ctx.check('waiting-window-was-observed', waiting.clockSamples.length >= MIN_WAITING_SAMPLES,
    `${waiting.clockSamples.length} clock samples over a ${delayMs} ms data delay`);
  ctx.check('clock-stays-at-new-start-while-waiting', waiting.clockSamples.length > 0 && worst <= WAITING_CLOCK_MAX_S,
    `worst page clock while waiting for data ${worst.toFixed(2)} s (limit ${WAITING_CLOCK_MAX_S} s)`);
  ctx.check('no-ended-while-waiting', !waiting.ended, `ended fired while waiting: ${waiting.ended}`);
}

/**
 * @param {string} scenarioName имя страницы (= имя файла без `.html`)
 * @param {{releaseOldSource?: string, firstRequestDelayMs?: number}} variant
 */
export async function runGenerationSwapPage(scenarioName, variant = {}) {
  const releaseOldSource = variant.releaseOldSource ?? RELEASE_OLD_SOURCE.SRC;
  const firstRequestDelayMs = variant.firstRequestDelayMs ?? 0;
  const swapOptions = { releaseOldSource, firstRequestDelayMs };
  await runScenario(scenarioName, async (ctx) => {
    const catalog = await loadCatalog();
    const adRefs = standardRefs(catalog, 'vp9-opus');
    const movieRefs = standardRefs(catalog, 'h264-aac', 'video-360');
    requireMimeSupport(catalog, adRefs);
    const video = mountVideo();
    const player = new BenchPlayer(video, catalog, { log: (line) => ctx.log(line) });
    await player.start({ refs: adRefs });
    await checkPlaying(ctx, video, 'ad-stream-playing');

    /** Смена поколения вариантом страницы; возвращает окно ожидания, если данные задержаны. */
    const swapGeneration = async () => {
      if (firstRequestDelayMs > 0) return swapWatchingTheGap(video, player, movieRefs, swapOptions);
      await player.newGeneration({ refs: movieRefs, positionS: 0, ...swapOptions });
      return null;
    };

    if (ctx.params.handoff) {
      // Реклама отдана плееру; смену MediaSource на том же элементе страница делает ПОД ТЕНЬЮ.
      await observeHandoff(ctx, { video, player, refs: adRefs, catalog }, {
        'swap-generation': async (hookCtx) => {
          const waiting = await swapGeneration();
          if (waiting) checkWaitingGap(hookCtx, waiting, firstRequestDelayMs);
          const advanced = await waitFor(() => video.currentTime > 1 && video.currentTime < 8, 10000);
          hookCtx.check('second-media-source-under-shadow', player.generations === 2,
            `generations=${player.generations}, currentTime=${video.currentTime.toFixed(2)}, release=${releaseOldSource}`);
          hookCtx.check('movie-clock-runs-after-swap', advanced, `currentTime=${video.currentTime.toFixed(2)}`);
        },
      }, { expectedExtraGenerations: 1 });
      await player.destroy();
      return;
    }

    await waitFor(() => video.currentTime >= AD_SECONDS, 8000);
    const adEnd = video.currentTime;
    const waiting = await swapGeneration();
    if (waiting) checkWaitingGap(ctx, waiting, firstRequestDelayMs);
    ctx.check('second-media-source-same-element', player.generations === 2 && player.mediaSources.length === 2,
      `generations=${player.generations}, release=${releaseOldSource}, ad stopped at ${adEnd.toFixed(2)} s, element reused=${document.querySelectorAll('video').length === 1}`);
    await checkPlaying(ctx, video, 'movie-playing-after-swap');
    ctx.check('movie-restarted-from-zero', video.currentTime < adEnd + 1 && video.currentTime > 0, `currentTime=${video.currentTime.toFixed(2)}`);
    await player.destroy();
  });
}
