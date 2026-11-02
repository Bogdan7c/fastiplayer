// Общие проверки «видео реально играет» и ожидалки. Без привязки к конкретному плееру.

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Ждёт `predicate()` до `timeoutMs`; возвращает true/false (исключений по таймауту нет). */
export async function waitFor(predicate, timeoutMs, pollMs = 50) {
  const deadline = performance.now() + timeoutMs;
  while (performance.now() < deadline) {
    if (predicate()) return true;
    await sleep(pollMs);
  }
  return Boolean(predicate());
}

/** Сколько кадров показал элемент (то, что видит и плеер браузера, и наш будущий рендер). */
export function totalFrames(video) {
  return video.getVideoPlaybackQuality?.().totalVideoFrames ?? 0;
}

/** Минимальный прирост кадров для «играет». */
const MIN_FRAMES_ADVANCE = 10;
/** Минимальный прирост времени для «играет», с. */
const MIN_TIME_ADVANCE_S = 1;
/** Сколько ждём прироста, мс. */
const PLAYBACK_WAIT_MS = 8000;

/**
 * Проверка «видео играет силами браузера»: растут и кадры, и currentTime.
 * Сравнение — относительно значений на момент вызова.
 */
export async function checkPlaying(context, video, name = 'playing', options = {}) {
  const minFrames = options.minFrames ?? MIN_FRAMES_ADVANCE;
  const minSeconds = options.minSeconds ?? MIN_TIME_ADVANCE_S;
  const framesBefore = totalFrames(video);
  const timeBefore = video.currentTime;
  const advanced = await waitFor(
    () => totalFrames(video) >= framesBefore + minFrames && video.currentTime >= timeBefore + minSeconds,
    options.timeoutMs ?? PLAYBACK_WAIT_MS,
  );
  return context.check(
    name,
    advanced && !video.paused,
    `totalVideoFrames ${framesBefore} -> ${totalFrames(video)}, currentTime ${timeBefore.toFixed(2)} -> ${video.currentTime.toFixed(2)}, paused=${video.paused}, ${video.videoWidth}x${video.videoHeight}`,
  );
}

/** Запускает воспроизведение без жеста; при отказе политики автоплея повторяет с muted. */
export async function startPlayback(context, video) {
  try {
    await video.play();
  } catch (error) {
    if (error?.name !== 'NotAllowedError') throw error;
    context.log(`autoplay blocked (${error.name}), retrying muted`);
    video.muted = true;
    await video.play();
  }
}

/** Скачивает текст/JSON с проверкой статуса. */
export async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`HTTP ${response.status} for ${url}`);
  return response.json();
}
