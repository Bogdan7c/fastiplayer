// Новые факты кандидата (09c): длительность, звук, цикл, размер кадра, хосты, MIME, видимость.
// Всё идёт через настоящий опрос по каналу участка 1 и проходит проверку изолированного мира.
import assert from "node:assert/strict";
import { describe, it, test } from "node:test";

import { frameDimensionPx, finiteDurationSeconds, hostOfHttpUrl } from "../../src/page/candidate-facts.ts";
import { MAX_SOURCE_MIME_TYPES } from "../../src/shared/page-message-validation.ts";
import { startHarness } from "./support/bridge-harness.ts";
import { createFakePage, type FakePage } from "./support/fake-page.ts";
import { PAGE_FACTS_OF_UNKNOWN_DURATION_VIDEO } from "./support/page-candidate-facts.ts";
import { playingMseVideo, SURVEY_VIDEO_MIME, surveyCandidates } from "./support/survey-helpers.ts";

const AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"';

test("survey reports every fact of an ordinary playing video", async () => {
  const harness = startHarness();
  const { video, mediaSource } = playingMseVideo(harness);
  mediaSource.addSourceBuffer(AUDIO_MIME);
  video.testDuration = 5400.25;
  const [candidate] = await surveyCandidates(harness);
  assert.deepEqual(candidate, {
    ...PAGE_FACTS_OF_UNKNOWN_DURATION_VIDEO,
    document: harness.bridge.documentTag,
    element: 1,
    media: { kind: "media_source", in_worker: false },
    playing: true,
    visible_area_px: 640 * 360,
    duration_s: 5400.25,
    source_mime_types: [SURVEY_VIDEO_MIME, AUDIO_MIME],
  });
});

describe("duration_s", () => {
  for (const [description, seconds, expected] of [
    ["finite", 12.5, 12.5],
    ["zero", 0, 0],
    ["not yet known (NaN)", Number.NaN, null],
    ["live (Infinity)", Infinity, null],
  ] as const) {
    it(description, async () => {
      const harness = startHarness();
      const { video } = playingMseVideo(harness);
      video.testDuration = seconds;
      assert.equal((await surveyCandidates(harness))[0]?.duration_s, expected);
    });
  }
});

describe("muted, loop, frame size", () => {
  it("muted is true for the real muted flag and for volume 0, false otherwise", async () => {
    const harness = startHarness();
    const { video } = playingMseVideo(harness);
    assert.equal((await surveyCandidates(harness))[0]?.muted, false);
    video.muted = true;
    assert.equal((await surveyCandidates(harness))[0]?.muted, true, "muted flag");
    video.muted = false;
    video.volume = 0;
    assert.equal((await surveyCandidates(harness))[0]?.muted, true, "volume 0");
    video.volume = 0.4;
    assert.equal((await surveyCandidates(harness))[0]?.muted, false);
  });

  it("loop follows the element", async () => {
    const harness = startHarness();
    const { video } = playingMseVideo(harness);
    assert.equal((await surveyCandidates(harness))[0]?.loop, false);
    video.loop = true;
    assert.equal((await surveyCandidates(harness))[0]?.loop, true);
  });

  it("frame size is videoWidth x videoHeight of the element", async () => {
    const harness = startHarness();
    playingMseVideo(harness);
    const [candidate] = await surveyCandidates(harness);
    assert.deepEqual([candidate?.video_width_px, candidate?.video_height_px], [640, 360]);
  });
});

