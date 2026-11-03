// Канал участка 1 и контроллер передачи: рукопожатие, описание кандидатов, `begin_handoff` по
// шагам, повтор памяти, живой поток только для MediaSource элемента, остановка потока.
// Канал — настоящий MessageChannel Node: сообщения идут асинхронно и через структурное
// клонирование, как в браузере.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { HANDSHAKE_MARKER_FIELD } from "../../src/shared/page-channel-handshake.ts";
import { MAX_PAGE_BYTES_EVENT_BYTES, MAX_VIDEO_CANDIDATES_PER_DOCUMENT, parseCandidatesReply, parsePageEvent } from "../../src/shared/page-message-validation.ts";
import { handshake, startHarness, startRealShadowHarness, type BridgeHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { AUDIO_MP4, VIDEO_MP4, concat } from "./support/media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"';

/** Страница с играющим видео: init и два фрагмента, звук целиком. */
function playingPage(harness: Pick<BridgeHarness, "page">) {
  const { page } = harness;
  const video = page.addVideo();
  video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
  const mediaSource = page.attachMediaSource(video);
  mediaSource.duration = 0.4;
  const videoBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
  const audioBuffer = mediaSource.addSourceBuffer(AUDIO_MIME);
  const videoFile = VIDEO_MP4.bytes();
  // Как часто бывает у плееров: куски — виды на одну большую загрузку страницы.
  const download = videoFile.slice().buffer;
  videoBuffer.appendBuffer(new Uint8Array(download, 0, 733));
  audioBuffer.appendBuffer(AUDIO_MP4.bytes().slice());
  videoBuffer.appendBuffer(new Uint8Array(download, 733));
  video.testSetState({ paused: false, currentTime: 0.25 });
  return { video, mediaSource, videoBuffer, audioBuffer, download };
}

async function withChannel(harness: Pick<BridgeHarness, "page">, body: (isolated: IsolatedSide) => Promise<void>): Promise<void> {
  const isolated = handshake(harness.page);
  try {
    await body(isolated);
    await isolated.settle();
    assertAcceptedByIsolatedWorld(harness.page.postedToPorts.map(({ clonedBeforeSend }) => clonedBeforeSend));
  } finally {
    isolated.close();
  }
}

/** Всё, что MAIN-мир отправил, обязано пройти проверку изолированного мира (пакет B). */
function assertAcceptedByIsolatedWorld(messages: readonly unknown[]): void {
  for (const message of messages) {
    const accepted = (message as { kind?: unknown }).kind === "candidates" ? parseCandidatesReply(message) : parsePageEvent(message);
    assert.ok(accepted !== undefined, `isolated world rejects ${JSON.stringify(message, (_key, value: unknown) => (value instanceof Uint8Array ? `bytes(${value.length})` : value))}`);
  }
}

function kinds(messages: readonly unknown[]): string[] {
  return messages.map((message) => {
    const typed = message as PageEvent;
    return typed.kind === "mse" ? `mse:${typed.event.kind}` : typed.kind;
  });
}

async function describeAndBegin(isolated: IsolatedSide): Promise<number> {
  isolated.send({ kind: "describe_candidates", request_id: 1 });
  await isolated.waitFor(1);
  const reply = isolated.received[0];
  assert.ok(reply !== undefined && reply.kind === "candidates");
  const live = reply.candidates.find((candidate) => candidate.media.kind === "media_source");
  assert.ok(live !== undefined, "a live candidate exists");
  isolated.received.length = 0;
  isolated.send({ kind: "begin_handoff", request_id: 2, document: live.document, element: live.element });
  return live.element;
}

test("handshake: only the first tagged message with one port is taken and hidden from the page", async () => {
  const harness = startHarness();
  const pageSaw: unknown[] = [];
  harness.page.win.addEventListener("message", (event) => pageSaw.push((event as MessageEvent).data));
  const first = handshake(harness.page);
  const second = handshake(harness.page); // вторая попытка (например, страница) — странице как есть
  try {
    assert.equal(pageSaw.length, 1, "the page sees only the second handshake");
    first.send({ kind: "describe_candidates", request_id: 7 });
    second.send({ kind: "describe_candidates", request_id: 8 });
    await first.waitFor(1);
    await second.settle();
    assert.deepEqual(first.received, [{ kind: "candidates", request_id: 7, candidates: [] }]);
    assert.equal(second.received.length, 0, "the second port is not ours");
  } finally {
    first.close();
    second.close();
  }
});

test("handshake: foreign messages pass to the page untouched and do not open the channel", async () => {
  const harness = startHarness();
  const { page } = harness;
  let pageSaw = 0;
  page.win.addEventListener("message", () => {
    pageSaw += 1;
  });
  const attempts = [
    handshake(page, { [HANDSHAKE_MARKER_FIELD]: 999 }), // чужая версия
    handshake(page, { [HANDSHAKE_MARKER_FIELD]: 1, extra: true }), // лишнее поле
    handshake(page, { hello: 1 }), // не наша метка
    handshake(page, { [HANDSHAKE_MARKER_FIELD]: 1 }, null), // не от своего окна
  ];
  try {
    for (const attempt of attempts) attempt.send({ kind: "describe_candidates", request_id: 1 });
    for (const attempt of attempts) await attempt.settle();
    assert.equal(pageSaw, attempts.length);
    assert.ok(attempts.every((attempt) => attempt.received.length === 0));
    const real = handshake(page);
    real.send({ kind: "describe_candidates", request_id: 2 });
    await real.waitFor(1);
    real.close();
  } finally {
    for (const attempt of attempts) attempt.close();
  }
});

test("describe_candidates: keys, live media, playing and visible area, including videos in closed and nested open shadow roots", async () => {
  const harness = startHarness();
  const { page } = harness;
  const { video } = playingPage(harness);
  // Второе видео — в закрытом shadow root, без MSE, наполовину за правым краем окна.
  const host = new page.FakeElement("player-host");
  page.document.children.push(host);
  const root = host.attachShadow({ mode: "closed" });
  const hidden = new page.FakeVideoElement();
  hidden.rect = { left: 1180, top: 100, right: 1380, bottom: 200 };
  root.children.push(hidden);
  // Третье — в открытом shadow root, вложенном в закрытый (находится обходом).
  const innerHost = new page.FakeElement("inner-host");
  root.children.push(innerHost);
  const innerRoot = innerHost.attachShadow({ mode: "open" });
  const nested = new page.FakeVideoElement();
  nested.rect = { left: -50, top: -50, right: 50, bottom: 50 };
  innerRoot.children.push(nested);

  await withChannel(harness, async (isolated) => {
    isolated.send({ kind: "describe_candidates", request_id: 5 });
    await isolated.waitFor(1);
    assert.deepEqual(isolated.received[0], {
      kind: "candidates",
      request_id: 5,
      candidates: [
        { document: harness.bridge.documentTag, element: 1, media: { kind: "media_source" }, playing: true, visible_area_px: 640 * 360 },
        { document: harness.bridge.documentTag, element: 2, media: { kind: "none" }, playing: false, visible_area_px: 100 * 100 },
        { document: harness.bridge.documentTag, element: 3, media: { kind: "none" }, playing: false, visible_area_px: 50 * 50 },
      ],
    });
    // Ключи стабильны: повторный запрос даёт те же номера.
    video.testSetState({ paused: true });
    isolated.send({ kind: "describe_candidates", request_id: 6 });
    await isolated.waitFor(2);
    const second = isolated.received[1];
    assert.ok(second?.kind === "candidates");
    assert.deepEqual(
      second.candidates.map((candidate) => [candidate.element, candidate.playing]),
      [
        [1, false],
        [2, false],
        [3, false],
      ],
    );
  });
});

test("begin_handoff: began → replay of memory → page_seek(T0) → page_playback(play) → shadow, bytes exact and transferred", async () => {
  const harness = startHarness();
  const { page, shadow } = harness;
  const { video, download } = playingPage(harness);
  await withChannel(harness, async (isolated) => {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    await isolated.settle();
    assert.deepEqual(kinds(isolated.received), [
      "handoff_began",
      "mse:source_buffer_added",
      "mse:duration_changed",
      "mse:source_buffer_added",
      "bytes",
      "bytes",
      "bytes",
      "page_seek",
      "page_playback",
    ]);
    assert.deepEqual(isolated.received[0], { kind: "handoff_began", request_id: 2, outcome: "started" });
    assert.deepEqual(isolated.received[7], { kind: "page_seek", target_us: 250_000 });
    assert.deepEqual(isolated.received[8], { kind: "page_playback", request: { kind: "play" } });
    assert.equal(shadow.started.length, 1);
    assert.equal(shadow.started[0]?.element, video, "the real element went into the shadow");
    // Тень получила точное знание реестра: адрес MediaSource — да, blob обычного файла — нет.
    const plainFileUrl = page.FakeURL.createObjectURL({ notAMediaSource: true });
    assert.equal(shadow.started[0]?.isMediaSourceObjectUrl(video.src), true);
    assert.equal(shadow.started[0]?.isMediaSourceObjectUrl(plainFileUrl), false);

    // Байты по SourceBuffer — ровно файлы, каждый кусок в собственном буфере ровно своего размера.
    const bytesEvents = isolated.received.filter((message) => message.kind === "bytes");
    for (const event of bytesEvents) assert.equal(event.bytes.buffer.byteLength, event.bytes.byteLength);
    const bySourceBuffer = (index: number) => concat(bytesEvents.filter((event) => event.source_buffer === index).map((event) => event.bytes));
    assert.deepEqual(bySourceBuffer(0), VIDEO_MP4.bytes());
    assert.deepEqual(bySourceBuffer(1), AUDIO_MP4.bytes());
    // Отправитель передал копию (transfer): у него она отсоединена, а у страницы буферы целы.
    const sentBytes = page.postedToPorts.filter(({ message }) => (message as PageEvent).kind === "bytes");
    assert.equal(sentBytes.length, 3);
    for (const { message, transfer } of sentBytes) {
      const bytes = (message as Extract<PageEvent, { kind: "bytes" }>).bytes;
      assert.deepEqual(transfer, [bytes.buffer]);
      assert.equal(bytes.buffer.byteLength, 0, "detached at the sender after transfer");
    }
    assert.equal(download.byteLength, VIDEO_MP4.bytes().length, "the page's own buffer is untouched");
  });
});

test("live stream: only the handed element's media sources, including a new one attached to it; stops after end_handoff", async () => {
  const harness = startHarness();
  const { page, shadow } = harness;
  const { video, videoBuffer } = playingPage(harness);
  // Другое видео со своим MediaSource на той же странице.
  const otherVideo = page.addVideo();
  const otherSource = page.attachMediaSource(otherVideo);
  const otherBuffer = otherSource.addSourceBuffer(VIDEO_MIME);

  await withChannel(harness, async (isolated) => {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    await isolated.settle();
    isolated.received.length = 0;

    videoBuffer.timestampOffset = 0.5;
    otherBuffer.appendBuffer(VIDEO_MP4.bytes().slice()); // чужой элемент — в порт не идёт
    videoBuffer.appendBuffer(VIDEO_MP4.bytes().slice(733, 5714));
    // Страница подключила к тому же элементу новый MediaSource (смена качества).
    const nextSource = page.attachMediaSource(video);
    const nextBuffer = nextSource.addSourceBuffer(VIDEO_MIME);
    nextBuffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 733));
    await isolated.waitFor(4);
    await isolated.settle();
    assert.deepEqual(isolated.received.map((message) => (message.kind === "mse" ? message.event : message.kind === "bytes" ? [message.media_source, message.source_buffer, message.bytes.length] : message)), [
      { kind: "timestamp_offset_changed", media_source: "ms-1", source_buffer: 0, offset_us: 500_000 },
      ["ms-1", 0, 5714 - 733],
      { kind: "source_buffer_added", media_source: "ms-3", source_buffer: 0, mime_type: VIDEO_MIME },
      ["ms-3", 0, 733],
    ]);

    isolated.received.length = 0;
    isolated.send({ kind: "end_handoff", position: { kind: "at", position_us: 1_000_000 }, resume: { kind: "exactly", resume: "paused" } });
    await isolated.waitFor(1);
    assert.deepEqual(isolated.received, [{ kind: "handoff_ended", cause: "returned" }]);
    assert.deepEqual(shadow.ended, [{ position: { kind: "at", position_us: 1_000_000 }, resume: { kind: "exactly", resume: "paused" } }]);
    videoBuffer.appendBuffer(VIDEO_MP4.bytes().slice(5714));
    nextBuffer.appendBuffer(VIDEO_MP4.bytes().slice(733));
    await isolated.settle();
    assert.equal(isolated.received.length, 1, "nothing flows after the handoff ended");
  });
});

