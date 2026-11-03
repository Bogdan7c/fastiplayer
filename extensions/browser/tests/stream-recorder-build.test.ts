// Сборка записи потока страницы: в обычной сборке нет НИ СТРОКИ рекордера, в отладочной он есть и
// работает как целый бандл (хук воркера выполняется в vm поверх фейкового воркера и сдаёт пачку).

import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";
import vm from "node:vm";

import { buildTarget, buildWorkerHookSource } from "../scripts/build.ts";
import type { TargetBrowser } from "../scripts/manifest.ts";
import { captureBinaryIntrinsics } from "../src/page/binary-view.ts";
import { classicWorkerScript } from "../src/page/worker-capture/script-composer.ts";
import { FLUSH_COMMAND_KIND, parseBatchReport } from "../src/page/stream-recorder/worker-wire.ts";
import { TRANSPARENT_POLICY } from "../src/shared/codec-policy.ts";
import { WORKER_CHANNEL_HANDSHAKE } from "../src/shared/worker-channel-contract.ts";
import { encodeWorkerHookConfig } from "../src/shared/worker-hook-config.ts";
import { createFakeWorkerScope } from "./worker/support/fake-worker-scope.ts";

/** Строки, которые встречаются ТОЛЬКО в коде рекордера (формат, договор воркера, отладочная поверхность). */
const RECORDER_MARKERS = [
  "FPREC001",
  "fastiplayer-page-stream-recording",
  "__fastiplayerStreamRecorder",
  "fastiplayer-stream-recorder-export",
  "recorder_batch",
  "recorder_flush",
  "append_truncated",
  "media_source_created",
];

const targets: readonly TargetBrowser[] = ["chrome", "firefox"];

function jsFilesOf(directory: string): { path: string; text: string }[] {
  return readdirSync(directory, { withFileTypes: true, recursive: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".js"))
    .map((entry) => ({ path: join(entry.parentPath, entry.name), text: readFileSync(join(entry.parentPath, entry.name), "utf8") }));
}

describe("обычная сборка не содержит рекордера", () => {
  const outputRoot = mkdtempSync(join(tmpdir(), "fastiplayer-bridge-product-"));
  for (const target of targets) {
    it(`${target}: ни один dist-файл (включая вшитый хук воркера) не содержит строк рекордера`, async () => {
      const directory = await buildTarget(target, outputRoot);
      const files = jsFilesOf(directory);
      assert.ok(files.some((file) => file.path.endsWith("page.js")), "page.js is built");
      for (const file of files) {
        for (const marker of RECORDER_MARKERS) {
          assert.ok(!file.text.includes(marker), `${file.path} contains the recorder marker «${marker}»`);
        }
      }
    });
  }
});

describe("отладочная сборка --stream-recorder", () => {
  const outputRoot = mkdtempSync(join(tmpdir(), "fastiplayer-bridge-recorder-"));

  for (const target of targets) {
    it(`${target}: page.js содержит рекордер, манифест (права) тот же, что у обычной сборки`, async () => {
      const recorderDir = await buildTarget(target, join(outputRoot, "recorder"), { streamRecorder: true });
      const productDir = await buildTarget(target, join(outputRoot, "product"));
      const pageText = readFileSync(join(recorderDir, "page.js"), "utf8");
      for (const marker of ["FPREC001", "fastiplayer-page-stream-recording", "__fastiplayerStreamRecorder", "fastiplayer-stream-recorder-export"]) {
        assert.ok(pageText.includes(marker), `page.js of the recorder build has «${marker}»`);
      }
      // Рекордер не требует новых прав: скачивание идёт якорем из документа страницы.
      const recorderManifest = JSON.parse(readFileSync(join(recorderDir, "manifest.json"), "utf8")) as Record<string, unknown>;
      const productManifest = JSON.parse(readFileSync(join(productDir, "manifest.json"), "utf8")) as Record<string, unknown>;
      assert.deepEqual(recorderManifest, productManifest);
    });
  }

  it("chrome: хук воркера отладочной сборки (целый бандл) записывает MSE воркера и сдаёт пачку по просьбе", async () => {
    const hookSource = await buildWorkerHookSource("chrome", { streamRecorder: true });
    assert.ok(hookSource.includes("recorder_batch"));
    assert.ok(!(await buildWorkerHookSource("chrome")).includes("recorder_batch"), "the product hook has no recorder");

    const fake = createFakeWorkerScope();
    Object.assign(fake.scope, {
      performance,
      JSON,
      setInterval: () => 0,
      console: { debug: () => undefined },
    });
    const context = vm.createContext({ self: fake.scope, TextEncoder, TextDecoder, console: { debug: () => undefined }, importScripts: () => undefined });
    vm.runInContext(classicWorkerScript({ source: hookSource, configLiteral: encodeWorkerHookConfig({ codecPolicy: TRANSPARENT_POLICY }) }, JSON.stringify("https://site.example/w.js")), context);

    const channel = new MessageChannel();
    const received: unknown[] = [];
    channel.port1.addEventListener("message", (event) => received.push((event as MessageEvent).data));
    channel.port1.start();
    try {
      fake.deliverToWorker(WORKER_CHANNEL_HANDSHAKE, [channel.port2]);
      const mediaSource = new (fake.scope.MediaSource as typeof fake.FakeMediaSource)();
      mediaSource.testOpen();
      const sourceBuffer = mediaSource.addSourceBuffer('video/mp4; codecs="avc1.42E01E"');
      sourceBuffer.appendBuffer(new Uint8Array([10, 20, 30]));
      channel.port1.postMessage({ kind: FLUSH_COMMAND_KIND, token: 1 });
      const binary = captureBinaryIntrinsics(globalThis);
      const deadline = Date.now() + 2000;
      const parse = () => received.flatMap((data) => parseBatchReport(data, binary, JSON.parse) ?? []);
      while (parse().length === 0 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 2));
      const [flushed] = parse();
      assert.ok(flushed !== undefined, "the bundle answered the flush request");
      assert.equal(flushed.token, 1);
      assert.deepEqual(flushed.batch.events.map((event) => event.op), ["media_source_created", "source_open", "add_source_buffer", "append"]);
      assert.deepEqual(flushed.batch.bytes, new Uint8Array([10, 20, 30]));
    } finally {
      channel.port1.close();
    }
  });
});
