// MAIN-мир и файл `<video src>` (сессия 09b): какая форма у источника элемента, что уходит фону
// в кандидатах и в каком порядке идёт начало передачи файла. Канал — настоящий MessageChannel.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { classifyMediaUrl } from "../../src/page/element-source.ts";
import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { MAX_FILE_SOURCE_URL_LENGTH, parseCandidatesReply, parsePageEvent } from "../../src/shared/page-message-validation.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { handshake, startHarness, type BridgeHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { createFakePage } from "./support/fake-page.ts";

const FILE_URL = "https://cdn.example/media/movie.mp4?token=a1";
const notMediaSource = (): boolean => false;

describe("форма источника по выбранному адресу", () => {
  it("http(s) — файл; m3u8 по пути или по типу `<source>` — нативный HLS (решение 185)", () => {
    assert.deepEqual(classifyMediaUrl(FILE_URL, null, notMediaSource), { kind: "file", url: FILE_URL });
    assert.deepEqual(classifyMediaUrl("HTTP://cdn.example/a.webm", "video/webm", notMediaSource), { kind: "file", url: "HTTP://cdn.example/a.webm" });
    assert.deepEqual(classifyMediaUrl("https://cdn.example/live/Master.M3U8?x=1#t", null, notMediaSource), { kind: "hls", url: "https://cdn.example/live/Master.M3U8?x=1#t" });
    assert.deepEqual(
      classifyMediaUrl("https://cdn.example/play?id=7", "application/vnd.apple.mpegURL; codecs=avc1", notMediaSource),
      { kind: "hls", url: "https://cdn.example/play?id=7" },
      "HLS без «.m3u8» в адресе узнаётся по типу выбранного <source>",
    );
    assert.deepEqual(classifyMediaUrl("https://cdn.example/m3u8.mp4", null, notMediaSource), { kind: "file", url: "https://cdn.example/m3u8.mp4" });
  });

  it("длинный адрес плейлиста, как и длинный адрес файла, — other_scheme (изолированный мир его отверг бы)", () => {
    const longPlaylist = `https://cdn.example/${"a".repeat(MAX_FILE_SOURCE_URL_LENGTH)}.m3u8`;
    assert.deepEqual(classifyMediaUrl(longPlaylist, null, notMediaSource), { kind: "unsupported", form: "other_scheme" });
  });

  it("blob обычного Blob, data: и чужие схемы — неподдержанные формы; blob MediaSource — пока нечего", () => {
    const blob = "blob:https://site.example/42";
    assert.deepEqual(classifyMediaUrl(blob, null, notMediaSource), { kind: "unsupported", form: "blob_file" });
    assert.deepEqual(classifyMediaUrl(blob, null, (url) => url === blob), { kind: "none" });
    assert.deepEqual(classifyMediaUrl("data:video/mp4;base64,AAAA", null, notMediaSource), { kind: "unsupported", form: "data_url" });
    assert.deepEqual(classifyMediaUrl("file:///home/movie.mp4", null, notMediaSource), { kind: "unsupported", form: "other_scheme" });
    assert.deepEqual(classifyMediaUrl("movie.mp4", null, notMediaSource), { kind: "unsupported", form: "other_scheme" });
  });

  it("адрес длиннее предела договора не выдаётся за файл (изолированный мир его бы отверг)", () => {
    const longUrl = `https://cdn.example/${"a".repeat(MAX_FILE_SOURCE_URL_LENGTH)}.mp4`;
    assert.deepEqual(classifyMediaUrl(longUrl, null, notMediaSource), { kind: "unsupported", form: "other_scheme" });
  });
});

/** Всё, что MAIN-мир отправил, обязано пройти проверку изолированного мира. */
function assertAcceptedByIsolatedWorld(harness: BridgeHarness): void {
  for (const { clonedBeforeSend } of harness.page.postedToPorts) {
    const message = clonedBeforeSend as { kind?: unknown };
    const accepted = message.kind === "candidates" ? parseCandidatesReply(message) : parsePageEvent(message);
    assert.ok(accepted !== undefined, `isolated world rejects ${JSON.stringify(message)}`);
  }
}

async function describeCandidates(isolated: IsolatedSide) {
  isolated.send({ kind: "describe_candidates", request_id: 1 });
  await isolated.waitFor(1);
  const reply = isolated.received[0];
  assert.ok(reply !== undefined && reply.kind === "candidates");
  isolated.received.length = 0;
  return reply.candidates;
}

