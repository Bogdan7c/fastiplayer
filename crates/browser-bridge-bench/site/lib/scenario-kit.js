// Общие куски сценариев: элемент <video>, «сыграть ассет», проверка поддержки кодека.

import { BenchPlayer, loadCatalog, standardRefs } from './bench-player.js';
import { checkPlaying } from './bench-checks.js';
import { UnsupportedError } from './bench-report.js';
import { runHandoffPhase } from './handoff-mode.js';

/** Создаёт видеоэлемент на странице (без жеста: autoplay решает флаг/pref робота). */
export function mountVideo(parent = document.body, id = 'v') {
  const video = document.createElement('video');
  video.id = id;
  video.controls = true;
  video.playsInline = true;
  video.width = 480;
  parent.append(video);
  return video;
}

/** Бросает UnsupportedError, если браузер не умеет mime дорожки (честное «нет»). */
export function requireMimeSupport(catalog, refs) {
  for (const ref of refs) {
    const track = catalog.assets[ref.assetId].tracks.find((entry) => entry.id === ref.trackId);
    if (!MediaSource.isTypeSupported(track.mime)) {
      throw new UnsupportedError(`MediaSource.isTypeSupported(${track.mime}) = false`);
    }
  }
}

/** Мини-плеер на ассете каталога; возвращает { video, catalog, player, refs }. */
export async function startAsset(context, assetId, playerOptions = {}, videoTrackId = null) {
  const catalog = await loadCatalog();
  const refs = standardRefs(catalog, assetId, videoTrackId);
  requireMimeSupport(catalog, refs);
  const video = playerOptions.video ?? mountVideo();
  const player = new BenchPlayer(video, catalog, { log: (line) => context.log(line), ...playerOptions });
  await player.start({ refs });
  return { video, catalog, player, refs };
}

/**
 * Режим передачи: страница отыграла свои проверки, ждёт, пока робот «отдаст вкладку»
 * плееру, и отчитывается фактами под тенью (см. handoff-mode.js).
 */
export async function observeHandoff(context, { video, player, refs, catalog }, hooks = {}, options = {}) {
  const durationS = refs && catalog ? catalog.assets[refs[0].assetId].duration_s : null;
  await runHandoffPhase(context, { video, player, hooks, durationS, ...options });
}

/** Сценарий «сыграть ассет и проверить, что играет» (webm, av1, muxed…). */
export async function playAssetScenario(context, assetId, extraCheck = null) {
  const session = await startAsset(context, assetId);
  await checkPlaying(context, session.video, 'playing');
  if (extraCheck) await extraCheck(session);
  if (context.params.handoff) await observeHandoff(context, session);
  await session.player.destroy();
}

/** Число SourceBuffer у текущего MediaSource последнего поколения. */
export function sourceBufferCount(player) {
  const last = player.mediaSources[player.mediaSources.length - 1];
  return last ? last.sourceBuffers.length : 0;
}