test("shadow commands reach the shadow; the shadow's own handoff_ended stops the stream", async () => {
  const harness = startHarness();
  const { shadow } = harness;
  const { videoBuffer } = playingPage(harness);
  await withChannel(harness, async (isolated) => {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    isolated.send({ kind: "shadow", command: { kind: "seek", seek_id: 4, target_us: 300_000 } });
    isolated.send({ kind: "shadow", command: { kind: "clock", position_us: 10, state: "playing", rate: 1000 } });
    await isolated.settle();
    assert.deepEqual(shadow.applied, [
      { kind: "seek", seek_id: 4, target_us: 300_000 },
      { kind: "clock", position_us: 10, state: "playing", rate: 1000 },
    ]);
    // Страница сама убрала источник: тень снялась и сказала об этом.
    isolated.received.length = 0;
    shadow.started[0]?.emit({ kind: "handoff_ended", cause: "element_released" });
    videoBuffer.appendBuffer(VIDEO_MP4.bytes().slice(733));
    await isolated.waitFor(1);
    await isolated.settle();
    assert.deepEqual(isolated.received, [{ kind: "handoff_ended", cause: "element_released" }]);
  });
});

test("begin_handoff outcomes: unknown element, element without live media, shadow that fails to start", async () => {
  const harness = startHarness();
  const { page, shadow } = harness;
  const plain = page.addVideo();
  plain.rect = { left: 0, top: 0, right: 10, bottom: 10 };
  await withChannel(harness, async (isolated) => {
    isolated.send({ kind: "describe_candidates", request_id: 1 });
    const tag = harness.bridge.documentTag;
    isolated.send({ kind: "begin_handoff", request_id: 2, document: tag, element: 1 });
    isolated.send({ kind: "begin_handoff", request_id: 3, document: tag, element: 99 });
    // Тот же ключ элемента, но метка другого документа (фрейм успел смениться) — не наш элемент.
    isolated.send({ kind: "begin_handoff", request_id: 4, document: "0123456789abcdef0123456789abcdef", element: 1 });
    await isolated.waitFor(4);
    assert.deepEqual(isolated.received.slice(1), [
      { kind: "handoff_began", request_id: 2, outcome: "no_media" },
      { kind: "handoff_began", request_id: 3, outcome: "element_not_found" },
      { kind: "handoff_began", request_id: 4, outcome: "element_not_found" },
    ]);
  });

  const failing = startHarness();
  playingPage(failing);
  failing.shadow.failToStart = new Error("shadow broke");
  await withChannel(failing, async (isolated) => {
    await describeAndBegin(isolated);
    await isolated.waitFor(10);
    assert.deepEqual(isolated.received.at(-1), { kind: "handoff_ended", cause: "returned" });
    assert.equal(failing.bridge.controller.isHandingOff, false);
  });
  assert.equal(shadow.started.length, 0);
});