describe("hosts and MIME types", () => {
  it("frame_host is the document's hostname; a file reports the host of its address, not the page's", async () => {
    const harness = startHarness();
    const video = harness.page.addVideo();
    video.src = "https://cdn.video-host.example:8443/movies/a.mp4?sig=1";
    const [candidate] = await surveyCandidates(harness);
    assert.equal(candidate?.media.kind, "file");
    assert.equal(candidate?.frame_host, "site.example");
    assert.equal(candidate?.file_host, "cdn.video-host.example");
    assert.deepEqual(candidate?.source_mime_types, []);
  });

  it("file_host is null for a media source and for a form that is not a file", async () => {
    const harness = startHarness();
    playingMseVideo(harness);
    const hls = harness.page.addVideo();
    hls.src = "https://cdn.example/live.m3u8";
    const candidates = await surveyCandidates(harness);
    assert.deepEqual(candidates.map((item) => item.file_host), [null, null]);
  });

  it("MIME types follow changeType and never exceed the contract limit", async () => {
    const harness = startHarness();
    const { mediaSource, videoBuffer } = playingMseVideo(harness);
    videoBuffer.changeType('video/mp4; codecs="hvc1.1.6.L93"');
    for (let index = 1; index < MAX_SOURCE_MIME_TYPES + 3; index += 1) mediaSource.addSourceBuffer(AUDIO_MIME);
    const mimeTypes = (await surveyCandidates(harness))[0]?.source_mime_types;
    assert.equal(mimeTypes?.length, MAX_SOURCE_MIME_TYPES);
    assert.equal(mimeTypes?.[0], 'video/mp4; codecs="hvc1.1.6.L93"');
  });
});

describe("pure helpers", () => {
  it("finiteDurationSeconds", () => {
    assert.deepEqual([3, 0, -1, Number.NaN, Infinity, "5", null].map(finiteDurationSeconds), [3, 0, null, null, null, null, null]);
  });
  it("frameDimensionPx", () => {
    assert.deepEqual([640, 640.9, -3, Number.NaN, Infinity].map(frameDimensionPx), [640, 640, 0, 0, 0]);
  });
  it("hostOfHttpUrl: host only, lowercase, no credentials or port; non-http gives null", () => {
    assert.equal(hostOfHttpUrl("https://User:pw@CDN.Example:8443/a.mp4"), "cdn.example");
    assert.equal(hostOfHttpUrl("http://[::1]:8080/a.mp4"), "[::1]");
    assert.equal(hostOfHttpUrl("http://192.168.1.5/a.mp4"), "192.168.1.5");
    assert.equal(hostOfHttpUrl("blob:https://site.example/1"), null);
    assert.equal(hostOfHttpUrl("data:video/mp4;base64,AAAA"), null);
    assert.equal(hostOfHttpUrl("https:///no-host"), null);
    assert.equal(hostOfHttpUrl(`https://${"a".repeat(300)}.example/x`), null);
  });
});

// --------------------------------------------------------------------------- видимость

type PathCase = {
  readonly description: string;
  /** Как спрятать плеер: что поставить на сам `<video>` и его предков (`root` — самый дальний предок). */
  readonly hide: (page: FakePage, chain: { root: InstanceType<FakePage["FakeElement"]>; middle: InstanceType<FakePage["FakeElement"]>; video: InstanceType<FakePage["FakeVideoElement"]> }) => void;
};

const HIDING_CASES: readonly PathCase[] = [
  { description: "visibility:hidden on the video", hide: (_page, { video }) => (video.testStyle.visibility = "hidden") },
  { description: "visibility:collapse on the video", hide: (_page, { video }) => (video.testStyle.visibility = "collapse") },
  { description: "opacity:0 on the video", hide: (_page, { video }) => (video.testStyle.opacity = "0") },
  { description: "display:none on the video", hide: (_page, { video }) => (video.testStyle.display = "none") },
  { description: "opacity:0 on an ancestor", hide: (_page, { middle }) => (middle.testStyle.opacity = "0") },
  { description: "display:none on the farthest ancestor", hide: (_page, { root }) => (root.testStyle.display = "none") },
];

/** Видео с двумя предками, видимое по геометрии: страница Ashdi прячет играющий плеер стилями. */
function videoInside(page: FakePage) {
  const root = new page.FakeElement("div");
  const middle = new page.FakeElement("div");
  middle.testParent = root;
  const video = page.addVideo();
  video.testParent = middle;
  video.rect = { left: 0, top: 0, right: 200, bottom: 100 };
  return { root, middle, video };
}

