// Нативный HLS как передаваемая форма (сессия 10a2, пакет P2): вердикт и выбор видео для записи (VOD)
// и для живого эфира (длительность Infinity). С 10a3 (решение 232) эфир — обычный кандидат: может ли плеер
// его играть, решает сеанс по версии протокола (hls-live-session.test.ts).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { chooseMainVideo, chooseRequestedVideo, chooseVideo } from "../../src/background/handoff/video-choice.ts";
import { classifyVideo, unsupportedFormOf } from "../../src/background/tab-model/video-verdict.ts";
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";

const AD_HOST = "video.unknown-ad-cdn.example";
const PLAYER_AREA = 296_000;

function hlsVideo(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return { document: "doc-top", element, media: { kind: "hls" }, playing: true, visible_area_px: PLAYER_AREA, ...DEFAULT_CANDIDATE_FACTS, file_host: "cdn.example", ...overrides };
}

const survey = (...candidates: VideoCandidate[]) => [{ frameId: 0, candidates }];

describe("вердикт нативного HLS", () => {
  it("адрес плейлиста до старта (не играет, длительности нет) — пустой плеер, как у Ashdi", () => {
    assert.equal(classifyVideo(hlsVideo(1, { playing: false, duration_s: null })), "empty");
  });

  it("играющая запись с известной длительностью — главное видео", () => {
    assert.equal(classifyVideo(hlsVideo(1)), "main");
  });

  it("играющий эфир (длительности нет) — главное видео, формы отказа у него нет (10a3)", () => {
    const live = hlsVideo(1, { duration_s: null });
    assert.equal(classifyVideo(live), "main");
    assert.equal(unsupportedFormOf(live), null);
  });

  it("эфир не реклама по длительности: без длительности не преролл и не короткий ролик даже рядом с ждущим фильмом", () => {
    const live = hlsVideo(1, { duration_s: null, file_host: AD_HOST });
    const waitingFilm = hlsVideo(2, { media: { kind: "media_source", in_worker: false }, playing: false, duration_s: 6381, file_host: null });
    assert.equal(classifyVideo(live, undefined, [waitingFilm]), "main");
    assert.equal(classifyVideo(hlsVideo(3, { duration_s: null, muted: true })), "main", "беззвучный эфир без цикла — не украшение");
  });

  it("у записи и у файла формы отказа нет", () => {
    assert.equal(unsupportedFormOf(hlsVideo(1)), null);
    assert.equal(unsupportedFormOf(hlsVideo(2, { media: { kind: "file" } })), null);
    assert.equal(unsupportedFormOf(hlsVideo(3, { media: { kind: "unsupported", form: "data_url" } })), "data_url");
  });

  it("плейлист с рекламного хоста — реклама", () => {
    assert.equal(classifyVideo(hlsVideo(1, { file_host: "ad.doubleclick.net" })), "ad");
  });

  it("короткий играющий HLS рядом с ждущим фильмом — преролл; одинокий — нет", () => {
    const clip = hlsVideo(1, { duration_s: 24, file_host: AD_HOST });
    const waitingFilm = hlsVideo(2, { media: { kind: "media_source", in_worker: false }, playing: false, duration_s: 6381, file_host: null });
    assert.equal(classifyVideo(clip, undefined, [waitingFilm]), "ad");
    assert.equal(classifyVideo(clip, undefined, []), "main");
  });

  it("беззвучный короткий цикл HLS — мусор, как и у файла", () => {
    assert.equal(classifyVideo(hlsVideo(1, { muted: true, loop: true })), "junk");
  });
});

describe("выбор видео: нативный HLS", () => {
  it("запись HLS передаётся; mayBeAd — только у короткого", () => {
    const choice = chooseVideo(survey(hlsVideo(1)));
    assert.deepEqual(choice, {
      kind: "hand_off",
      video: { frameId: 0, document: "doc-top", element: 1, frameHost: "site.example", fileHost: "cdn.example", role: "film", mayBeAd: false },
    });
    const short = chooseVideo(survey(hlsVideo(1, { duration_s: 40 })));
    assert.equal(short.kind === "hand_off" && short.video.mayBeAd, true);
  });

  it("при равной площади и длительности MSE предпочтительнее файла, файл — HLS", () => {
    const mediaSource = hlsVideo(1, { media: { kind: "media_source", in_worker: false }, file_host: null });
    const file = hlsVideo(2, { media: { kind: "file" } });
    const hls = hlsVideo(3);
    for (const [first, second, winner] of [[hls, file, 2], [hls, mediaSource, 1], [file, mediaSource, 1]] as const) {
      const choice = chooseMainVideo(survey(first, second));
      assert.equal(choice.kind === "hand_off" && choice.video.element, winner);
    }
  });

  it("живой эфир передаётся ролью film и без признака «может быть рекламой» (у эфира нет длительности)", () => {
    const live = hlsVideo(1, { duration_s: null });
    const expected = { kind: "hand_off", video: { frameId: 0, document: "doc-top", element: 1, frameHost: "site.example", fileHost: "cdn.example", role: "film", mayBeAd: false } };
    assert.deepEqual(chooseVideo(survey(live)), expected);
    assert.deepEqual(chooseMainVideo(survey(live)), expected);
  });

  it("запись рядом с эфиром той же площади: передаётся запись (неизвестная длительность — последняя)", () => {
    const choice = chooseVideo(survey(hlsVideo(1, { duration_s: null }), hlsVideo(2)));
    assert.equal(choice.kind === "hand_off" && choice.video.element, 2);
  });

  it("незапущенный адрес плейлиста — «нечего передавать»", () => {
    assert.deepEqual(chooseVideo(survey(hlsVideo(1, { playing: false, duration_s: null }))), { kind: "nothing" });
  });

  it("выбор человека: запись и эфир передаются ролью film, незапущенный плейлист — «нечего передавать»", () => {
    const ref = { frame_id: 0, document: "doc-top", element: 1 };
    const vod = chooseRequestedVideo(survey(hlsVideo(1)), ref);
    assert.equal(vod.kind === "hand_off" && vod.video.role, "film");
    const live = chooseRequestedVideo(survey(hlsVideo(1, { duration_s: null })), ref);
    assert.equal(live.kind === "hand_off" && live.video.role, "film");
    const unstarted = chooseRequestedVideo(survey(hlsVideo(1, { playing: false, duration_s: null })), ref);
    assert.deepEqual(unstarted, { kind: "nothing" });
  });
});
