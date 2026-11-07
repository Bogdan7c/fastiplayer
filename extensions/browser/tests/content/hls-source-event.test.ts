// Событие `hls_source` (сессия 10a2): проверка формы изолированным миром, бюджет и подмена происхождения.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createPageEventBudget, PAGE_EVENT_LIMITS_PER_SECOND } from "../../src/content/page-event-budget.ts";
import { parseCandidatesReply, parsePageEvent, MAX_FILE_SOURCE_URL_LENGTH } from "../../src/shared/page-message-validation.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";

const PLAYLIST = "https://cdn.example/vod/master.m3u8?token=a1";
const valid = { kind: "hls_source", url: PLAYLIST, referrer_origin: null, referrer_policy: "no-referrer", stream: "vod" };

describe("проверка hls_source изолированным миром", () => {
  it("принимает ту же форму, что file_source, и сохраняет вид события", () => {
    assert.deepEqual(parsePageEvent(valid), valid);
    const withOrigin = { ...valid, referrer_origin: "https://site.example", referrer_policy: "origin" };
    assert.deepEqual(parsePageEvent(withOrigin), withOrigin);
    const live = { ...valid, stream: "live" };
    assert.deepEqual(parsePageEvent(live), live, "эфир (10a3) — та же форма с видом live");
  });

  it("отвергает лишние и недостающие ключи, плохие адреса и политики", () => {
    const rejected: unknown[] = [
      { ...valid, extra: 1 },
      { kind: "hls_source", url: PLAYLIST },
      { ...valid, url: "ftp://cdn.example/a.m3u8" },
      { ...valid, url: "/relative.m3u8" },
      { ...valid, url: "https://user:pw@cdn.example/a.m3u8" },
      { ...valid, url: "https://cdn.example/a b.m3u8" },
      { ...valid, url: `https://cdn.example/${"a".repeat(MAX_FILE_SOURCE_URL_LENGTH)}.m3u8` },
      { ...valid, url: 7 },
      { ...valid, referrer_origin: "https://site.example/path" },
      { ...valid, referrer_policy: "leak-everything" },
      { kind: "hls_source", url: PLAYLIST, referrer_origin: null, referrer_policy: "no-referrer" },
      { ...valid, stream: "dvr" },
      { ...valid, stream: null },
    ];
    for (const message of rejected) assert.equal(parsePageEvent(message), undefined, JSON.stringify(message).slice(0, 90));
  });

  it("кандидат формы hls принимается строго и несёт хост плейлиста", () => {
    const base = { ...DEFAULT_CANDIDATE_FACTS, document: "doc1", element: 1, playing: false, visible_area_px: 0 };
    const accepted = parseCandidatesReply({ kind: "candidates", request_id: 1, candidates: [{ ...base, media: { kind: "hls" }, file_host: "cdn.example" }] });
    assert.deepEqual(accepted?.candidates[0]?.media, { kind: "hls" });
    assert.equal(accepted?.candidates[0]?.file_host, "cdn.example");
    const withAddress = { kind: "candidates", request_id: 1, candidates: [{ ...base, media: { kind: "hls", url: PLAYLIST } }] };
    assert.equal(parseCandidatesReply(withAddress), undefined, "адрес в кандидате не допускается");
    const mediaSourceWithHost = { kind: "candidates", request_id: 1, candidates: [{ ...base, media: { kind: "none" }, file_host: "cdn.example" }] };
    assert.equal(parseCandidatesReply(mediaSourceWithHost), undefined, "хост медиа есть только у файла и HLS");
  });
});

describe("бюджет hls_source", () => {
  it("свой вид бюджета с лимитом как у file_source: за секунду проходит лимит, дальше превышение", () => {
    assert.equal(PAGE_EVENT_LIMITS_PER_SECOND.hls_source, PAGE_EVENT_LIMITS_PER_SECOND.file_source);
    const budget = createPageEventBudget({ now: () => 0, limitsPerSecond: { ...PAGE_EVENT_LIMITS_PER_SECOND, hls_source: 2 }, replayEventCredit: 0 });
    assert.deepEqual([budget.admit("hls_source"), budget.admit("hls_source"), budget.admit("hls_source")], ["admitted", "admitted", "exceeded"]);
    assert.equal(budget.admit("file_source"), "admitted", "ведро file_source отдельное");
  });
});
