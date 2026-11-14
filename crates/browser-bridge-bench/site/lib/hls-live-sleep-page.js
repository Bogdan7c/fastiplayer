// Страница `hls-live-sleep` (сессия 16d4): родной HLS-эфир в `<video src>`, сервер которого «засыпает».
//
// Эфир делает маршрут стенда `/cdn/hls-live-sleep/<прогон>/stream.m3u8` (src/site/hls_live_sleep.rs):
// как `hls-live` (скользящее окно сегментов `hls-ts-live`), но через 30 с после первого запроса
// плейлиста сервер на 20 с отвечает на плейлист 503, а окно эфира всё это время едет дальше. После
// «пробуждения» в окне нет ни одного прежнего сегмента — так выглядит эфир для проспавшей машины.
//
// Под тенью страница ничего не делает сама: всё доказывают журналы (сервер — 503 и сегменты нового
// окна у фона экстеншена, плеер — ожидание сети и склейка окна). От страницы требуются только общие
// проверки режима передачи: страница жива под тенью, её часы идут, буфер растёт, паузы нет.
// Окно этих проверок — от передачи до конца прогона, то есть «сон» лежит внутри, а плеер обязан
// после него продолжить. Поэтому проверки не отключаются, в отличие от `hls-live-ends`.
//
// Firefox на Linux нативного HLS не играет — честное `unsupported` (`playNativeHlsAsset`).

import { runScenario } from './bench-report.js';
import { playNativeHlsAsset } from './native-hls-page.js';

/** Страница: нативный HLS-эфир с «засыпающим» сервером. */
export async function runHlsLiveSleep(scenarioName) {
  await runScenario(scenarioName, (ctx) => playNativeHlsAsset(ctx, 'hls-ts-live', {
    // Эфир строит маршрут стенда; `<прогон>` в пути обнуляет часы эфира (и расписание сна) для
    // каждого прогона.
    playlistAddress: () => `/cdn/hls-live-sleep/${ctx.params.run}/stream.m3u8`,
    afterPlaying: async (video) => {
      ctx.check('live-duration-is-infinity', video.duration === Infinity, `duration=${String(video.duration)}`);
    },
    hooks: {},
    // Длины у эфира нет: «близко к концу» не наступает никогда.
    handoffOptions: { durationS: null },
  }));
}