test("garbage commands are ignored without an answer; a valid one afterwards still works", async () => {
  const harness = startHarness();
  await withChannel(harness, async (isolated) => {
    for (const garbage of [
      null,
      "describe_candidates",
      { kind: "describe_candidates" },
      { kind: "describe_candidates", request_id: -1 },
      { kind: "describe_candidates", request_id: 1, extra: 1 },
      { kind: "begin_handoff", request_id: 1, document: "x", element: "1" },
      { kind: "begin_handoff", request_id: 1, element: 1 },
      { kind: "shadow", command: { kind: "set_rate", rate: 5 } },
      { kind: "end_handoff", position: { kind: "at" }, resume: { kind: "as_shadow" } },
      { kind: "launch_missiles" },
    ]) {
      isolated.send(garbage);
    }
    isolated.send({ kind: "describe_candidates", request_id: 9 });
    await isolated.waitFor(1);
    await isolated.settle();
    assert.deepEqual(isolated.received, [{ kind: "candidates", request_id: 9, candidates: [] }]);
  });
});

test("a second handoff after return replays the memory again (journal is kept during and after a handoff)", async () => {
  const harness = startHarness();
  const { videoBuffer } = playingPage(harness);
  await withChannel(harness, async (isolated) => {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    videoBuffer.appendBuffer(VIDEO_MP4.bytes().slice(733, 5714)); // во время передачи
    isolated.send({ kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
    await isolated.waitFor(11);
    isolated.received.length = 0;
    isolated.send({ kind: "begin_handoff", request_id: 3, document: harness.bridge.documentTag, element: 1 });
    await isolated.waitFor(10);
    await isolated.settle();
    const videoBytes = isolated.received.filter((message) => message.kind === "bytes" && message.source_buffer === 0);
    assert.equal(
      videoBytes.reduce((sum, message) => sum + (message.kind === "bytes" ? message.bytes.length : 0), 0),
      VIDEO_MP4.bytes().length + (5714 - 733),
      "the replay includes what was appended during the first handoff",
    );
  });
});

test("a chunk larger than one bytes event is sent as consecutive exact parts (≤ MAX_PAGE_BYTES_EVENT_BYTES, each transferred)", async () => {
  const harness = startHarness({ memoryBudgetBytes: 4 * MAX_PAGE_BYTES_EVENT_BYTES });
  const { page } = harness;
  const video = page.addVideo();
  video.rect = { left: 0, top: 0, right: 100, bottom: 100 };
  const mediaSource = page.attachMediaSource(video);
  const sourceBuffer = mediaSource.addSourceBuffer(VIDEO_MIME);
  sourceBuffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 733));
  // Один appendBuffer: moof + mdat больше предела события.
  const mdatBody = MAX_PAGE_BYTES_EVENT_BYTES + 50;
  const big = new Uint8Array(16 + 8 + mdatBody);
  const header = new DataView(big.buffer);
  header.setUint32(0, 16);
  big.set([0x6d, 0x6f, 0x6f, 0x66], 4);
  header.setUint32(16, 8 + mdatBody);
  big.set([0x6d, 0x64, 0x61, 0x74], 20);
  big[big.length - 1] = 0xab;
  sourceBuffer.appendBuffer(big);
  await withChannel(harness, async (isolated) => {
    await describeAndBegin(isolated);
    await isolated.waitFor(6);
    await isolated.settle();
    const parts = isolated.received.filter((message) => message.kind === "bytes").map((message) => (message.kind === "bytes" ? message.bytes : new Uint8Array()));
    assert.deepEqual(parts.map((part) => part.length), [733, MAX_PAGE_BYTES_EVENT_BYTES, big.length - MAX_PAGE_BYTES_EVENT_BYTES]);
    assert.ok(parts.every((part) => part.buffer.byteLength === part.length), "each part is its own exact buffer");
    assert.deepEqual(concat(parts.slice(1)), big);
    // Живой поток режет так же.
    isolated.received.length = 0;
    sourceBuffer.appendBuffer(big);
    await isolated.waitFor(2);
    assert.deepEqual(isolated.received.map((message) => (message.kind === "bytes" ? message.bytes.length : -1)), [MAX_PAGE_BYTES_EVENT_BYTES, big.length - MAX_PAGE_BYTES_EVENT_BYTES]);
  });
});

