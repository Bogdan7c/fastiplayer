// Плеер стенда внутри Dedicated Worker: MediaSource создаётся здесь, странице уходит только
// MediaSourceHandle (так делают Twitch и подобные). Общий для модульного (`bench-worker.js`) и
// классического (`bench-worker-classic.js`, как у Twitch) воркеров.
// Сообщения:
//   page -> worker: init {tracks, durationS, mediaBase, credentials, bufferAheadS, positionS},
//                   new_generation {то же, что init} — новый MediaSource вместо старого (реклама,
//                   следующая серия), clock {currentTime, paused}, seek {positionS},
//                   unload — «выгрузка» эфира, как у Twitch на паузе (буфер удалён, подкачка стоит),
//                   noise {seq} — пустое сообщение (атака `attack-worker-flood`, воркер его игнорирует),
//                   flood {durationMs, batch} — воркер сам сыплет `noise` странице (атака)
//   worker -> page: handle (transfer) — по одному на поколение, info {inWorker},
//                   progress {segments} (все поколения), log {line}, noise {seq} (только при flood)

import { MseEngine } from './mse-engine.js';

/** Период отправки счётчика запросов странице, мс. */
const PROGRESS_PERIOD_MS = 250;
/** Пауза между пачками сообщений при атаке `flood`, мс (даёт воркеру обработать входящие). */
const FLOOD_BATCH_PAUSE_MS = 1;

/**
 * Создаёт плеер воркера над глобалом `scope`. Возвращает обработчик сообщений страницы — его
 * вызывает файл воркера (у модульного и классического воркеров разный способ загрузки).
 */
export function createWorkerPlayer(scope) {
  let latestClock = { currentTime: 0, paused: true };
  let engine = null;
  /** Сегментные запросы прошлых поколений (счётчик для страницы — сквозной). */
  let retiredSegments = 0;
  let progressTimer = null;
  let generationChain = Promise.resolve();

  const postLog = (line) => scope.postMessage({ type: 'log', line });

  /**
   * Атака «шквал сообщений»: пачками по `batch` штук шлёт странице `noise` в течение
   * `durationMs`, потом докладывает итог в журнал. Воркер при этом продолжает играть.
   */
  async function floodThePage({ durationMs, batch }) {
    const deadline = performance.now() + durationMs;
    let sent = 0;
    while (performance.now() < deadline) {
      for (let index = 0; index < batch; index += 1) {
        scope.postMessage({ type: 'noise', seq: sent });
        sent += 1;
      }
      await new Promise((resolve) => setTimeout(resolve, FLOOD_BATCH_PAUSE_MS));
    }
    postLog(`flood finished: ${sent} noise messages sent to the page`);
  }

  /** Новое поколение: остановить старый движок, создать MediaSource, отдать handle странице. */
  async function startGeneration(message) {
    if (engine) {
      await engine.stop();
      retiredSegments += engine.segmentRequestCount();
      engine = null;
    }
    const mediaSource = new MediaSource();
    const opened = new Promise((resolve) => {
      mediaSource.addEventListener('sourceopen', resolve, { once: true });
    });
    // Handle нужно передать (transfer), иначе страница не сможет привязать его к <video>.
    scope.postMessage({ type: 'handle', handle: mediaSource.handle }, [mediaSource.handle]);
    scope.postMessage({
      type: 'info',
      inWorker: typeof WorkerGlobalScope !== 'undefined' && scope instanceof WorkerGlobalScope,
    });
    await opened;
    engine = new MseEngine({
      mediaSource,
      tracks: message.tracks,
      durationS: message.durationS,
      mediaBase: message.mediaBase,
      credentials: message.credentials,
      bufferAheadS: message.bufferAheadS,
      clock: () => latestClock,
      log: postLog,
    });
    engine.start(message.positionS);
    if (progressTimer === null) {
      progressTimer = setInterval(() => {
        const current = engine ? engine.segmentRequestCount() : 0;
        scope.postMessage({ type: 'progress', segments: retiredSegments + current });
      }, PROGRESS_PERIOD_MS);
    }
  }

  return function handle(message) {
    if (message.type === 'init' || message.type === 'new_generation') {
      // Поколения строго по очереди: новое не начинается, пока старое не остановлено.
      generationChain = generationChain
        .then(() => startGeneration(message))
        .catch((error) => postLog(`${message.type} failed: ${error}`));
    } else if (message.type === 'clock') {
      latestClock = { currentTime: message.currentTime, paused: message.paused };
    } else if (message.type === 'unload') {
      // По очереди с поколениями: выгрузка не должна задеть MediaSource, который ещё создаётся.
      generationChain = generationChain
        .then(() => engine?.unload())
        .catch((error) => postLog(`unload failed: ${error}`));
    } else if (message.type === 'seek') {
      engine?.seek(message.positionS).catch((error) => postLog(`seek failed: ${error}`));
    } else if (message.type === 'flood') {
      floodThePage(message).catch((error) => postLog(`flood failed: ${error}`));
    }
    // `noise` от страницы — намеренный мусор атаки: воркер его читает и выбрасывает.
  };
}
