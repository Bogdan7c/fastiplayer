// Общие шаги тестов MAIN-мира 11a: страница с живым MSE-видео, опрос и начало передачи по личному порту.
import assert from "node:assert/strict";
import type { BridgeHarness, IsolatedSide } from "./bridge-harness.ts";
import { VIDEO_MP4 } from "./media-samples.ts";

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';

/** Играющее MSE-видео: один SourceBuffer и init-сегмент. */
export function addPlayingMseVideo(harness: Pick<BridgeHarness, "page">) {
  const { page } = harness;
  const video = page.addVideo();
  video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
  const mediaSource = page.attachMediaSource(video);
  mediaSource.duration = 0.4;
  const buffer = mediaSource.addSourceBuffer(VIDEO_MIME);
  buffer.appendBuffer(VIDEO_MP4.bytes().slice());
  video.testSetState({ paused: false, currentTime: 0.25 });
  return { video, mediaSource };
}

/** Спросить кандидатов и начать передачу первого кандидата нужной формы; вернуть его ключ. */
export async function describeAndBegin(isolated: IsolatedSide, media: "media_source" | "file" = "media_source"): Promise<number> {
  const before = isolated.received.length;
  isolated.send({ kind: "describe_candidates", request_id: 1 });
  await isolated.waitFor(before + 1);
  const reply = isolated.received[before];
  assert.ok(reply !== undefined && reply.kind === "candidates");
  const chosen = reply.candidates.find((candidate) => candidate.media.kind === media);
  assert.ok(chosen !== undefined, `a ${media} candidate exists`);
  isolated.send({ kind: "begin_handoff", request_id: 2, document: chosen.document, element: chosen.element });
  await isolated.waitFor(before + 2);
  return chosen.element;
}

/** Часы плеера — тень и сторожа живут на этих тиках. */
export function playerClock(isolated: IsolatedSide, positionUs = 1_000_000): void {
  isolated.send({ kind: "shadow", command: { kind: "clock", position_us: positionUs, state: "playing", rate: 1000 } });
}