test("more videos than the isolated world accepts: the reply is capped, the live and playing ones are kept", async () => {
  const harness = startHarness();
  const { page } = harness;
  for (let index = 0; index < MAX_VIDEO_CANDIDATES_PER_DOCUMENT + 40; index += 1) {
    const preview = page.addVideo();
    preview.rect = { left: 0, top: 0, right: 10 + (index % 7), bottom: 10 };
  }
  const { video } = playingPage(harness); // последний в документе, с живым MSE и играет
  await withChannel(harness, async (isolated) => {
    isolated.send({ kind: "describe_candidates", request_id: 1 });
    await isolated.waitFor(1);
    const reply = isolated.received[0];
    assert.ok(reply?.kind === "candidates");
    assert.equal(reply.candidates.length, MAX_VIDEO_CANDIDATES_PER_DOCUMENT);
    assert.deepEqual(reply.candidates[0], { document: harness.bridge.documentTag, element: harness.bridge.registry.keyOf(video as unknown as Element), media: { kind: "media_source" }, playing: true, visible_area_px: 640 * 360 });
  });
});

test("document tag: 128 random bits as hex, different per document, the same in every candidate", () => {
  const first = startHarness();
  const second = startHarness();
  assert.match(first.bridge.documentTag, /^[0-9a-f]{32}$/);
  assert.notEqual(first.bridge.documentTag, second.bridge.documentTag);
});

