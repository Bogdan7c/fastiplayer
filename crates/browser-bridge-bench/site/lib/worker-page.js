// Общее тело страниц «MSE в Dedicated Worker» (`mse-worker` — модульный воркер,
// `mse-worker-classic` — классический, как у Twitch). MediaSource живёт в воркере, на <video>
// уходит MediaSourceHandle (`srcObject`).
//
// Воркер плеера создаётся из blob-адреса страницы (как Twitch/IVS и Vite, bench-player.js); рядом —
// проба: воркер из http-адреса сайта экстеншен не оборачивает (решение 192).
// Обычный режим: играет, MediaSource в воркере, воркер качает сегменты; со сменой поколения —
// ещё и новый MediaSource того же воркера на том же элементе играет.
// Режим передачи: после своих проверок страница отдаётся роботу; хук `swap-generation` (если
// включён) делает новый MediaSource в воркере уже ПОД ТЕНЬЮ: `srcObject = null`, затем новый handle
// (как YouTube/Twitch при смене потока) — плеер обязан показать его новым поколением.

import { runScenario, UnsupportedError } from './bench-report.js';
import { checkPlaying, waitFor } from './bench-checks.js';
import { BenchPlayer, loadCatalog, standardRefs, RELEASE_OLD_SOURCE } from './bench-player.js';
import { mountVideo, observeHandoff } from './scenario-kit.js';

/** Буфер наперёд в режиме передачи, с (как у mse-fmp4: место для перемотки за буфер). */
const HANDOFF_BUFFER_AHEAD_S = 4;
/** Сколько ждём хода часов нового поколения, мс. */
const NEW_GENERATION_CLOCK_WAIT_MS = 10000;
/** Сколько ждём доклада пробы http-воркера, мс. */
const PROBE_WORKER_TIMEOUT_MS = 5000;

/**
 * Решение 192: воркер из http(s)-адреса того же сайта экстеншен не трогает — внутри него
 * `self.location` остаётся http-адресом, относительный `fetch` работает. В `pages` (без экстеншена)
 * проверка проходит сама собой; в `capture` (с экстеншеном) она ловит лишнюю обёртку.
 */
async function checkSameOriginWorkerUntouched(ctx) {
  const probeUrl = new URL('./same-origin-probe-worker.js', import.meta.url).href;
  const probe = new Worker(probeUrl);
  const report = await new Promise((resolve) => {
    const timer = setTimeout(() => resolve(null), PROBE_WORKER_TIMEOUT_MS);
    probe.addEventListener('message', (event) => { clearTimeout(timer); resolve(event.data); });
    probe.addEventListener('error', (event) => { clearTimeout(timer); resolve({ href: '', fetchOk: false, detail: `error: ${event.message}` }); });
  });
  probe.terminate();
  ctx.check('http-worker-not-wrapped', report !== null && report.href === probeUrl && report.fetchOk === true,
    report === null ? 'probe worker did not answer' : `location=${report.href}, relative fetch ok=${report.fetchOk} (${report.detail})`);
}

/**
 * @param {string} scenarioName имя страницы (= имя файла без `.html`)
 * @param {{worker: 'module'|'classic', generationSwap: boolean}} variant
 */
export async function runWorkerPage(scenarioName, variant) {
  await runScenario(scenarioName, async (ctx) => {
    if (!('canConstructInDedicatedWorker' in MediaSource) || !MediaSource.canConstructInDedicatedWorker) {
      throw new UnsupportedError(`MediaSource.canConstructInDedicatedWorker=${MediaSource.canConstructInDedicatedWorker}`);
    }
    await checkSameOriginWorkerUntouched(ctx);
    const catalog = await loadCatalog();
    const refs = standardRefs(catalog, 'h264-aac');
    const movieRefs = standardRefs(catalog, 'h264-aac', 'video-360');
    const video = mountVideo();
    const player = new BenchPlayer(video, catalog, {
      worker: variant.worker,
      log: (line) => ctx.log(line),
      ...(ctx.params.handoff ? { bufferAheadS: HANDOFF_BUFFER_AHEAD_S } : {}),
    });
    await player.start({ refs });
    await checkPlaying(ctx, video, 'playing');
    const info = player.workerInfo;
    ctx.check('media-source-lives-in-worker', info?.inWorker === true && video.srcObject !== null,
      `inWorker=${info?.inWorker}, srcObject=${video.srcObject?.constructor?.name}, worker=${variant.worker}`);
    ctx.check('worker-fetched-segments', player.segmentRequestCount > 0, `segment requests (worker)=${player.segmentRequestCount}`);

    /** Новый MediaSource в воркере на том же элементе, старый источник снят `srcObject = null`. */
    const swapGeneration = async (checkCtx) => {
      await player.newGeneration({ refs: movieRefs, positionS: 0, releaseOldSource: RELEASE_OLD_SOURCE.SRC_OBJECT_NULL });
      const advanced = await waitFor(() => video.currentTime > 1 && video.currentTime < 8, NEW_GENERATION_CLOCK_WAIT_MS);
      checkCtx.check('second-worker-media-source', player.generations === 2,
        `generations=${player.generations}, srcObject=${video.srcObject?.constructor?.name}`);
      checkCtx.check('new-generation-clock-runs', advanced, `currentTime=${video.currentTime.toFixed(2)}`);
    };

    if (ctx.params.handoff) {
      const hooks = variant.generationSwap ? { 'swap-generation': swapGeneration } : {};
      await observeHandoff(ctx, { video, player, refs, catalog }, hooks,
        { expectedExtraGenerations: variant.generationSwap ? 1 : 0 });
      await player.destroy();
      return;
    }
    if (variant.generationSwap) {
      await swapGeneration(ctx);
      await checkPlaying(ctx, video, 'playing-after-worker-swap');
    }
    await player.destroy();
  });
}
