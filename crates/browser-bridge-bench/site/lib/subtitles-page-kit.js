// Общие куски страниц `subtitles-*` (сессия 13b2): MSE-страница с мини-плеером и субтитрами,
// которые страница берёт САМА (fetch, XHR, addTextTrack), как это делают настоящие плееры сайтов.

import { checkPlaying, waitFor } from './bench-checks.js';
import { observeHandoff, startAsset } from './scenario-kit.js';

/** Ассет MSE-страниц: 30 с, достаточно, чтобы плеер показал кадры со слоем субтитров. */
export const SUBTITLES_MSE_ASSET = 'h264-aac';
/** Адрес файла субтитров стенда (статика `site/subtitles/`) на origin `origin`. */
export const subtitleUrl = (origin, fileName) => `${origin}/site/subtitles/${fileName}`;

/**
 * Страница: мини-плеер играет ассет, `loadSubtitles(ctx, session)` берёт субтитры так, как это
 * делает конкретная страница; в режиме передачи после этого страница уходит под тень.
 * `hooks` — команды робота `hook:<имя>` (нужны только `subtitles-text-track`).
 */
export async function runMseSubtitlesPage(context, { loadSubtitles, hooks = {} }) {
  const session = await startAsset(context, SUBTITLES_MSE_ASSET);
  await checkPlaying(context, session.video, 'playing');
  await loadSubtitles(context, session);
  if (context.params.handoff) {
    await observeHandoff(context, session, hooks);
  }
  await session.player.destroy();
}

/** Сказать в журнале страницы и проверить условие (одна строка вместо двух). */
export async function waitForCondition(context, name, predicate, detail, timeoutMs = 5000) {
  const ok = await waitFor(predicate, timeoutMs);
  context.check(name, ok, typeof detail === 'function' ? detail() : detail);
  return ok;
}