test("a failed send in the live stream ends the handoff honestly instead of leaving a hole", async () => {
  const harness = startHarness();
  const { page, shadow, bridge } = harness;
  const { videoBuffer } = playingPage(harness);
  const isolated = handshake(page);
  try {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    page.portFailure.error = new Error("DataCloneError: out of memory");
    videoBuffer.appendBuffer(VIDEO_MP4.bytes().slice(733, 5714));
    assert.deepEqual(shadow.ended, [{ position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } }], "the shadow was taken off");
    assert.equal(bridge.controller.isHandingOff, false);
    page.portFailure.error = null;
    const before = isolated.received.length;
    videoBuffer.appendBuffer(VIDEO_MP4.bytes().slice(5714));
    await isolated.settle();
    assert.equal(isolated.received.length, before, "nothing flows after the failed send");
  } finally {
    isolated.close();
  }
});

test("a shadow event that cannot be sent ends the handoff honestly and leaves a trace; nothing flows afterwards", async () => {
  const harness = startHarness();
  const { page, shadow, bridge, debugLines } = harness;
  playingPage(harness);
  const isolated = handshake(page);
  try {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    page.portFailure.error = new Error("DataCloneError: out of memory");
    // Тень не должна получить исключение обратно: передача кончается у контроллера.
    shadow.started[0]?.emit({ kind: "page_seek", target_us: 5_000_000 });
    assert.equal(bridge.controller.isHandingOff, false);
    assert.deepEqual(shadow.ended, [{ position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } }], "the shadow was taken off");
    assert.equal(debugLines.length, 1, "the failed send left a trace");
    page.portFailure.error = null;
    const before = isolated.received.length;
    shadow.started[0]?.emit({ kind: "shadow_alive" });
    await isolated.settle();
    assert.equal(isolated.received.length, before, "no pulse after the handoff ended");
  } finally {
    isolated.close();
  }
});

