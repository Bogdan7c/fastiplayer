// Общие куски страниц входов 10c (правый клик, ссылка): играет файлы, зонд для робота, ожидание конца.

import { BenchPlayer, loadCatalog, standardRefs } from './bench-player.js';
import { mountVideo, requireMimeSupport } from './scenario-kit.js';
import { checkPlaying } from './bench-checks.js';
import { installEntryProbe } from './entry-probe.js';
import { CommandChannel } from './handoff-mode.js';

/** Файл каталога как адрес `<video src>`. */
export async function progressiveUrl(assetId) {
  const catalog = await loadCatalog();
  return `/media/${catalog.assets[assetId].url}`;
}

/**
 * Видео из файла каталога: создаётся, запускается (при запрете автозапуска без звука) и проверяется
 * «играет». `parent` — куда вставить, `id` — имя элемента (оно же имя цели правого клика `video:<id>`).
 */
export async function playFileVideo(ctx, { assetId, id, parent = document.body, width = 360 }) {
  const video = document.createElement('video');
  video.id = id;
  video.controls = true;
  video.playsInline = true;
  video.width = width;
  video.src = await progressiveUrl(assetId);
  parent.append(video);
  await video.play().catch(async () => { video.muted = true; await video.play(); });
  await checkPlaying(ctx, video, `${id}-playing`);
  return video;
}

/**
 * Видео потоком MSE (мини-плеер стенда), как у большинства живых сайтов: адрес `blob:`, а не файл.
 * Строки позднего входа (10d) берут именно его: Firefox вставляет наш скрипт в уже открытую вкладку
 * позже начала потока, и перезагрузка нужна только для потока, а файл он отдаёт и так.
 * `autoplay: false` оставляет видео стоять, пока человек не нажмёт кнопку страницы.
 * `prepare(video)` вызывается ДО запуска потока: слушатели, которые не должны опоздать к первой перемотке.
 */
export async function mountMseVideo(ctx, { id, assetId = 'h264-aac', autoplay = true, parent = document.body, width = 360, prepare = null }) {
  const catalog = await loadCatalog();
  const refs = standardRefs(catalog, assetId);
  requireMimeSupport(catalog, refs);
  const video = mountVideo(parent, id);
  video.width = width;
  prepare?.(video);
  const player = new BenchPlayer(video, catalog, { log: (line) => ctx.log(line) });
  await player.start({ refs, autoplay });
  if (autoplay) await checkPlaying(ctx, video, `${id}-playing`);
  return { video, player };
}

/**
 * Страница сыграла и ждёт робота: зонд входов включён, готовность объявлена, дальше страница
 * выполняет команды `hook:<имя>` (действия зрителя: нажать play) и по `finish` отчитывается.
 * Кадры под тенью здесь не судим: судья входов — журнал плеера и сервера.
 */
export async function holdForRobot(ctx, hooks = {}) {
  // Обычный режим (команда `pages`): страница только играла и проверила «играет», ждать некого.
  if (!ctx.params.handoff) return;
  installEntryProbe(ctx);
  ctx.log('handoff-ready');
  const commands = new CommandChannel(ctx);
  for (;;) {
    const command = await commands.next();
    if (command === 'finish') {
      ctx.log('ack finish');
      return;
    }
    if (command.startsWith('hook:')) await hooks[command.slice('hook:'.length)]?.();
    ctx.log(`ack ${command}`);
  }
}
