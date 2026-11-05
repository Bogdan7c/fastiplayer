// DRM-метки MAIN-мира (09c, инвариант 4): что считается защитой, что не считается, что обёртка не
// меняет для страницы, как защита останавливает передачу и освобождает память журнала.
import assert from "node:assert/strict";
import { test } from "node:test";

import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { parsePageEvent } from "../../src/shared/page-message-validation.ts";
import { handshake, startHarness, startRealShadowHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { createFakePage } from "./support/fake-page.ts";
import { VIDEO_MP4 } from "./support/media-samples.ts";
import { describeVia, playingMseVideo, SURVEY_VIDEO_MIME, surveyCandidates } from "./support/survey-helpers.ts";

const KEYS = { keySystem: "org.w3.clearkey" };

/** Дать микрозадачам (наблюдатель промиса `setMediaKeys`) отработать. */
async function settleMicrotasks(): Promise<void> {
  for (let round = 0; round < 4; round += 1) await Promise.resolve();
}

// --------------------------------------------------------------------------- что метит

test("setMediaKeys with keys marks the element — and the mark outlives setMediaKeys(null)", async () => {
  const harness = startHarness();
  const { video } = playingMseVideo(harness);
  const { drmMarks } = harness.bridge;
  assert.equal(drmMarks.isProtected(video), false, "a clean element is not protected");

  await video.setMediaKeys(KEYS);
  await settleMicrotasks();
  assert.equal(drmMarks.isProtected(video), true);
  assert.equal(video.setMediaKeysCalls, 1, "the browser's own setMediaKeys ran exactly once");

  // Страница «сняла» ключи, чтобы выглядеть чистой: метка остаётся (сам факт защиты уже был).
  await video.setMediaKeys(null);
  await settleMicrotasks();
  assert.equal(video.mediaKeys, null, "the element really has no keys now");
  assert.equal(drmMarks.isProtected(video), true, "the mark does not depend on the live keys");
  assert.equal((await surveyCandidates(harness))[0]?.drm, true);
});

test("setMediaKeys(null) alone does not mark", async () => {
  const harness = startHarness();
  const { video } = playingMseVideo(harness);
  await video.setMediaKeys(null);
  await settleMicrotasks();
  assert.equal(harness.bridge.drmMarks.isProtected(video), false);
  assert.equal((await surveyCandidates(harness))[0]?.drm, false);
});

test("a failed setMediaKeys (rejected promise or synchronous throw) does not mark and the page gets its own error", async () => {
  const harness = startHarness();
  const { video } = playingMseVideo(harness);

  const rejection = new Error("NotSupportedError: key system");
  video.testRejectNextSetMediaKeys = rejection;
  await assert.rejects(video.setMediaKeys(KEYS), (error: unknown) => error === rejection);
  await settleMicrotasks();
  assert.equal(harness.bridge.drmMarks.isProtected(video), false, "a rejected promise is not a protection");

  const thrown = new TypeError("bad argument");
  video.testThrowNextSetMediaKeys = thrown;
  assert.throws(() => video.setMediaKeys(KEYS), (error: unknown) => error === thrown, "the page gets exactly its own exception");
  await settleMicrotasks();
  assert.equal(harness.bridge.drmMarks.isProtected(video), false, "an exception is not a protection");
});

test("the wrapper hands the page the very promise the browser returned, and looks like the original", async () => {
  const harness = startHarness();
  const { video } = playingMseVideo(harness);
  const returned = video.setMediaKeys(KEYS);
  assert.equal(returned, video.lastSetMediaKeysPromise, "same promise object, not a derived one");
  await returned;
  const mediaPrototype = Object.getPrototypeOf(harness.page.FakeVideoElement.prototype) as object;
  const wrapped = Object.getOwnPropertyDescriptor(mediaPrototype, "setMediaKeys")?.value as ((keys: object | null) => unknown) | undefined;
  assert.equal(wrapped?.name, "setMediaKeys");
  assert.equal(wrapped?.length, 1);
});

test("11 requestMediaKeySystemAccess probes (Shaka) mark nothing and the function is not touched", async () => {
  const page = createFakePage();
  const probes: string[] = [];
  const probe = (keySystem: string): Promise<never> => {
    probes.push(keySystem);
    return Promise.reject(new Error("NotSupportedError"));
  };
  (page.win as unknown as { navigator: Record<string, unknown> }).navigator["requestMediaKeySystemAccess"] = probe;
  const harness = startHarness({ page });
  const { video } = playingMseVideo(harness);

  for (let index = 0; index < 11; index += 1) {
    await (page.win as unknown as { navigator: { requestMediaKeySystemAccess: typeof probe } }).navigator.requestMediaKeySystemAccess(`com.example.cdm.${index}`).catch(() => undefined);
  }
  assert.equal(probes.length, 11, "all probes reached the browser");
  assert.equal(
    (page.win as unknown as { navigator: { requestMediaKeySystemAccess: unknown } }).navigator.requestMediaKeySystemAccess,
    probe,
    "we did not replace requestMediaKeySystemAccess",
  );
  assert.equal(harness.bridge.drmMarks.isProtected(video), false);
  assert.equal((await surveyCandidates(harness))[0]?.drm, false, "a probing page is not a protected page");
});

test("a trusted encrypted event marks the element; a page-made one does not", async () => {
  const harness = startHarness();
  const { page } = harness;
  const { video } = playingMseVideo(harness);
  const forged = new Event("encrypted"); // dispatchEvent страницы: isTrusted === false
  video.dispatchEvent(forged);
  assert.equal(harness.bridge.drmMarks.isProtected(video), false, "an untrusted event proves nothing");

  video.dispatchEvent(page.browserEvent("encrypted"));
  assert.equal(harness.bridge.drmMarks.isProtected(video), true);
  assert.equal((await surveyCandidates(harness))[0]?.drm, true);
});

test("encrypted inside open and closed shadow roots marks the video there", async () => {
  const harness = startHarness();
  const { page } = harness;
  const openHost = new page.FakeElement("open-player");
  page.document.children.push(openHost);
  const openVideo = page.addVideoInShadowRoot(openHost.attachShadow({ mode: "open" }));
  const closedHost = new page.FakeElement("closed-player");
  page.document.children.push(closedHost);
  const closedVideo = page.addVideoInShadowRoot(closedHost.attachShadow({ mode: "closed" }));
  const cleanVideo = page.addVideo();

  openVideo.dispatchEvent(page.browserEvent("encrypted"));
  closedVideo.dispatchEvent(page.browserEvent("encrypted"));
  const { drmMarks } = harness.bridge;
  assert.equal(drmMarks.isProtected(openVideo), true, "open shadow root");
  assert.equal(drmMarks.isProtected(closedVideo), true, "closed shadow root");
  assert.equal(drmMarks.isProtected(cleanVideo), false, "a video in the document is not marked by its neighbours");
});

test("an element that already has keys at survey time is reported as drm without any mark", async () => {
  const page = createFakePage();
  // Оригинал до установки наших обёрток: им ключи ставит «код, которого мы не видели».
  const mediaPrototype = Object.getPrototypeOf(page.FakeVideoElement.prototype) as { setMediaKeys: (keys: object | null) => Promise<void> };
  const unobservedSetMediaKeys = mediaPrototype.setMediaKeys;
  const harness = startHarness({ page });
  const { video } = playingMseVideo(harness);
  await Reflect.apply(unobservedSetMediaKeys, video, [KEYS]);
  assert.equal(video.mediaKeys, KEYS);
  assert.equal((await surveyCandidates(harness))[0]?.drm, true);
});

// --------------------------------------------------------------------------- передача

async function beginOn(isolated: IsolatedSide, requestId: number) {
  const candidate = (await describeVia(isolated)).find((item) => item.media.kind === "media_source");
  assert.ok(candidate !== undefined);
  isolated.received.length = 0;
  isolated.send({ kind: "begin_handoff", request_id: requestId, document: candidate.document, element: candidate.element });
  return candidate;
}

test("begin_handoff on a protected element is refused as `protected`: no stream, no shadow, nothing else is sent", async () => {
  const harness = startHarness();
  const { video } = playingMseVideo(harness);
  await video.setMediaKeys(KEYS);
  await settleMicrotasks();
  const isolated = handshake(harness.page);
  try {
    await beginOn(isolated, 7);
    await isolated.waitFor(1);
    await isolated.settle();
    assert.deepEqual(isolated.received, [{ kind: "handoff_began", request_id: 7, outcome: "protected" }]);
    assert.ok(parsePageEvent(isolated.received[0]) !== undefined, "the isolated world accepts the refusal");
    assert.equal(harness.shadow.started.length, 0, "the shadow never started");
    assert.equal(harness.bridge.controller.isHandingOff, false);
    assert.equal(video.paused, false, "the page keeps playing its own video");
  } finally {
    isolated.close();
  }
});

test("a protected video does not block handing off another, clean one", async () => {
  const harness = startHarness();
  const { page } = harness;
  const { video: protectedVideo } = playingMseVideo(harness);
  await protectedVideo.setMediaKeys(KEYS);
  await settleMicrotasks();
  const clean = page.addVideo();
  clean.rect = { left: 0, top: 0, right: 100, bottom: 100 };
  const cleanSource = page.attachMediaSource(clean);
  cleanSource.addSourceBuffer(SURVEY_VIDEO_MIME).appendBuffer(VIDEO_MP4.bytes().slice());
  const isolated = handshake(page);
  try {
    const candidates = await describeVia(isolated);
    assert.deepEqual(candidates.map((item) => item.drm), [true, false]);
    const cleanCandidate = candidates[1];
    assert.ok(cleanCandidate !== undefined);
    isolated.received.length = 0;
    isolated.send({ kind: "begin_handoff", request_id: 3, document: cleanCandidate.document, element: cleanCandidate.element });
    await isolated.waitFor(1);
    assert.deepEqual(isolated.received[0], { kind: "handoff_began", request_id: 3, outcome: "started" });
    assert.equal(harness.shadow.started.length, 1);
  } finally {
    isolated.close();
  }
});

test("DRM arriving mid-handoff ends it honestly: handoff_ended, the shadow is lifted at its own position, the element is the page's again", async () => {
  const harness = startRealShadowHarness();
  const { page, bridge } = harness;
  const { video } = playingMseVideo(harness);
  const isolated = handshake(page);
  try {
    await beginOn(isolated, 2);
    await isolated.waitFor(5);
    assert.equal(bridge.controller.isHandingOff, true, "the handoff is running");
    isolated.received.length = 0;

    await video.setMediaKeys(KEYS); // защита появилась ПОСЛЕ начала передачи
    await settleMicrotasks();
    await isolated.settle();
    const tail = isolated.received as PageEvent[];
    assert.equal(tail.at(-1)?.kind, "handoff_ended", "the background is told the handoff is over");
    assert.equal(tail.filter((message) => message.kind === "handoff_ended").length, 1, "told exactly once");
    assert.equal(bridge.controller.isHandingOff, false);
    assert.equal(video.paused, false, "the element is back with the page, still playing");

    // Повторная попытка на том же элементе — уже отказ по DRM.
    isolated.received.length = 0;
    const candidate = (await describeVia(isolated)).find((item) => item.drm);
    assert.ok(candidate !== undefined, "the survey now reports drm");
    isolated.received.length = 0;
    isolated.send({ kind: "begin_handoff", request_id: 9, document: candidate.document, element: candidate.element });
    await isolated.waitFor(1);
    assert.deepEqual(isolated.received[0], { kind: "handoff_began", request_id: 9, outcome: "protected" });
  } finally {
    isolated.close();
  }
});

test("encrypted on a ANOTHER element does not end the handoff of this one", async () => {
  const harness = startHarness();
  const { page, bridge } = harness;
  playingMseVideo(harness);
  const other = page.addVideo();
  const isolated = handshake(page);
  try {
    await beginOn(isolated, 2);
    await isolated.waitFor(1);
    other.dispatchEvent(page.browserEvent("encrypted"));
    await isolated.settle();
    assert.equal(bridge.controller.isHandingOff, true);
    assert.equal(harness.shadow.ended.length, 0);
  } finally {
    isolated.close();
  }
});

// --------------------------------------------------------------------------- память журнала

test("a MediaSource of a protected element releases its journal: nothing is retained, before and after the mark", async () => {
  const harness = startHarness();
  const { video, videoBuffer, mediaSource } = playingMseVideo(harness);
  const { tracker } = harness.bridge;
  const record = tracker.recordOf(mediaSource);
  assert.ok(record !== undefined && record !== null);
  assert.equal(record.journal.completeness, "complete");
  assert.ok(record.journal.replay().some((item) => item.kind === "bytes"), "control: before protection the journal keeps the bytes");

  await video.setMediaKeys(KEYS);
  await settleMicrotasks();
  assert.equal(record.journal.completeness, "incomplete", "the live media source released its journal at the mark");
  videoBuffer.appendBuffer(VIDEO_MP4.bytes().slice());
  assert.equal(record.journal.replay().filter((item) => item.kind === "bytes").length, 0, "no byte reference survives, old or new");
});

test("EME order: keys first, MediaSource attached later — its journal never retains bytes", async () => {
  const harness = startHarness();
  const { page } = harness;
  const video = page.addVideo();
  await video.setMediaKeys(KEYS);
  await settleMicrotasks();
  const mediaSource = page.attachMediaSource(video);
  const sourceBuffer = mediaSource.addSourceBuffer(SURVEY_VIDEO_MIME);
  sourceBuffer.appendBuffer(VIDEO_MP4.bytes().slice());
  const record = harness.bridge.tracker.recordOf(mediaSource);
  assert.ok(record !== undefined && record !== null);
  assert.equal(record.journal.completeness, "incomplete");
  assert.equal(record.journal.replay().filter((item) => item.kind === "bytes").length, 0);

  // Контроль: тот же поток на чистом элементе копит ссылки.
  const clean = page.addVideo();
  const cleanSource = page.attachMediaSource(clean);
  cleanSource.addSourceBuffer(SURVEY_VIDEO_MIME).appendBuffer(VIDEO_MP4.bytes().slice());
  const cleanRecord = harness.bridge.tracker.recordOf(cleanSource);
  assert.ok(cleanRecord !== undefined && cleanRecord !== null);
  assert.ok(cleanRecord.journal.replay().some((item) => item.kind === "bytes"));
});