test("handoff_ended that cannot be sent: no exception into the shadow, a trace, and the shadow's later events are dropped", async () => {
  const harness = startHarness();
  const { page, shadow, bridge, debugLines } = harness;
  playingPage(harness);
  const isolated = handshake(page);
  try {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    page.portFailure.error = new Error("port is broken");
    shadow.started[0]?.emit({ kind: "handoff_ended", cause: "element_released" });
    assert.equal(bridge.controller.isHandingOff, false);
    assert.equal(debugLines.length, 1);
    page.portFailure.error = null;
    const before = isolated.received.length;
    shadow.started[0]?.emit({ kind: "shadow_alive" });
    await isolated.settle();
    assert.equal(isolated.received.length, before, "the background learns from the silence of the pulse");
  } finally {
    isolated.close();
  }
});

/** Тик часов плеера странице (через изолированный мир, как от фона). */
function clockTick(isolated: IsolatedSide, positionUs: number): void {
  isolated.send({ kind: "shadow", command: { kind: "clock", position_us: positionUs, state: "playing", rate: 1000 } });
}

test("real shadow, YouTube order on the handed element: the new MediaSource flows live as a new stream, no handoff_ended", async () => {
  const harness = startRealShadowHarness();
  const { page, bridge } = harness;
  const { video } = playingPage(harness);
  await withChannel(harness, async (isolated) => {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    await isolated.settle();
    isolated.received.length = 0;

    // YouTube перед рекламой: новый MediaSource → адрес → srcObject = null (src уже снят) → src = адрес.
    const adSource = new page.FakeMediaSource();
    const adUrl = page.FakeURL.createObjectURL(adSource);
    video.removeAttribute("src");
    video.srcObject = null;
    video.src = adUrl;
    video.testAttachedMediaSource = adSource;
    adSource.testOpen(); // sourceopen
    const adBuffer = adSource.addSourceBuffer(VIDEO_MIME);
    adBuffer.appendBuffer(VIDEO_MP4.bytes().slice(0, 733));
    await isolated.waitFor(2);
    // Плеер продолжает слать часы: тень жива и отвечает пульсом.
    clockTick(isolated, 2_953_000_000);
    await isolated.waitFor(3);
    await isolated.settle();
    assert.deepEqual(
      isolated.received.map((message) => (message.kind === "mse" ? message.event : message.kind === "bytes" ? [message.media_source, message.bytes.length] : message)),
      [{ kind: "source_buffer_added", media_source: "ms-2", source_buffer: 0, mime_type: VIDEO_MIME }, ["ms-2", 733], { kind: "shadow_alive" }],
    );
    assert.equal(bridge.controller.isHandingOff, true);
    assert.equal(video.currentTime, 0, "the page sees the new source from 0, not the player's old position");
    assert.equal(video.paused, true);
  });
});

