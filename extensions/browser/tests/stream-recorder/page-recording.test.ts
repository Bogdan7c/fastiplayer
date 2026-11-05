// Запись на настоящем мосте: настоящая тень, настоящий контроллер передачи, канал на MessageChannel.
// Доказывает: вызовы СТРАНИЦЫ над элементом записываются, а наши (тени и моста) — нет; шаги передачи
// и куски, ушедшие плееру, совпадают с тем, что дошло до изолированного мира; выгрузка даёт файл
// формата `.fprec`; сводка и событие выгрузки работают.

import { INACTIVE_NEXT_VIDEO_GUARD } from "../page/support/bridge-harness.ts";
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { startPageBridge } from "../../src/page/bridge.ts";
import { captureShadowEnvironment, shadowStarterFor, type ShadowWindow } from "../../src/page/shadow-assembly.ts";
import { fnv1a32Hex, FPREC_MAGIC, type RecordedEvent } from "../../src/page/stream-recorder/format.ts";
import { PageRecording, RECORDER_EXPORT_EVENT, RECORDER_GLOBAL_NAME, type PageRecordingWindow } from "../../src/page/stream-recorder/page-recording.ts";
import { handshake, type IsolatedSide } from "../page/support/bridge-harness.ts";
import { createFakePage, type FakePage } from "../page/support/fake-page.ts";
import { AUDIO_MP4, VIDEO_MP4 } from "../page/support/media-samples.ts";
import { VIDEO_MIME, withoutTime } from "./support/recording-scope.ts";

const AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"';

/** Скачанные «файлы»: имя и склеенные байты. */
interface Downloaded {
  readonly filename: string;
  readonly bytes: Uint8Array;
}

function startRecordedBridge() {
  const page: FakePage = createFakePage();
  // Шпион на «браузерный» pause: докажет, что тень реально паузит настоящий элемент (иначе проверка «не записано» пуста).
  const mediaPrototype = Object.getPrototypeOf(page.FakeVideoElement.prototype) as { pause(): void };
  const browserPause = mediaPrototype.pause;
  const browserPauseCalls = { count: 0 };
  mediaPrototype.pause = function (this: unknown) {
    browserPauseCalls.count += 1;
    return Reflect.apply(browserPause, this, []);
  };
  // Окно записи: нужны origin, JSON, часы с timeOrigin; остальное — из фейковой страницы.
  Object.assign(page.win, {
    origin: "https://site.example",
    JSON,
    performance: { now: () => page.clock.nowMs, timeOrigin: 0 },
  });
  const downloads: Downloaded[] = [];
  const oddities: string[] = [];
  const shadowEnvironment = captureShadowEnvironment(page.win as unknown as ShadowWindow);
  assert.ok(shadowEnvironment !== null, "the fake page hosts the real shadow");
  const recording = new PageRecording(page.win as unknown as PageRecordingWindow, {
    browser: "chrome",
    reportOddity: (what) => oddities.push(what),
    wallClockMs: () => 1_793_700_000_000,
    downloader: {
      download: (parts, filename) => {
        const total = parts.reduce((sum, part) => sum + part.length, 0);
        const bytes = new Uint8Array(total);
        let position = 0;
        for (const part of parts) {
          bytes.set(part, position);
          position += part.length;
        }
        downloads.push({ filename, bytes });
        return total;
      },
    },
  });
  const bridge = startPageBridge(page.win, {
    startShadow: shadowStarterFor(shadowEnvironment),
    ...INACTIVE_NEXT_VIDEO_GUARD,
    debugSink: (line) => oddities.push(line),
    handoffTap: recording.tap,
    workerChannelExtension: recording.channelExtension,
  });
  recording.install(bridge.tracker);
  return { page, bridge, recording, downloads, oddities, browserPauseCalls };
}

/** Разобрать скачанный `.fprec`: магия, манифест, склад. */
function parseFprec(bytes: Uint8Array): { manifest: { events: RecordedEvent[]; truncated: boolean; browser: string; origin: string; started_at_unix_ms: number; version: number; format: string }; store: Uint8Array } {
  assert.equal(new TextDecoder().decode(bytes.slice(0, 8)), FPREC_MAGIC);
  const manifestLength = new DataView(bytes.buffer, bytes.byteOffset + 8, 4).getUint32(0, true);
  const manifest = JSON.parse(new TextDecoder().decode(bytes.slice(12, 12 + manifestLength)));
  return { manifest, store: bytes.slice(12 + manifestLength) };
}

async function beginHandoff(isolated: IsolatedSide): Promise<void> {
  isolated.send({ kind: "describe_candidates", request_id: 1 });
  await isolated.waitFor(1);
  const reply = isolated.received[0];
  assert.ok(reply !== undefined && reply.kind === "candidates");
  const live = reply.candidates.find((candidate) => candidate.media.kind === "media_source");
  assert.ok(live !== undefined);
  isolated.received.length = 0;
  isolated.send({ kind: "begin_handoff", request_id: 2, document: live.document, element: live.element });
}

