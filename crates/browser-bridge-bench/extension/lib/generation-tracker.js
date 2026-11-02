// TEST-ONLY. Учёт поколений и SourceBuffer по событиям MSE — чистая логика без браузерных API,
// поэтому её можно проверить фейками в node. Хук (page-hook.js) лишь вызывает note*() ПОСЛЕ
// успешного оригинального вызова и отдаёт результат в emit().
//
// Инварианты:
//  * поколение = один экземпляр MediaSource; номер начинается с 1 и растёт в пределах фрейма.
//    Поколение открывается лениво, на первом addSourceBuffer (addSourceBuffer возможен только у
//    присоединённого MediaSource, так что это и есть «присоединён к элементу»);
//  * индекс SourceBuffer 0..7 — по порядку addSourceBuffer внутри поколения;
//  * события SourceBuffer, который мы не отслеживаем (больше 8 или неверный mime), молча не
//    теряются, а идут в журнал одной строкой.
// emit получает: {kind:"message", message} (готовое сообщение протокола page->player),
// {kind:"append", generation, source_buffer, bytes} или {kind:"log", line}.
(function (root) {
  "use strict";
  const MAX_SOURCE_BUFFERS = 8;
  const MAX_MIME_BYTES = 256;
  const MICROS_PER_SECOND = 1e6;

  const toMicros = (seconds) => Math.round(seconds * MICROS_PER_SECOND);
  // mime в протоколе: печатный ASCII 0x20..0x7e, 1..256 байт.
  const isValidMime = (mime) =>
    typeof mime === "string" && mime.length >= 1 && mime.length <= MAX_MIME_BYTES && /^[\x20-\x7e]+$/.test(mime);

  function createGenerationTracker(options) {
    const emit = options.emit;
    const mediaSources = new WeakMap(); // MediaSource -> {generation, nextIndex}
    const pendingDurations = new WeakMap(); // duration, выставленный до первого addSourceBuffer
    const sourceBuffers = new WeakMap(); // SourceBuffer -> {generation, index}
    let lastGeneration = 0;

    const message = (body) => emit({ kind: "message", message: body });
    const log = (line) => emit({ kind: "log", line });

    function durationMessage(generation, seconds) {
      if (Number.isNaN(seconds)) return { type: "duration_changed", generation, duration: { kind: "unknown" } };
      if (seconds === Infinity) return { type: "duration_changed", generation, duration: { kind: "live" } };
      return { type: "duration_changed", generation, duration: { kind: "finite", duration_us: Math.max(0, toMicros(seconds)) } };
    }

    function startGeneration(mediaSource) {
      lastGeneration += 1;
      const state = { generation: lastGeneration, nextIndex: 0 };
      mediaSources.set(mediaSource, state);
      message({ type: "generation_started", generation: state.generation });
      if (pendingDurations.has(mediaSource)) {
        message(durationMessage(state.generation, pendingDurations.get(mediaSource)));
        pendingDurations.delete(mediaSource);
      }
      return state;
    }

    function trackedBuffer(sourceBuffer) {
      return sourceBuffers.get(sourceBuffer) || null;
    }

    return {
      noteSourceBufferAdded(mediaSource, sourceBuffer, mime) {
        const state = mediaSources.get(mediaSource) || startGeneration(mediaSource);
        if (state.nextIndex >= MAX_SOURCE_BUFFERS) { log("source buffer ignored: more than 8 in generation " + state.generation); return; }
        if (!isValidMime(mime)) { log("source buffer ignored: mime not representable in protocol"); return; }
        const index = state.nextIndex;
        state.nextIndex += 1;
        sourceBuffers.set(sourceBuffer, { generation: state.generation, index });
        message({ type: "source_buffer_added", generation: state.generation, source_buffer: index, mime_type: mime });
      },
      noteAppend(sourceBuffer, bytes) {
        const buffer = trackedBuffer(sourceBuffer);
        if (!buffer || bytes.byteLength === 0) return; // пустой груз протокол отвергает
        emit({ kind: "append", generation: buffer.generation, source_buffer: buffer.index, bytes });
      },
      noteChangeType(sourceBuffer, mime) {
        const buffer = trackedBuffer(sourceBuffer);
        if (!buffer) return;
        if (!isValidMime(mime)) { log("changeType ignored: mime not representable in protocol"); return; }
        message({ type: "source_buffer_type_changed", generation: buffer.generation, source_buffer: buffer.index, mime_type: mime });
      },
      noteTimestampOffset(sourceBuffer, seconds) {
        const buffer = trackedBuffer(sourceBuffer);
        if (!buffer || !Number.isFinite(seconds)) return;
        message({ type: "timestamp_offset_changed", generation: buffer.generation, source_buffer: buffer.index, offset_us: toMicros(seconds) });
      },
      // end === Infinity (remove(start, Infinity)) = «до конца таймлайна».
      noteRemove(sourceBuffer, startSeconds, endSeconds) {
        const buffer = trackedBuffer(sourceBuffer);
        if (!buffer) return;
        const end = Number.isFinite(endSeconds) ? { kind: "at", time_us: Math.max(0, toMicros(endSeconds)) } : { kind: "end_of_timeline" };
        message({ type: "source_buffer_removed", generation: buffer.generation, source_buffer: buffer.index, start_us: Math.max(0, toMicros(startSeconds)), end });
      },
      noteAbort(sourceBuffer) {
        const buffer = trackedBuffer(sourceBuffer);
        if (buffer) message({ type: "source_buffer_aborted", generation: buffer.generation, source_buffer: buffer.index });
      },
      noteDuration(mediaSource, seconds) {
        const state = mediaSources.get(mediaSource);
        if (state) message(durationMessage(state.generation, seconds));
        else pendingDurations.set(mediaSource, seconds);
      },
      // endOfStream("network"|"decode") — это сбой, а не конец потока: не пересказываем.
      noteEndOfStream(mediaSource, errorArgument) {
        const state = mediaSources.get(mediaSource);
        if (state && errorArgument === undefined) message({ type: "end_of_stream", generation: state.generation });
      },
    };
  }

  const api = { createGenerationTracker };
  root.__fpBench = Object.assign(root.__fpBench || {}, api);
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(globalThis);
