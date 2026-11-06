// Тексты попапа: исходы передачи, причины недоступной вкладки, строки списка видео (чистые функции).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { TabVideoRecord } from "../../src/background/tab-model/tab-snapshot.ts";
import type { HandOffTabOutcome, UnsupportedMediaForm } from "../../src/shared/handoff-contract.ts";
import type { SurveyUnavailableReason } from "../../src/shared/survey-contract.ts";
import { describeConsentQuestion, describeOutcome, describeUnavailable, describeUnsupportedForm } from "../../src/popup/outcome-text.ts";
import { describeVideoList, describeVideoRow, formatDuration, formatFrameSize } from "../../src/popup/video-row-text.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";

const ALL_FORMS: readonly UnsupportedMediaForm[] = ["native_hls", "blob_file", "data_url", "media_stream", "other_scheme", "untracked_worker_media_source"];

const ALL_OUTCOMES: readonly HandOffTabOutcome[] = [
  { kind: "started" },
  { kind: "no_video" },
  { kind: "not_supported_yet", form: "native_hls" },
  { kind: "protected" },
  { kind: "video_gone" },
  { kind: "local_address_refused" },
  { kind: "local_address_needs_consent", host: "nas.local" },
  { kind: "player_unavailable", reason: "player_not_responding" },
  { kind: "failed", reason: "internal:x" },
];

const textOf = (outcome: HandOffTabOutcome): string => {
  const notice = describeOutcome(outcome);
  return [notice.headline, ...notice.details].join(" ");
};

