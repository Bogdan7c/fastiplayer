// Dedicated Worker с MSE: MediaSource создаётся здесь, на странице уходит только MediaSourceHandle
// (так делают Twitch и подобные). Сообщения:
//   page -> worker: init {tracks, durationS, mediaBase, credentials, bufferAheadS, positionS},
//                   clock {currentTime, paused}, seek {positionS}
//   worker -> page: handle (transfer), info {inWorker, hasHandle}, progress {segments}, log {line}

import { MseEngine } from './mse-engine.js';

/** Период отправки счётчика запросов странице, мс. */
const PROGRESS_PERIOD_MS = 250;

let latestClock = { currentTime: 0, paused: true };
let engine = null;

function postLog(line) {
  self.postMessage({ type: 'log', line });
}

async function handleInit(message) {
  const mediaSource = new MediaSource();
  const opened = new Promise((resolve) => {
    mediaSource.addEventListener('sourceopen', resolve, { once: true });
  });
  // Handle нужно передать (transfer), иначе страница не сможет привязать его к <video>.
  self.postMessage({ type: 'handle', handle: mediaSource.handle }, [mediaSource.handle]);
  self.postMessage({
    type: 'info',
    inWorker: typeof WorkerGlobalScope !== 'undefined' && self instanceof WorkerGlobalScope,
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
  setInterval(() => {
    self.postMessage({ type: 'progress', segments: engine.segmentRequestCount() });
  }, PROGRESS_PERIOD_MS);
}

self.addEventListener('message', (event) => {
  const message = event.data;
  if (message.type === 'init') {
    handleInit(message).catch((error) => postLog(`init failed: ${error}`));
  } else if (message.type === 'clock') {
    latestClock = { currentTime: message.currentTime, paused: message.paused };
  } else if (message.type === 'seek') {
    engine?.seek(message.positionS).catch((error) => postLog(`seek failed: ${error}`));
  }
});