describe("запись на настоящем мосте", () => {
  it("действия страницы над элементом пишутся, а вызовы тени и моста (оригиналы) — нет", async () => {
    const { page, recording, browserPauseCalls } = startRecordedBridge();
    const video = page.addVideo();
    video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
    const mediaSource = page.attachMediaSource(video);
    const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    sourceBuffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 733));
    video.testSetState({ paused: false, currentTime: 0.25 });

    const isolated = handshake(page);
    try {
      await beginHandoff(isolated);
      await isolated.waitFor(5);
      await isolated.settle();
      // Тень стоит: она сама паузила настоящий элемент (браузерный pause вызван), но это НЕ действие страницы.
      assert.equal(browserPauseCalls.count, 1, "the shadow paused the real element through the original");
      assert.equal(recording.recorder.sortedEvents().filter((event) => event.op === "element_pause").length, 0, "the shadow's own pause is not a page action");

      // Теперь ходит страница.
      video.pause();
      video.play().catch(() => undefined); // тень может прервать обещание — для записи это неважно
      video.currentTime = 7.25;
      video.playbackRate = 1.5;
      video.load();
      const pageActions = recording.recorder
        .sortedEvents()
        .filter((event) => event.op.startsWith("element_") && event.op !== "element_source");
      assert.deepEqual(withoutTime(pageActions), [
        { ctx: "main", op: "element_pause", el: "el-1" },
        { ctx: "main", op: "element_play", el: "el-1" },
        { ctx: "main", op: "element_seek", el: "el-1", value: 7.25 },
        { ctx: "main", op: "element_rate", el: "el-1", value: 1.5 },
        { ctx: "main", op: "element_load", el: "el-1" },
      ]);
    } finally {
      isolated.close();
    }
  });

  it("источник элемента: blob MediaSource, setAttribute, removeAttribute, srcObject — каждый своим видом", () => {
    const { page, recording } = startRecordedBridge();
    const video = page.addVideo();
    const mediaSource = new page.FakeMediaSource();
    const url = page.FakeURL.createObjectURL(mediaSource);
    video.src = url;
    video.setAttribute("src", "https://cdn.example/clip.mp4");
    video.removeAttribute("src");
    video.srcObject = mediaSource;
    video.srcObject = {};
    video.srcObject = null;
    const sources = recording.recorder.sortedEvents().filter((event) => event.op === "element_source");
    assert.deepEqual(withoutTime(sources), [
      { ctx: "main", op: "element_source", el: "el-1", kind: "media_source_url", ms: "ms-1" },
      { ctx: "main", op: "element_source", el: "el-1", kind: "src_attribute" },
      { ctx: "main", op: "element_source", el: "el-1", kind: "none" },
      { ctx: "main", op: "element_source", el: "el-1", kind: "src_object_media_source", ms: "ms-1" },
      { ctx: "main", op: "element_source", el: "el-1", kind: "src_object_other" },
      { ctx: "main", op: "element_source", el: "el-1", kind: "none" },
    ]);
  });

  it("передача: handoff_began → sent(replay) → handoff_replayed → команда плеера → handoff_ended; sent совпадает с байтами, дошедшими до плеера", async () => {
    const { page, recording } = startRecordedBridge();
    const video = page.addVideo();
    video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
    const mediaSource = page.attachMediaSource(video);
    const videoBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    const audioBuffer = mediaSource.addSourceBuffer(AUDIO_MIME);
    const videoFile = VIDEO_MP4.bytes();
    videoBuffer.appendBuffer(videoFile.slice(0, 733));
    audioBuffer.appendBuffer(AUDIO_MP4.bytes().slice());
    video.testSetState({ paused: false, currentTime: 0.25 });

    const isolated = handshake(page);
    try {
      await beginHandoff(isolated);
      await isolated.waitFor(5);
      // Живой кусок уже после начала передачи.
      videoBuffer.appendBuffer(videoFile.slice(733, 1500));
      await isolated.waitFor(6);
      isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
      await isolated.settle();

      const delivered = isolated.received.filter((message) => message.kind === "bytes");
      const events = recording.recorder.sortedEvents();
      const sent = events.filter((event) => event.op === "sent");
      assert.equal(sent.length, delivered.length, "every chunk that reached the player is recorded");
      sent.forEach((event, order) => {
        const message = delivered[order];
        assert.ok(message !== undefined && message.kind === "bytes");
        assert.equal(event["length"], message.bytes.length);
        assert.equal(event["hash"], fnv1a32Hex(message.bytes), "the recorded hash is the hash of the bytes the player received");
      });
      assert.deepEqual(sent.map((event) => [event["sb"], event["replay"]]), [["sb-1", true], ["sb-2", true], ["sb-1", false]]);

      const steps = events.map((event) => event.op).filter((op) => ["handoff_began", "handoff_replayed", "page_command", "handoff_ended"].includes(op));
      // Первая команда — сам begin_handoff, вторая — end_handoff.
      assert.deepEqual(steps, ["page_command", "handoff_began", "handoff_replayed", "page_command", "handoff_ended"]);
      const replayed = events.find((event) => event.op === "handoff_replayed");
      assert.deepEqual(replayed && { items: replayed["items"], bytes: replayed["bytes"] }, { items: 2, bytes: 733 + AUDIO_MP4.bytes().length });
      const command = events.filter((event) => event.op === "page_command").at(-1);
      assert.deepEqual(events.find((event) => event.op === "page_command")?.["command"] !== undefined && (events.find((event) => event.op === "page_command")?.["command"] as { kind: string }).kind, "begin_handoff");
      assert.deepEqual(command?.["command"], { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
      const ended = events.find((event) => event.op === "handoff_ended");
      assert.equal(ended?.["cause"], "returned");
      assert.equal(events.find((event) => event.op === "handoff_began")?.["el"], "el-1");
    } finally {
      isolated.close();
    }
  });
});