describe("кандидаты и начало передачи файла", () => {
  it("кандидаты: файл, нативный HLS, MediaStream и пустой элемент — каждый своей формой", async () => {
    const page = createFakePage();
    class FakeMediaStream {}
    Object.assign(page.win, { MediaStream: FakeMediaStream });
    const harness = startHarness({ page });
    const file = page.addVideo();
    file.src = FILE_URL;
    const hls = page.addVideo();
    hls.src = "https://cdn.example/hls/master.m3u8";
    const camera = page.addVideo();
    camera.srcObject = new FakeMediaStream();
    page.addVideo();
    const isolated = handshake(page);
    try {
      const candidates = await describeCandidates(isolated);
      assert.deepEqual(
        candidates.map((candidate) => candidate.media),
        [{ kind: "file" }, { kind: "hls" }, { kind: "unsupported", form: "media_stream" }, { kind: "none" }],
      );
      await isolated.settle();
      assertAcceptedByIsolatedWorld(harness);
    } finally {
      isolated.close();
    }
  });

  it("begin_handoff файла: started → file_source{url} → page_seek{T0} → play, тень — в виде «файл»", async () => {
    const harness = startHarness();
    const video = harness.page.addVideo();
    video.src = FILE_URL;
    video.testSetState({ paused: false, currentTime: 12.5 });
    const isolated = handshake(harness.page);
    try {
      const [candidate] = await describeCandidates(isolated);
      assert.ok(candidate !== undefined);
      isolated.send({ kind: "begin_handoff", request_id: 2, document: candidate.document, element: candidate.element });
      await isolated.waitFor(4);
      assert.deepEqual(isolated.received, [
        { kind: "handoff_began", request_id: 2, outcome: "started" },
        { kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" },
        { kind: "page_seek", target_us: 12_500_000 },
        { kind: "page_playback", request: { kind: "play" } },
      ] satisfies PageEvent[]);
      assert.equal(harness.shadow.started.length, 1);
      assert.equal(harness.shadow.started[0]?.source, "file");
      assert.equal(harness.shadow.started[0]?.element, video);
      await isolated.settle();
      assertAcceptedByIsolatedWorld(harness);
    } finally {
      isolated.close();
    }
  });

  it("между опросом и командой источник стал неподдержанным — no_media, тень не ставится", async () => {
    const harness = startHarness();
    const video = harness.page.addVideo();
    video.src = FILE_URL;
    const isolated = handshake(harness.page);
    try {
      const [candidate] = await describeCandidates(isolated);
      assert.ok(candidate !== undefined);
      video.src = "https://cdn.example/hls/master.m3u8";
      isolated.send({ kind: "begin_handoff", request_id: 3, document: candidate.document, element: candidate.element });
      await isolated.waitFor(1);
      await isolated.settle();
      assert.deepEqual(isolated.received, [{ kind: "handoff_began", request_id: 3, outcome: "no_media" }]);
      assert.equal(harness.shadow.started.length, 0);
    } finally {
      isolated.close();
    }
  });
});

describe("начало передачи записи HLS (сессия 10a2)", () => {
  const PLAYLIST_URL = "https://cdn.example/vod/master.m3u8";

  it("begin_handoff записи: started → hls_source{url, referrer} → page_seek{T0} → play, тень — в виде «файл»", async () => {
    const harness = startHarness();
    const video = harness.page.addVideo();
    video.src = PLAYLIST_URL;
    video.testDuration = 600;
    video.testSetState({ paused: false, currentTime: 42 });
    const isolated = handshake(harness.page);
    try {
      const [candidate] = await describeCandidates(isolated);
      assert.deepEqual(candidate?.media, { kind: "hls" });
      assert.equal(candidate?.duration_s, 600);
      isolated.send({ kind: "begin_handoff", request_id: 2, document: candidate!.document, element: candidate!.element });
      await isolated.waitFor(4);
      assert.deepEqual(isolated.received.slice(-4), [
        { kind: "handoff_began", request_id: 2, outcome: "started" },
        { kind: "hls_source", url: PLAYLIST_URL, referrer_origin: null, referrer_policy: "no-referrer", stream: "vod" },
        { kind: "page_seek", target_us: 42_000_000 },
        { kind: "page_playback", request: { kind: "play" } },
      ] satisfies PageEvent[]);
      assert.equal(harness.shadow.started.length, 1);
      assert.equal(harness.shadow.started[0]?.source, "file", "VOD HLS под тенью ведёт себя как файл (решения 186/187)");
      assert.equal(harness.shadow.started[0]?.element, video);
      await isolated.settle();
      assertAcceptedByIsolatedWorld(harness);
    } finally {
      isolated.close();
    }
  });

  it("эфир (duration Infinity, 10a3): hls_source{stream: live}, затем T0 и play; тень — вид «эфир»", async () => {
    const harness = startHarness();
    const video = harness.page.addVideo();
    video.src = PLAYLIST_URL;
    video.testDuration = Number.POSITIVE_INFINITY;
    video.testSetState({ paused: false, currentTime: 7.5 });
    const isolated = handshake(harness.page);
    try {
      const [candidate] = await describeCandidates(isolated);
      assert.equal(candidate?.duration_s, null, "у эфира длительности нет");
      isolated.send({ kind: "begin_handoff", request_id: 4, document: candidate!.document, element: candidate!.element });
      await isolated.waitFor(4);
      assert.deepEqual(isolated.received.slice(-4), [
        { kind: "handoff_began", request_id: 4, outcome: "started" },
        { kind: "hls_source", url: PLAYLIST_URL, referrer_origin: null, referrer_policy: "no-referrer", stream: "live" },
        { kind: "page_seek", target_us: 7_500_000 },
        { kind: "page_playback", request: { kind: "play" } },
      ] satisfies PageEvent[]);
      assert.equal(harness.shadow.started[0]?.source, "live_hls");
      await isolated.settle();
      assertAcceptedByIsolatedWorld(harness);
    } finally {
      isolated.close();
    }
  });

  it("между опросом и командой метаданные пропали (NaN): no_media, тень не ставится, hls_source не уходит", async () => {
    const harness = startHarness();
    const video = harness.page.addVideo();
    video.src = PLAYLIST_URL;
    video.testDuration = 600;
    const isolated = handshake(harness.page);
    try {
      const [candidate] = await describeCandidates(isolated);
      video.testDuration = Number.NaN;
      isolated.send({ kind: "begin_handoff", request_id: 3, document: candidate!.document, element: candidate!.element });
      await isolated.waitFor(1);
      await isolated.settle();
      assert.deepEqual(isolated.received, [{ kind: "handoff_began", request_id: 3, outcome: "no_media" }]);
      assert.equal(harness.shadow.started.length, 0);
    } finally {
      isolated.close();
    }
  });
});

describe("проверка формы file_source изолированным миром", () => {
  it("принимает абсолютный http(s) без логина; отвергает прочее", () => {
    assert.deepEqual(parsePageEvent({ kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" }), { kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer" });
    const rejected: unknown[] = [
      { kind: "file_source", url: "ftp://cdn.example/a.mp4", referrer_origin: null, referrer_policy: "no-referrer" },
      { kind: "file_source", url: "blob:https://site.example/1", referrer_origin: null, referrer_policy: "no-referrer" },
      { kind: "file_source", url: "/relative.mp4", referrer_origin: null, referrer_policy: "no-referrer" },
      { kind: "file_source", url: "https://user:pw@cdn.example/a.mp4", referrer_origin: null, referrer_policy: "no-referrer" },
      { kind: "file_source", url: "https://cdn.example/a b.mp4", referrer_origin: null, referrer_policy: "no-referrer" },
      { kind: "file_source", url: `https://cdn.example/${"a".repeat(MAX_FILE_SOURCE_URL_LENGTH)}`, referrer_origin: null, referrer_policy: "no-referrer" },
      { kind: "file_source", url: 7, referrer_origin: null, referrer_policy: "no-referrer" },
      { kind: "file_source", url: FILE_URL, referrer_origin: null, referrer_policy: "no-referrer", extra: 1 },
    ];
    for (const message of rejected) assert.equal(parsePageEvent(message), undefined, JSON.stringify(message).slice(0, 80));
  });

  it("кандидат: форма источника строго по договору", () => {
    const base = { ...DEFAULT_CANDIDATE_FACTS, document: "doc1", element: 1, playing: false, visible_area_px: 0 };
    const accepted = parseCandidatesReply({ kind: "candidates", request_id: 1, candidates: [{ ...base, media: { kind: "unsupported", form: "data_url" } }] });
    assert.deepEqual(accepted?.candidates[0]?.media, { kind: "unsupported", form: "data_url" });
    for (const media of [{ kind: "unsupported", form: "flash" }, { kind: "file", url: FILE_URL }, { kind: "unsupported" }, "file", true]) {
      assert.equal(parseCandidatesReply({ kind: "candidates", request_id: 1, candidates: [{ ...base, media }] }), undefined, JSON.stringify(media));
    }
    assert.equal(
      parseCandidatesReply({ kind: "candidates", request_id: 1, candidates: [{ ...base, has_live_media: true }] }),
      undefined,
      "старая форма 09a больше не принимается",
    );
  });
});
