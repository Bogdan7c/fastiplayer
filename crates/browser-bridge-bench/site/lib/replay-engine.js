// Движок повтора записи страницы (`*.fprec`, см. crates/browser-bridge-bench/src/recording.rs).
//
// Один движок = один контекст записи (`main` или `worker-N`): он проигрывает СВОИ события по
// порядку, в исходном темпе (`t_ms` от общей точки отсчёта `t0Epoch`), настоящими вызовами MSE.
// Один и тот же код работает в главном потоке страницы и в воркере (`replay-worker.js`).
//
// Что исполняется и что нет:
//  - исполняются действия страницы над MediaSource/SourceBuffer и над элементом (ТОЛЬКО в
//    главном потоке: воркеру элемент недоступен);
//  - метки (`update_end`, `buffered`, `sent`, `append_error`, `source_ended`, `source_close`,
//    `handoff_replayed`, `handoff_ended`, `page_command`) не исполняются: это наблюдения записи;
//  - `source_open` — барьер: ждём настоящий `sourceopen` (источник открывается асинхронно после
//    привязки к элементу), иначе `addSourceBuffer` бросил бы InvalidStateError;
//  - `handoff_began` — отметка для стенда: хост вызывает `host.special.handoff_began`.
// Ошибка операции не обрывает повтор: она копится в `errors` и попадает в отчёт страницы.

/** Операции-метки: ничего не исполняют. */
const MARK_OPS = new Set([
  'update_end', 'buffered', 'sent', 'append_error', 'source_ended', 'source_close',
  'handoff_replayed', 'handoff_ended', 'page_command',
]);
/** Сколько ждём `sourceopen`, мс. */
const SOURCE_OPEN_TIMEOUT_MS = 20000;
/** Сколько ждём конца обновления SourceBuffer перед следующей операцией над ним, мс. */
const IDLE_TIMEOUT_MS = 30000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
/** Часы в той же шкале, что `t0Epoch` (одинаковы для страницы и воркера). */
const epochNow = () => performance.timeOrigin + performance.now();

/** Число из записи: `"Infinity"`/`"NaN"` приходят строками. */
function number(value) {
  return typeof value === 'string' ? Number(value) : value;
}