test("real shadow: a pulse send that throws ends the handoff, returns the element and the pulse stops", async () => {
  const harness = startRealShadowHarness();
  const { page, bridge, debugLines } = harness;
  const { video } = playingPage(harness);
  const isolated = handshake(page);
  try {
    await describeAndBegin(isolated);
    await isolated.waitFor(9);
    clockTick(isolated, 250_000);
    await isolated.waitFor(10);
    assert.deepEqual(isolated.received.at(-1), { kind: "shadow_alive" });
    assert.equal(video.muted, false, "the page sees its own muted state");

    page.clock.advance(1_000);
    page.portFailure.error = new Error("DataCloneError: out of memory");
    clockTick(isolated, 1_250_000);
    await isolated.settle();
    assert.equal(bridge.controller.isHandingOff, false, "the handoff ended at the page");
    assert.ok(debugLines.length >= 1, "the failed send left a trace");
    assert.equal(video.paused, false, "the element went back to the page, playing as the shadow did");

    page.portFailure.error = null;
    const before = isolated.received.length;
    for (let tick = 0; tick < 3; tick += 1) {
      page.clock.advance(1_000);
      clockTick(isolated, 2_250_000 + tick * 1_000_000);
    }
    await isolated.settle();
    assert.equal(isolated.received.length, before, "no pulse after the end: the background learns within its silence limit");
  } finally {
    isolated.close();
  }
});