for (const [path, options] of [
  ["browser checkVisibility", { nativeCheckVisibility: true }],
  ["computed-style walk (engine without checkVisibility)", { nativeCheckVisibility: false }],
] as const) {
  describe(`visible_area_px, ${path}`, () => {
    it("a visible video keeps its geometric area", async () => {
      const harness = startHarness({ page: createFakePage(options) });
      videoInside(harness.page);
      assert.equal((await surveyCandidates(harness))[0]?.visible_area_px, 200 * 100);
    });

    for (const { description, hide } of HIDING_CASES) {
      it(`${description} gives area 0 (the hidden playing player must not win)`, async () => {
        const harness = startHarness({ page: createFakePage(options) });
        const chain = videoInside(harness.page);
        chain.video.testSetState({ paused: false });
        hide(harness.page, chain);
        const [candidate] = await surveyCandidates(harness);
        assert.equal(candidate?.visible_area_px, 0);
        assert.equal(candidate?.playing, true, "hidden does not mean paused: the fact is reported honestly");
      });
    }

    it("a visible child of a hidden-by-visibility ancestor is visible (visibility is inherited, the child opts back in)", async () => {
      const harness = startHarness({ page: createFakePage(options) });
      const chain = videoInside(harness.page);
      chain.middle.testStyle.visibility = "hidden"; // у ребёнка своё значение «visible» — видео не скрыто
      assert.equal((await surveyCandidates(harness))[0]?.visible_area_px, 200 * 100);
    });

    it("an off-screen video is 0 whatever its styles say", async () => {
      const harness = startHarness({ page: createFakePage(options) });
      const chain = videoInside(harness.page);
      chain.video.rect = { left: 5000, top: 0, right: 5200, bottom: 100 };
      assert.equal((await surveyCandidates(harness))[0]?.visible_area_px, 0);
    });

    it("a video in a shadow root is hidden by its HOST's opacity", async () => {
      const harness = startHarness({ page: createFakePage(options) });
      const { page } = harness;
      const host = new page.FakeElement("player-host");
      page.document.children.push(host);
      const inside = page.addVideoInShadowRoot(host.attachShadow({ mode: "open" }));
      inside.rect = { left: 0, top: 0, right: 100, bottom: 100 };
      assert.equal((await surveyCandidates(harness))[0]?.visible_area_px, 100 * 100, "control: visible");
      host.testStyle.opacity = "0";
      assert.equal((await surveyCandidates(harness))[0]?.visible_area_px, 0);
    });
  });
}

describe("survey cost with many elements", () => {
  it("browser path: one checkVisibility per on-screen video and no computed-style reads at all", async () => {
    const harness = startHarness({ page: createFakePage({ nativeCheckVisibility: true }) });
    for (let index = 0; index < 120; index += 1) {
      const video = harness.page.addVideo();
      video.rect = { left: 0, top: 0, right: 50, bottom: 50 };
    }
    const candidates = await surveyCandidates(harness);
    assert.equal(candidates.length, 120);
    assert.equal(harness.page.styleReads.count, 0, "the page's styles were never walked by script");
  });

  it("fallback path is linear: 200 videos under one 40-deep chain read each node once, not once per video", async () => {
    const harness = startHarness({ page: createFakePage({ nativeCheckVisibility: false }) });
    const { page } = harness;
    const DEPTH = 40;
    const VIDEOS = 200;
    let parent: InstanceType<typeof page.FakeElement> | null = null;
    for (let level = 0; level < DEPTH; level += 1) {
      const node: InstanceType<typeof page.FakeElement> = new page.FakeElement("div");
      node.testParent = parent;
      parent = node;
    }
    for (let index = 0; index < VIDEOS; index += 1) {
      const video = page.addVideo();
      video.testParent = parent;
      video.rect = { left: 0, top: 0, right: 50, bottom: 50 };
    }
    const candidates = await surveyCandidates(harness);
    assert.equal(candidates.length, VIDEOS);
    assert.ok(candidates.every((item) => item.visible_area_px === 50 * 50));
    // Каждый узел цепочки: display + opacity (2 чтения), каждое видео: visibility + display + opacity.
    const linearBound = 2 * DEPTH + 3 * VIDEOS + 10;
    assert.ok(page.styleReads.count <= linearBound, `${page.styleReads.count} style reads; linear bound ${linearBound}, naive would be ~${2 * DEPTH * VIDEOS}`);
  });
});
