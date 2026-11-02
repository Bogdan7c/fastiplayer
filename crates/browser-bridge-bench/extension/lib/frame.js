// TEST-ONLY. Двоичные рамки данных сеанса (зеркало data_frame.rs / data-frame.ts, вид 1).
// Заголовок 24 байта little-endian: [0]=1 вид, [1]=флаги 0, [2..4]=резерв 0, [4..8]=поколение u32,
// [8..10]=индекс SourceBuffer u16, [10..16]=резерв 0, [16..24]=порядковый номер u64. Затем груз
// 1 байт .. 1 МиБ (пустой груз и больше лимита — ошибка протокола).
(function (root) {
  "use strict";
  const HEADER_BYTES = 24;
  const MAX_PAYLOAD_BYTES = 1024 * 1024;
  const SOURCE_BUFFER_BYTES_KIND = 1;

  function buildFrame(generation, sourceBuffer, sequence, payload) {
    const frame = new Uint8Array(HEADER_BYTES + payload.byteLength);
    const view = new DataView(frame.buffer);
    view.setUint8(0, SOURCE_BUFFER_BYTES_KIND);
    view.setUint32(4, generation, true);
    view.setUint16(8, sourceBuffer, true);
    view.setBigUint64(16, BigInt(sequence), true);
    frame.set(payload, HEADER_BYTES);
    return frame;
  }

  // Владелец порядковых номеров: свой счётчик на каждую пару (поколение, SourceBuffer), с 0.
  // Один appendBuffer крупнее лимита режется на несколько рамок — номер растёт на каждую рамку.
  function createFrameSequencer() {
    const nextSequence = new Map();
    return {
      framesFor(generation, sourceBuffer, bytes) {
        const key = generation + ":" + sourceBuffer;
        const frames = [];
        for (let offset = 0; offset < bytes.byteLength; offset += MAX_PAYLOAD_BYTES) {
          const sequence = nextSequence.get(key) || 0;
          nextSequence.set(key, sequence + 1);
          frames.push(buildFrame(generation, sourceBuffer, sequence, bytes.subarray(offset, offset + MAX_PAYLOAD_BYTES)));
        }
        return frames;
      },
    };
  }

  const api = { HEADER_BYTES, MAX_PAYLOAD_BYTES, buildFrame, createFrameSequencer };
  root.__fpBench = Object.assign(root.__fpBench || {}, api);
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(globalThis);