describe("тексты исходов", () => {
  it("каждый исход имеет непустой заголовок и своё имя случая", () => {
    const ids = ALL_OUTCOMES.map((outcome) => describeOutcome(outcome).id);
    assert.equal(new Set(ids).size, ALL_OUTCOMES.length, "имена случаев не повторяются");
    for (const outcome of ALL_OUTCOMES) assert.notEqual(describeOutcome(outcome).headline, "", outcome.kind);
  });

  it("no_video подсказывает нажать play и повторить", () => {
    assert.match(textOf({ kind: "no_video" }), /Press play on the page, then click the button again/);
  });

  it("protected говорит, что защищённое видео остаётся в браузере", () => {
    assert.match(textOf({ kind: "protected" }), /protected/i);
    assert.match(textOf({ kind: "protected" }), /stay in the browser/);
  });

  it("not_supported_yet: общая фраза и своё слово для каждой формы", () => {
    const words = ALL_FORMS.map((form) => {
      const text = textOf({ kind: "not_supported_yet", form });
      assert.match(text, /This video can't be handed to Fastiplayer yet/);
      assert.ok(text.includes(describeUnsupportedForm(form)), form);
      return describeUnsupportedForm(form);
    });
    assert.equal(new Set(words).size, ALL_FORMS.length, "слова форм различаются");
    assert.match(describeUnsupportedForm("native_hls"), /HLS/);
  });

  it("local_address_refused: заблокировано и можно сменить в настройках экстеншена", () => {
    const text = textOf({ kind: "local_address_refused" });
    assert.match(text, /Blocked/);
    assert.match(text, /extension settings/);
  });

  it("вопрос про локальный адрес называет хост и даёт Allow / Don't allow", () => {
    assert.deepEqual(describeConsentQuestion("nas.local"), {
      host: "nas.local",
      text: "This page wants the player to download from nas.local on your network. Allow?",
      allowLabel: "Allow",
      denyLabel: "Don't allow",
    });
  });

  it("player_unavailable, failed и video_gone говорят, что делать дальше", () => {
    assert.match(textOf({ kind: "player_unavailable", reason: "x" }), /connected/);
    assert.match(textOf({ kind: "failed", reason: "x" }), /Try again/);
    assert.match(textOf({ kind: "video_gone" }), /no longer on the page/);
  });

  it("внутренняя причина (reason) пользователю не показывается", () => {
    assert.ok(!textOf({ kind: "failed", reason: "internal:secret" }).includes("secret"));
    assert.ok(!textOf({ kind: "player_unavailable", reason: "player_not_responding" }).includes("player_not_responding"));
  });

  it("недоступная вкладка: страница браузера объяснена без передачи, остальные причины — свои тексты", () => {
    const reasons: SurveyUnavailableReason[] = ["tab_not_scriptable", "survey_timed_out", "survey_failed"];
    const heads = reasons.map((reason) => describeUnavailable(reason).headline);
    assert.equal(new Set(heads).size, 3, "у каждой причины свой заголовок");
    assert.match(describeUnavailable("tab_not_scriptable").details.join(" "), /Browser pages and extension stores/);
    assert.match(describeUnavailable("survey_timed_out").headline, /did not answer/);
  });
});

function record(overrides: Partial<TabVideoRecord> & { element: number }): TabVideoRecord {
  return {
    ...DEFAULT_CANDIDATE_FACTS,
    document: "doc-1",
    media: { kind: "media_source", in_worker: false },
    playing: false,
    visible_area_px: 1000,
    frame_id: 0,
    verdict: "main",
    ...overrides,
  };
}

describe("строки списка видео", () => {
  it("размер и длительность в человеческом виде; 0×0 и неизвестная длительность не показываются", () => {
    assert.equal(formatFrameSize(1920, 1080), "1920×1080");
    assert.equal(formatFrameSize(0, 0), null);
    assert.equal(formatFrameSize(1280, 0), null);
    assert.equal(formatDuration(5025), "1:23:45");
    assert.equal(formatDuration(187), "3:07");
    assert.equal(formatDuration(3600), "1:00:00");
    assert.equal(formatDuration(0), "0:00");
    assert.equal(formatDuration(null), null);
  });

  it("заголовок: у главного документа — заголовок вкладки, у iframe — хост фрейма", () => {
    const top = describeVideoRow(record({ element: 1 }), "Movie night", null);
    assert.equal(top.title, "Movie night");
    const embedded = describeVideoRow(record({ element: 2, frame_id: 4, frame_host: "player.example" }), "Movie night", null);
    assert.equal(embedded.title, "player.example");
    assert.equal(describeVideoRow(record({ element: 1 }), "  ", null).title, "This page");
    assert.equal(describeVideoRow(record({ element: 2, frame_id: 4, frame_host: "" }), "x", null).title, "Embedded player");
  });

  it("подробности собирают размер и длительность через точку", () => {
    const row = describeVideoRow(record({ element: 1, video_width_px: 1920, video_height_px: 1080, duration_s: 5025 }), "t", null);
    assert.equal(row.details, "1920×1080 · 1:23:45");
    const unknown = describeVideoRow(record({ element: 1, video_width_px: 0, video_height_px: 0, duration_s: null }), "t", null);
    assert.equal(unknown.details, "");
  });

  it("метки и выбираемость: ad?, DRM, not supported yet, in player; DRM, неподдержанное и видео в плеере — не кнопки", () => {
    const ad = describeVideoRow(record({ element: 1, verdict: "ad" }), "t", null);
    assert.deepEqual([ad.labels, ad.selectable], [["ad?"], true]);
    const drm = describeVideoRow(record({ element: 2, verdict: "protected" }), "t", null);
    assert.deepEqual([drm.labels, drm.selectable], [["DRM"], false]);
    const hls = describeVideoRow(record({ element: 3, verdict: "unsupported", media: { kind: "unsupported", form: "native_hls" } }), "t", null);
    assert.deepEqual([hls.labels, hls.selectable], [["not supported yet"], false]);
    const current = describeVideoRow(record({ element: 4 }), "t", { frame_id: 0, document: "doc-1", element: 4 });
    assert.deepEqual([current.labels, current.selectable], [["in player"], false]);
    const other = describeVideoRow(record({ element: 5 }), "t", { frame_id: 0, document: "doc-1", element: 4 });
    assert.deepEqual([other.labels, other.selectable], [[], true]);
  });

  it("список: порядок как у модели, empty скрыт, junk показан выбираемым", () => {
    const rows = describeVideoList(
      [record({ element: 1 }), record({ element: 2, verdict: "empty" }), record({ element: 3, verdict: "junk" }), record({ element: 4, verdict: "protected" })],
      "t",
      null,
    );
    assert.deepEqual(rows.map((row) => row.ref.element), [1, 3, 4]);
    assert.equal(rows[1]?.selectable, true, "мусор можно выбрать");
  });
});
