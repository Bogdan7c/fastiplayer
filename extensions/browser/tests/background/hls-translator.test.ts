// Переводчик страницы: поколение нативного HLS нумеруется вместе с файлом и MSE, номерок корня идёт в сообщение.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createDocumentTranslator } from "../../src/background/handoff/page-event-translator.ts";
import { createStreamNumbering } from "../../src/background/handoff/stream-numbering.ts";

describe("translateHlsSource", () => {
  it("generation_started{N} + hls_source_added{N, root}; номер общий с файлом и MSE", () => {
    const translator = createDocumentTranslator(createStreamNumbering());
    const file = translator.translateFileSource();
    const hls = translator.translateHlsSource(7);
    const mse = translator.translate({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: "video/mp4" } });
    assert.ok(file.ok && hls.ok && mse.ok);
    assert.equal(hls.generation, file.generation + 1);
    assert.deepEqual(hls.items, [
      { kind: "message", message: { type: "generation_started", generation: 2 } },
      { kind: "message", message: { type: "hls_source_added", generation: 2, root: 7 } },
    ]);
    assert.deepEqual(mse.items[0], { kind: "message", message: { type: "generation_started", generation: 3 } });
  });

  it("новый документ сеанса продолжает счёт поколений", () => {
    const numbering = createStreamNumbering();
    createDocumentTranslator(numbering).translateHlsSource(0);
    const second = createDocumentTranslator(numbering).translateHlsSource(5);
    assert.ok(second.ok);
    assert.equal(second.generation, 2);
  });
});
