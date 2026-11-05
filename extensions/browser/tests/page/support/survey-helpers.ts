// Общие помощники тестов опроса кандидатов и DRM: опрос через настоящий канал участка 1 и
// страница с играющим MSE.
import assert from "node:assert/strict";
import { after } from "node:test";
import type { CandidatesReply, VideoCandidate } from "../../../src/shared/handoff-contract.ts";
import { parseCandidatesReply } from "../../../src/shared/page-message-validation.ts";
import { handshake, type BridgeHarness, type IsolatedSide } from "./bridge-harness.ts";
import { VIDEO_MP4 } from "./media-samples.ts";

export const SURVEY_VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';

/** Канал опроса на страницу: рукопожатие принимается один раз, поэтому повторные опросы идут по тому же каналу. */
const surveyChannels = new WeakMap<object, IsolatedSide>();
/** Открытые каналы опроса: закрываются по концу файла тестов, иначе порты держат процесс. */
const openSurveyChannels: IsolatedSide[] = [];
after(() => {
  for (const channel of openSurveyChannels) channel.close();
});

/**
 * Спросить кандидатов у MAIN-мира по настоящему каналу; ответ обязан пройти проверку изолированного
 * мира. Можно звать много раз за тест. Каналы закрываются сами по концу файла тестов.
 */
export async function surveyCandidates(harness: Pick<BridgeHarness, "page">): Promise<VideoCandidate[]> {
  let isolated = surveyChannels.get(harness.page);
  if (isolated === undefined) {
    isolated = handshake(harness.page);
    openSurveyChannels.push(isolated);
    surveyChannels.set(harness.page, isolated);
  }
  return describeVia(isolated);
}

/** То же на уже открытом канале (когда тест держит его для передачи). */
export async function describeVia(isolated: IsolatedSide): Promise<VideoCandidate[]> {
  const before = isolated.received.length;
  isolated.send({ kind: "describe_candidates", request_id: before + 1 });
  await isolated.waitFor(before + 1);
  const reply = isolated.received[before] as CandidatesReply;
  assert.equal(reply.kind, "candidates");
  const accepted = parseCandidatesReply(JSON.parse(JSON.stringify(reply)));
  assert.ok(accepted !== undefined, "the isolated world accepts the survey reply");
  return reply.candidates as VideoCandidate[];
}

/** Видео с живым MSE (init + один кусок), играющее, 640x360 в окне. */
export function playingMseVideo(harness: Pick<BridgeHarness, "page">) {
  const { page } = harness;
  const video = page.addVideo();
  video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
  const mediaSource = page.attachMediaSource(video);
  mediaSource.duration = 60;
  const videoBuffer = mediaSource.addSourceBuffer(SURVEY_VIDEO_MIME);
  videoBuffer.appendBuffer(VIDEO_MP4.bytes().slice());
  video.testSetState({ paused: false, currentTime: 0.25 });
  return { video, mediaSource, videoBuffer };
}
