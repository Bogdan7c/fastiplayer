// Страница `mse-live-ends` (сессия 16d1): MSE-эфир, который посреди игры становится записью.
//
// Как делают hls.js / Shaka / dash.js в конце прямого эфира:
//   1. эфир открывается как живой: `MediaSource.duration = Infinity`, данные докладываются;
//   2. в конце эфира плеер сайта ставит КОНЕЧНУЮ длину и зовёт `endOfStream()`.
// Данные — обычный короткий fMP4 стенда (`h264-aac`); конец эфира ставит страница, а не конец данных
// (`BenchPlayer({broadcastLive: true})`, `endLiveBroadcast()`).
//
// Обычный режим (без экстеншена): страница сама играет несколько секунд, заканчивает эфир и ждёт
// `ended`. Режим передачи: страница под командой робота (он знает, когда плеер уже показал кадры):
//   hook:mark-on-air      под тенью эфир ещё идёт (`duration === Infinity`)
//   hook:end-live         конечная длина + endOfStream()
//   hook:expect-recording длина конечна, элемент дошёл до `ended`

import { runScenario } from './bench-report.js';
import { checkPlaying, sleep } from './bench-checks.js';
import { startAsset, observeHandoff } from './scenario-kit.js';
import { LiveEndsObserver } from './live-ends-observer.js';

/** Буфер наперёд, с: короткий, чтобы после конца эфира осталось доиграть недолго. */
const BUFFER_AHEAD_S = 4;
/** Ожидания конца: длина ставится самой страницей (быстро), остаток буфера доигрывается дольше, мс. */
const WAITS = { durationWaitMs: 15000, endedWaitMs: 25000 };
/** Обычный режим: сколько секунд эфир идёт до своего конца, мс. */
const PLAIN_ON_AIR_MS = 3000;

/** Страница: эфир MSE, который заканчивается. */
export async function runMseLiveEnds(scenarioName) {
  await runScenario(scenarioName, async (ctx) => {
    const session = await startAsset(ctx, 'h264-aac', { bufferAheadS: BUFFER_AHEAD_S, broadcastLive: true });
    const { video, player } = session;
    const observer = new LiveEndsObserver(ctx, video);
    await checkPlaying(ctx, video, 'playing');
    ctx.check('live-duration-is-infinity', video.duration === Infinity, `duration=${String(video.duration)}`);
    if (!ctx.params.handoff) {
      await sleep(PLAIN_ON_AIR_MS);
      observer.markOnAir('on-air-duration-is-infinity');
      await player.endLiveBroadcast();
      await observer.expectRecording({ exactlyOneChange: false, ...WAITS });
      await player.destroy();
      return;
    }
    await observeHandoff(ctx, session, {
      'mark-on-air': () => { observer.markOnAir('shadow-on-air-duration-is-infinity'); },
      'end-live': async () => {
        const durationS = await player.endLiveBroadcast();
        ctx.log(`the page ended the live stream at ${durationS.toFixed(2)} s`);
      },
      'expect-recording': () => observer.expectRecording({ exactlyOneChange: false, ...WAITS }),
    }, {
      // После конца эфира буфер страницы не растёт: «докачивается» нечего.
      skipFinishChecks: ['page-buffer-keeps-filling'],
    });
    await player.destroy();
  });
}
