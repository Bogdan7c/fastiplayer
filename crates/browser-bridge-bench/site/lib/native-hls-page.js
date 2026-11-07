// Общий код страниц «нативный HLS VOD в <video src>» (сессия 10a2).
//
// Страница кладёт адрес плейлиста (.m3u8) прямо в `src`, вопреки ответу canPlayType: экстеншен
// отвечает «HLS не умею», но сайт всё равно играет плейлист, и Chromium справляется сам.
// В режиме передачи экстеншен отдаёт HLS плееру: плейлисты, сегменты и ключ качает ФОН экстеншена
// по дескрипторам, а элемент под тенью ничего не качает (это проверяет судья по журналу сервера).
// Firefox на Linux HLS сам не играет — тогда честное unsupported.

import { UnsupportedError } from './bench-report.js';
import { checkPlaying } from './bench-checks.js';
import { loadCatalog } from './bench-player.js';
import { mountVideo, observeHandoff } from './scenario-kit.js';

/** Тип плейлиста для canPlayType (по нему экстеншен отвечает пустой строкой). */
const HLS_MIME = 'application/vnd.apple.mpegurl';

/**
 * Играет HLS-ролик каталога через `<video src>` и, если нужно, уходит в режим передачи.
 * @param context контекст сценария (`runScenario`)
 * @param assetId идентификатор HLS-ролика в каталоге
 * @param options.playlistAddress функция «ассет -> адрес плейлиста» (по умолчанию `/media/<url>`)
 * @param options.handoffOptions добавки к `runHandoffPhase` (например, пропуск проверок)
 * @param options.hooks действия страницы по команде робота `hook:<имя>` (10a3: проверки эфира)
 * @param options.afterPlaying `async (video) => …`: собственные проверки страницы после «играет»,
 *   до фазы передачи (10a3: эфир, `duration = Infinity`, ход `seekable`)
 */
export async function playNativeHlsAsset(context, assetId, options = {}) {
  const catalog = await loadCatalog();
  const asset = catalog.assets[assetId];
  if (!asset || asset.kind !== 'hls') throw new Error(`catalog has no HLS asset '${assetId}'`);
  const answer = document.createElement('video').canPlayType(HLS_MIME);
  const video = mountVideo();
  video.src = options.playlistAddress ? options.playlistAddress(asset) : `/media/${asset.url}`;
  try {
    await video.play().catch(async (error) => {
      if (error?.name !== 'NotAllowedError') throw error;
      video.muted = true;
      await video.play();
    });
  } catch (error) {
    throw new UnsupportedError(`the browser cannot play HLS natively (${error?.name}); canPlayType answered '${answer}'`);
  }
  context.log(`canPlayType(${HLS_MIME}) answered '${answer}', the page put the playlist into src anyway`);
  await checkPlaying(context, video, 'plays-native-hls');
  if (options.afterPlaying) await options.afterPlaying(video);
  if (context.params.handoff) {
    await observeHandoff(context, { video, player: null }, options.hooks ?? {}, {
      durationS: asset.duration_s,
      ...(options.handoffOptions ?? {}),
    });
  }
}
