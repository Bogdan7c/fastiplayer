// Страница-повтор записи потока страницы: грузит `*.fprec` с сервера стенда (`/bench/recording`),
// повторяет события на своём <video> и докладывает стенду. Формат файла: crates/browser-bridge-bench/
// src/recording.rs (магия FPREC001, u32 LE длина манифеста, манифест JSON, склад байтов).
//
// Связь со стендом (единственный канал — журнал и отчёт страницы):
//   `replay-started`        — события пошли;
//   `replay-handoff-point`  — наступил `handoff_began` записи: стенд «отдаёт вкладку» плееру;
//   `replay-finished ...`   — события кончились; после запаса (`tail_ms`) уходит отчёт.
// Контексты: `main` — в этой странице; `worker-N` — каждый в своём blob-воркере (MSE в воркере
// есть только в Chromium; в Firefox такая запись — честное unsupported).

import { runScenario, UnsupportedError } from './bench-report.js';
import { mountVideo } from './scenario-kit.js';
import { startPlayback } from './bench-checks.js';
import { runContext } from './replay-engine.js';

const MAGIC = 'FPREC001';
const HEADER_LENGTH = 12;
const MAIN_CONTEXT = 'main';
/** Запас после последнего события по умолчанию, мс (стенд передаёт свой через `tail_ms`). */
const DEFAULT_TAIL_MS = 5000;
/** Пауза перед стартом событий: страница успевает отправить журнал и создать воркеры, мс. */
const START_DELAY_MS = 500;
/** Сколько ждём handle от воркера для `srcObject`, мс. */
const HANDLE_TIMEOUT_MS = 20000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Разбор файла записи: манифест и склад байтов. */
function parseRecording(buffer) {
  const view = new DataView(buffer);
  const magic = new TextDecoder().decode(new Uint8Array(buffer, 0, MAGIC.length));
  if (buffer.byteLength < HEADER_LENGTH || magic !== MAGIC) throw new Error(`not a recording (magic '${magic}')`);
  const manifestLength = view.getUint32(MAGIC.length, true);
  const manifestEnd = HEADER_LENGTH + manifestLength;
  const manifest = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, HEADER_LENGTH, manifestLength)));
  return { manifest, store: buffer.slice(manifestEnd) };
}

/**
 * Раскладывает события по контекстам. Нагрузки `append` каждого контекста собираются в его
 * собственный склад (он уезжает воркеру передачей, без копии всей записи); `offset` переписан.
 * Неизвестные операции стенд уже отсёк, но порядок по t_ms поддерживаем сами (стабильно).
 */
function partitionByContext(manifest, store) {
  const parts = new Map();
  const sorted = manifest.events.map((event, index) => ({ ...event, index }))
    .sort((left, right) => left.t_ms - right.t_ms || left.index - right.index);
  const lastHandleContext = new Map();
  for (const event of sorted) {
    if (!parts.has(event.ctx)) parts.set(event.ctx, { events: [], chunks: [], size: 0 });
    const part = parts.get(event.ctx);
    const copy = { ...event };
    if (event.op === 'append') {
      part.chunks.push(new Uint8Array(store, event.offset, event.length));
      copy.offset = part.size;
      part.size += event.length;
    }
    // Какому воркеру принадлежит `srcObject = handle`: последний `media_source_handle` с этим ms.
    if (event.op === 'media_source_handle') lastHandleContext.set(event.ms, event.ctx);
    if (event.op === 'element_source' && event.kind === 'src_object_handle') {
      copy.handleContext = lastHandleContext.get(event.ms) ?? null;
    }
    part.events.push(copy);
  }
  for (const part of parts.values()) {
    const compact = new Uint8Array(part.size);
    let position = 0;
    for (const chunk of part.chunks) { compact.set(chunk, position); position += chunk.length; }
    part.store = compact.buffer;
    delete part.chunks;
  }
  return parts;
}

/** Главный поток: особые операции (элемент, object URL, handle от воркера, отметка передачи). */
function createMainHost(ctx, handleBox) {
  const videos = new Map();
  const objectUrls = new Map();
  const videoOf = (id) => {
    if (!videos.has(id)) videos.set(id, mountVideo(document.body, `video-${id}`));
    return videos.get(id);
  };
  return {
    videos,
    log: (line) => ctx.log(line),
    special: {
      media_source_object_url(event, state) {
        objectUrls.set(event.ms, URL.createObjectURL(state.mediaSources.get(event.ms)));
      },
      async element_source(event, state) {
        const video = videoOf(event.el);
        switch (event.kind) {
          case 'media_source_url': video.src = objectUrls.get(event.ms); break;
          case 'src_object_media_source': video.srcObject = state.mediaSources.get(event.ms); break;
          case 'src_object_handle': video.srcObject = await handleBox.take(event.handleContext, event.ms); break;
          case 'none': video.srcObject = null; video.removeAttribute('src'); video.load(); break;
          default: throw new Error(`element_source kind '${event.kind}' cannot be replayed (the recording has no source data)`);
        }
      },
      element_play(event) {
        // Не ждём промис: play() не завершится, пока нет данных; отказ автоплея повторяем без звука.
        startPlayback(ctx, videoOf(event.el)).catch((error) => ctx.log(`replay element_play failed: ${error?.message}`));
      },
      element_pause(event) { videoOf(event.el).pause(); },
      element_seek(event) { videoOf(event.el).currentTime = Number(event.value); },
      element_rate(event) { videoOf(event.el).playbackRate = Number(event.value); },
      element_load(event) { videoOf(event.el).load(); },
      handoff_began() { ctx.log('replay-handoff-point'); },
    },
  };
}