/** Ждёт событие `name` у `target` не дольше `timeoutMs`; бросает при таймауте. */
function waitEvent(target, name, timeoutMs, what) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${what}: no '${name}' within ${timeoutMs} ms`)), timeoutMs);
    target.addEventListener(name, () => { clearTimeout(timer); resolve(); }, { once: true });
  });
}

/**
 * Проигрывает события одного контекста.
 * @param {object} options
 * @param {string} options.contextName `main` или `worker-N` (для сообщений)
 * @param {Array} options.events события контекста по возрастанию t_ms
 * @param {ArrayBuffer} options.store склад байтов ЭТОГО контекста (offset у append — в нём)
 * @param {number} options.t0Epoch момент старта записи в шкале `performance.timeOrigin+now()`
 * @param {{special?: Object<string, Function>, log: Function}} options.host особенности хоста
 * @returns {Promise<{executed:number, marks:number, errors:Array, maxLatenessMs:number}>}
 */
export async function runContext({ contextName, events, store, t0Epoch, host }) {
  const mediaSources = new Map();
  const opened = new Map();
  const sourceBuffers = new Map();
  const result = { executed: 0, marks: 0, errors: [], maxLatenessMs: 0 };
  const state = { mediaSources, sourceBuffers, store };

  const sourceBufferOf = (event) => {
    const found = sourceBuffers.get(`${event.ms}/${event.sb}`);
    if (!found) throw new Error(`no SourceBuffer ${event.ms}/${event.sb}`);
    return found;
  };
  const mediaSourceOf = (event) => {
    const found = mediaSources.get(event.ms);
    if (!found) throw new Error(`no MediaSource ${event.ms}`);
    return found;
  };
  /** SourceBuffer занят `appendBuffer`/`remove`: следующую операцию над ним ждём. */
  const whenIdle = async (sourceBuffer) => {
    if (sourceBuffer.updating) await waitEvent(sourceBuffer, 'updateend', IDLE_TIMEOUT_MS, 'SourceBuffer stayed busy');
  };
  const whenAllIdle = async (mediaSource) => {
    for (const sourceBuffer of mediaSource.sourceBuffers) await whenIdle(sourceBuffer);
  };

  const operations = {
    media_source_created(event) {
      const mediaSource = new MediaSource();
      mediaSources.set(event.ms, mediaSource);
      opened.set(event.ms, new Promise((resolve) => {
        mediaSource.addEventListener('sourceopen', resolve, { once: true });
      }));
    },
    async source_open(event) {
      mediaSourceOf(event);
      let timer;
      const timeout = new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`MediaSource ${event.ms} never opened`)), SOURCE_OPEN_TIMEOUT_MS);
      });
      // clearTimeout в finally: проигравший таймер не должен стать необработанным отказом страницы.
      try { await Promise.race([opened.get(event.ms), timeout]); } finally { clearTimeout(timer); }
    },
    async add_source_buffer(event) {
      sourceBuffers.set(`${event.ms}/${event.sb}`, mediaSourceOf(event).addSourceBuffer(event.mime));
    },
    async remove_source_buffer(event) {
      const sourceBuffer = sourceBufferOf(event);
      await whenIdle(sourceBuffer);
      mediaSourceOf(event).removeSourceBuffer(sourceBuffer);
      sourceBuffers.delete(`${event.ms}/${event.sb}`);
    },
    async append(event) {
      const sourceBuffer = sourceBufferOf(event);
      await whenIdle(sourceBuffer);
      // Копия среза: appendBuffer отдаёт буфер браузеру, а склад нужен дальше.
      sourceBuffer.appendBuffer(store.slice(event.offset, event.offset + event.length));
    },
    async remove(event) {
      const sourceBuffer = sourceBufferOf(event);
      await whenIdle(sourceBuffer);
      sourceBuffer.remove(number(event.start), number(event.end));
    },
    async abort(event) { sourceBufferOf(event).abort(); },
    async change_type(event) {
      const sourceBuffer = sourceBufferOf(event);
      await whenIdle(sourceBuffer);
      sourceBuffer.changeType(event.mime);
    },
    async timestamp_offset(event) {
      const sourceBuffer = sourceBufferOf(event);
      await whenIdle(sourceBuffer);
      sourceBuffer.timestampOffset = number(event.value);
    },
    async append_window_start(event) {
      const sourceBuffer = sourceBufferOf(event);
      await whenIdle(sourceBuffer);
      sourceBuffer.appendWindowStart = number(event.value);
    },
    async append_window_end(event) {
      const sourceBuffer = sourceBufferOf(event);
      await whenIdle(sourceBuffer);
      sourceBuffer.appendWindowEnd = number(event.value);
    },
    async mode(event) {
      const sourceBuffer = sourceBufferOf(event);
      await whenIdle(sourceBuffer);
      sourceBuffer.mode = event.value;
    },
    async duration(event) {
      const mediaSource = mediaSourceOf(event);
      await whenAllIdle(mediaSource);
      mediaSource.duration = number(event.value);
    },
    async end_of_stream(event) {
      const mediaSource = mediaSourceOf(event);
      await whenAllIdle(mediaSource);
      if (event.error) mediaSource.endOfStream(event.error); else mediaSource.endOfStream();
    },
    live_seekable_range(event) { mediaSourceOf(event).setLiveSeekableRange(number(event.start), number(event.end)); },
    live_seekable_range_cleared(event) { mediaSourceOf(event).clearLiveSeekableRange(); },
  };

  for (const event of events) {
    const waitMs = t0Epoch + event.t_ms - epochNow();
    if (waitMs > 0) await sleep(waitMs);
    else result.maxLatenessMs = Math.max(result.maxLatenessMs, -waitMs);
    if (MARK_OPS.has(event.op)) { result.marks += 1; continue; }
    const handler = operations[event.op] ?? host.special?.[event.op];
    if (!handler) {
      result.errors.push({ index: event.index, op: event.op, message: `op is not supported by the replay in ${contextName}` });
      continue;
    }
    try {
      await handler(event, state);
      result.executed += 1;
    } catch (error) {
      host.log(`replay op failed: #${event.index} ${event.op}: ${error?.name}: ${error?.message}`);
      result.errors.push({ index: event.index, op: event.op, message: `${error?.name}: ${error?.message}` });
    }
  }
  return result;
}
