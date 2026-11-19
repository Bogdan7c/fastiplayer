// Превью другого раздела (сессия 16b3): то, что живые сайты запускают сразу после ухода из ленты
// (Instagram /explore/: тихий ролик-файл; TikTok /explore и Shorts на главной: тихий ролик на MediaSource).
// Окно плеера НЕ должно принимать такое превью за следующий ролик ленты: уход из ленты — честный конец.
//
// Журнал страницы: `leave-preview-playing kind=<file|mse> muted=<bool> visible=<bool>` — превью ИГРАЕТ
// и видно на экране (стенд требует эту строку: без превью строка прошла бы вхолостую).

import { waitUntilPlaying } from './feed-kit.js';

/** Какой ролик каталога служит превью на MSE (ролик 4: его ширина кадра не совпадает с роликами ленты). */
export const PREVIEW_REEL = 4;
/** Ролик-файл для превью без MediaSource (тот же, что играет страница `video-src`). */
const FILE_PREVIEW_ASSET = 'progressive-long-h264-aac';
/** Превью лежит в правом нижнем углу окна и целиком в нём. */
const PREVIEW_STYLE = 'position:fixed;right:16px;bottom:16px;width:320px;height:180px;background:#000;z-index:10';

/** Целиком ли элемент в окне (превью «видно»). */
function isVisible(element) {
  const { left, top, right, bottom, width, height } = element.getBoundingClientRect();
  return width > 0 && height > 0 && left >= 0 && top >= 0 && right <= window.innerWidth && bottom <= window.innerHeight;
}

/** Ждёт, пока превью заиграет, и пишет в журнал, каким оно получилось. */
async function announcePreview(context, video, kind) {
  const playing = await waitUntilPlaying(video);
  context.log(playing
    ? `leave-preview-playing kind=${kind} muted=${video.muted} visible=${isVisible(video)}`
    : `leave-preview-failed kind=${kind}`);
}

/** Тихое превью `<video src=файл>` (Instagram /explore/). */
export async function playFilePreview(context, catalog) {
  const asset = catalog.assets[FILE_PREVIEW_ASSET];
  const video = document.createElement('video');
  video.muted = true;
  video.playsInline = true;
  video.style.cssText = PREVIEW_STYLE;
  video.src = `/media/${asset.url}`;
  document.body.append(video);
  await video.play();
  await announcePreview(context, video, 'file');
}

/** Тихое превью на MediaSource: на готовом элементе `video` с загруженным плеером или на новом. */
export async function playMsePreview(context, source, entry = null) {
  const preview = entry ?? await source.prepare(PREVIEW_REEL, { muted: true });
  preview.video.muted = true;
  preview.video.style.cssText = PREVIEW_STYLE;
  if (entry !== null) {
    await entry.player.newGeneration({ refs: source.refsOf(PREVIEW_REEL), positionS: 0, autoplay: false });
  }
  document.body.append(preview.video);
  await preview.video.play();
  await announcePreview(context, preview.video, 'mse');
}