/** Копилка handle-ов, пришедших от воркеров: `take` ждёт нужный. */
function createHandleBox() {
  const arrived = new Map();
  const waiters = new Map();
  const key = (contextName, ms) => `${contextName}|${ms}`;
  return {
    deliver(contextName, ms, handle) {
      const slot = key(contextName, ms);
      arrived.set(slot, handle);
      waiters.get(slot)?.(handle);
    },
    take(contextName, ms) {
      if (contextName === null) return Promise.reject(new Error(`no media_source_handle for ${ms} was recorded before the element used it`));
      const slot = key(contextName, ms);
      if (arrived.has(slot)) return Promise.resolve(arrived.get(slot));
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`worker ${contextName} never sent the handle of ${ms}`)), HANDLE_TIMEOUT_MS);
        waiters.set(slot, (handle) => { clearTimeout(timer); resolve(handle); });
      });
    },
  };
}

/** Классический blob-воркер, как у Twitch; возвращает { worker, finished } (finished — итог движка). */
function startWorkerContext(ctx, contextName, part, t0Epoch, handleBox) {
  const workerScript = new URL('./replay-worker.js', import.meta.url).href;
  const coreUrl = new URL('./replay-engine.js', import.meta.url).href;
  const loader = `importScripts(${JSON.stringify(workerScript)});\n`;
  const blobUrl = URL.createObjectURL(new Blob([loader], { type: 'text/javascript' }));
  const worker = new Worker(blobUrl);
  const finished = new Promise((resolve, reject) => {
    worker.addEventListener('message', (message) => {
      const data = message.data;
      if (data.type === 'handle') handleBox.deliver(contextName, data.ms, data.handle);
      else if (data.type === 'log') ctx.log(`${contextName}: ${data.line}`);
      else if (data.type === 'done') resolve(data.result);
      else if (data.type === 'failed') reject(new Error(`${contextName}: ${data.message}`));
    });
    worker.addEventListener('error', (event) => reject(new Error(`${contextName} worker error: ${event.message}`)));
  });
  worker.postMessage(
    { type: 'run', coreUrl, contextName, events: part.events, store: part.store, t0Epoch },
    [part.store],
  );
  return { worker, finished };
}

/** Все MIME из записи: честное unsupported, если браузер не умеет хотя бы один. */
function requireMimeSupport(manifest) {
  const unsupported = manifest.events
    .filter((event) => event.op === 'add_source_buffer' || event.op === 'change_type')
    .map((event) => event.mime)
    .filter((mime, position, all) => all.indexOf(mime) === position)
    .filter((mime) => !MediaSource.isTypeSupported(mime));
  if (unsupported.length > 0) throw new UnsupportedError(`MediaSource.isTypeSupported = false for: ${unsupported.join(' | ')}`);
}

export async function runReplayPage() {
  await runScenario('replay-recording', async (ctx) => {
    const tailMs = Number(ctx.params.raw.get('tail_ms') ?? DEFAULT_TAIL_MS);
    const response = await fetch('/bench/recording');
    if (!response.ok) throw new Error(`recording download failed: HTTP ${response.status}`);
    const { manifest, store } = parseRecording(await response.arrayBuffer());
    const parts = partitionByContext(manifest, store);
    ctx.log(`recording: ${manifest.events.length} events, contexts=${[...parts.keys()].join(',')}, recorded in ${manifest.browser} at ${manifest.origin}`);

    const workerContexts = [...parts.keys()].filter((name) => name !== MAIN_CONTEXT);
    if (workerContexts.length > 0 && !MediaSource.canConstructInDedicatedWorker) {
      throw new UnsupportedError(`the recording has worker contexts (${workerContexts.join(', ')}) but MediaSource.canConstructInDedicatedWorker=${MediaSource.canConstructInDedicatedWorker}`);
    }
    requireMimeSupport(manifest);

    const handleBox = createHandleBox();
    const t0Epoch = performance.timeOrigin + performance.now() + START_DELAY_MS;
    const workers = workerContexts.map((name) => startWorkerContext(ctx, name, parts.get(name), t0Epoch, handleBox));
    const mainPart = parts.get(MAIN_CONTEXT) ?? { events: [], store: new ArrayBuffer(0) };
    const mainHost = createMainHost(ctx, handleBox);
    ctx.log('replay-started');
    const mainResult = await runContext({
      contextName: MAIN_CONTEXT, events: mainPart.events, store: mainPart.store, t0Epoch, host: mainHost,
    });
    const workerResults = await Promise.all(workers.map((entry) => entry.finished));
    const results = [mainResult, ...workerResults];
    const executed = results.reduce((sum, result) => sum + result.executed, 0);
    const errors = results.flatMap((result) => result.errors);
    const lateness = Math.max(...results.map((result) => result.maxLatenessMs));
    ctx.log(`replay-finished executed=${executed} errors=${errors.length} max_lateness_ms=${Math.round(lateness)}`);

    // Запас после конца записи: плеер успевает доиграть и написать сводку.
    await sleep(tailMs);
    const video = mainHost.videos.values().next().value ?? null;
    ctx.metric('events_executed', executed);
    ctx.metric('op_errors', errors.length);
    ctx.metric('max_lateness_ms', Math.round(lateness));
    if (video) ctx.metric('video_current_time_s', video.currentTime);
    ctx.check('replay-ops-ok', errors.length === 0,
      errors.length === 0 ? `${executed} operations executed` : errors.slice(0, 5).map((e) => `#${e.index} ${e.op}: ${e.message}`).join(' | '));
    ctx.check('replay-had-a-video-element', video !== null, `elements=${mainHost.videos.size}`);
  }, { budgetMs: 30 * 60 * 1000 });
}