describe("выгрузка и отладочная поверхность", () => {
  it("exportRecording отдаёт файл .fprec: магия, манифест формата, склад = байты append; итог описывает файл", async () => {
    const { page, recording, downloads } = startRecordedBridge();
    const video = page.addVideo();
    const mediaSource = page.attachMediaSource(video);
    const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
    const first = VIDEO_MP4.bytes().slice(0, 733);
    const second = VIDEO_MP4.bytes().slice(733, 1200);
    sourceBuffer.appendBuffer(first);
    sourceBuffer.appendBuffer(second);

    const result = await recording.exportRecording();
    assert.equal(downloads.length, 1);
    const file = downloads[0]!;
    assert.match(file.filename, /^fastiplayer-https_site\.example-1793700000000\.fprec$/);
    const { manifest, store } = parseFprec(file.bytes);
    assert.equal(manifest.format, "fastiplayer-page-stream-recording");
    assert.equal(manifest.version, 1);
    assert.equal(manifest.browser, "chrome");
    assert.equal(manifest.origin, "https://site.example", "only the origin, no path or query");
    assert.equal(manifest.started_at_unix_ms, 1_793_700_000_000);
    assert.equal(manifest.truncated, false);
    const appends = manifest.events.filter((event) => event.op === "append");
    assert.deepEqual(
      appends.map((event) => store.slice(event["offset"] as number, (event["offset"] as number) + (event["length"] as number))),
      [first, second],
      "the store holds exactly the appended bytes",
    );
    assert.deepEqual(manifest.events.map((event) => event.t_ms), [...manifest.events.map((event) => event.t_ms)].sort((a, b) => a - b), "events are ordered by t_ms");
    assert.equal(result.filename, file.filename);
    assert.equal(result.size_bytes, file.bytes.length);
    assert.equal(result.events, manifest.events.length);
    assert.equal(result.truncated, false);
  });

  it("глобал-сводка видна скрипту и растёт вместе с записью; событие окна запускает выгрузку", async () => {
    const { page, downloads, oddities } = startRecordedBridge();
    const surface = (page.win as unknown as Record<string, { summary(): { recording: boolean; events: number; appended_bytes: number; truncated: boolean; browser: string; origin: string }; exportRecording(): Promise<unknown> }>)[RECORDER_GLOBAL_NAME];
    assert.ok(surface !== undefined, "the debug global exists in the recording build");
    assert.deepEqual(Object.keys(page.win).includes(RECORDER_GLOBAL_NAME), false, "the global is not enumerable");
    const before = surface.summary();
    assert.equal(before.recording, true);
    assert.equal(before.events, 0);

    const video = page.addVideo();
    const mediaSource = page.attachMediaSource(video);
    mediaSource.addSourceBuffer(VIDEO_MIME).appendBuffer(new Uint8Array(100).fill(7));
    const after = surface.summary();
    assert.ok(after.events >= 4, "created, open, source, add_source_buffer, append...");
    assert.equal(after.appended_bytes, 100);
    assert.equal(after.truncated, false);
    assert.equal(after.browser, "chrome");
    assert.equal(after.origin, "https://site.example");

    page.win.dispatchEvent(new Event(RECORDER_EXPORT_EVENT));
    for (let round = 0; round < 20 && downloads.length === 0; round += 1) await new Promise((resolve) => setTimeout(resolve, 2));
    assert.equal(downloads.length, 1, "the window event triggers the export");
    assert.deepEqual(oddities, []);
  });
});
