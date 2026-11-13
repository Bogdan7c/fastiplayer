// Страница `hls-live-ends` (сессия 16d1): родной HLS-эфир в `<video src>`, который кончается.
//
// Эфир делает маршрут стенда `/cdn/hls-live-ends/<прогон>/stream.m3u8` (src/site/hls_live_ends.rs):
// как `hls-live` (скользящее окно сегментов `hls-ts-live`), но после фиксированного числа сегментов
// в плейлист дописывается `#EXT-X-ENDLIST`, окно замирает. Браузер сам делает из эфира запись:
// `duration` из `Infinity` становится конечной (одно `durationchange`), затем `ended`.
//
// Под тенью то же обязана показать тень по сообщению плеера `recording_window` (протокол 1.11).
// Firefox на Linux нативного HLS не играет — честное `unsupported` (`playNativeHlsAsset`).
//
// Команды робота (режим передачи):
//   hook:mark-on-air      под тенью эфир ещё идёт (`duration === Infinity`), точка отсчёта событий
//   hook:expect-recording ждём конечную длину РОВНО с одним `durationchange`, затем `ended`

import { runScenario } from './bench-report.js';
import { sleep } from './bench-checks.js';
import { playNativeHlsAsset } from './native-hls-page.js';
import { LiveEndsObserver } from './live-ends-observer.js';

/** Бюджет страницы в обычном режиме, мс: эфир идёт до `ENDLIST` ~36 с (общий бюджет 25 с мал). */
const PLAIN_BUDGET_MS = 55000;
/** Ожидания конца в режиме передачи (укладываются в ожидание ответа робота, 45 с), мс. */
const HANDOFF_WAITS = { durationWaitMs: 30000, endedWaitMs: 12000 };
/** Ожидания конца в обычном режиме: эфир идёт до `ENDLIST` ~36 с от первого запроса, мс. */
const PLAIN_WAITS = { durationWaitMs: 45000, endedWaitMs: 12000 };
/** Обычный режим: сколько мс смотрим на эфир, прежде чем ждать его конец. */
const PLAIN_ON_AIR_MS = 1500;

/** Страница: нативный HLS-эфир, который заканчивается `ENDLIST`. */
export async function runHlsLiveEnds(scenarioName) {
  await runScenario(scenarioName, (ctx) => {
    let observer = null;
    const hooks = {};
    return playNativeHlsAsset(ctx, 'hls-ts-live', {
      // Эфир строит маршрут стенда; `<прогон>` в пути обнуляет часы эфира для каждого прогона.
      playlistAddress: () => `/cdn/hls-live-ends/${ctx.params.run}/stream.m3u8`,
      afterPlaying: async (video) => {
        observer = new LiveEndsObserver(ctx, video);
        ctx.check('live-duration-is-infinity', video.duration === Infinity, `duration=${String(video.duration)}`);
        if (!ctx.params.handoff) {
          await sleep(PLAIN_ON_AIR_MS);
          observer.markOnAir('on-air-duration-is-infinity');
          await observer.expectRecording({ exactlyOneChange: false, ...PLAIN_WAITS });
          return;
        }
        Object.assign(hooks, {
          'mark-on-air': () => { observer.markOnAir('shadow-on-air-duration-is-infinity'); },
          // Под тенью «одно изменение длины» — требование к тени (экстеншену), а не к браузеру.
          'expect-recording': () => observer.expectRecording({ exactlyOneChange: true, ...HANDOFF_WAITS }),
        });
      },
      hooks,
      handoffOptions: {
        durationS: null,
        // Эфир кончается: ход часов и рост буфера «до/после» теряют смысл (часы встают на конце записи).
        skipFinishChecks: ['page-clock-advances', 'page-buffer-keeps-filling'],
      },
    });
  }, { budgetMs: PLAIN_BUDGET_MS });
}
