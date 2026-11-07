// Нативный HLS как передаваемая форма (сессия 10a2, пакет P2): вердикт и выбор видео для записи (VOD)
// и для живого эфира (длительность Infinity, который в 10a2 по решению 231 не передаётся).
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

  it("играющий эфир (длительности нет) — неподдержанная форма native_hls", () => {
    const live = hlsVideo(1, { duration_s: null });
    assert.equal(classifyVideo(live), "unsupported");
    assert.equal(unsupportedFormOf(live), "native_hls");
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

  it("живой эфир — not_supported_yet{native_hls}, а не «нечего передавать»", () => {
    const live = hlsVideo(1, { duration_s: null });
    assert.deepEqual(chooseVideo(survey(live)), { kind: "not_supported_yet", form: "native_hls" });
    assert.deepEqual(chooseMainVideo(survey(live)), { kind: "not_supported_yet", form: "native_hls" });
  });

  it("запись рядом с эфиром: передаётся запись", () => {
    const choice = chooseVideo(survey(hlsVideo(1, { duration_s: null }), hlsVideo(2)));
    assert.equal(choice.kind === "hand_off" && choice.video.element, 2);
  });

  it("незапущенный адрес плейлиста — «нечего передавать»", () => {
    assert.deepEqual(chooseVideo(survey(hlsVideo(1, { playing: false, duration_s: null }))), { kind: "nothing" });
  });

  it("выбор человека: запись передаётся ролью film, эфир даёт not_supported_yet{native_hls}", () => {
    const ref = { frame_id: 0, document: "doc-top", element: 1 };
    const vod = chooseRequestedVideo(survey(hlsVideo(1)), ref);
    assert.equal(vod.kind === "hand_off" && vod.video.role, "film");
    const live = chooseRequestedVideo(survey(hlsVideo(1, { duration_s: null })), ref);
    assert.deepEqual(live, { kind: "not_supported_yet", form: "native_hls" });
    const unstarted = chooseRequestedVideo(survey(hlsVideo(1, { playing: false, duration_s: null })), ref);
    assert.deepEqual(unstarted, { kind: "nothing" });
  });
});
