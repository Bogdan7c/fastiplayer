// Проверка формы сообщений входов (сессия 10c): что пришло от страницы или контент-скрипта — не доверяем.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  parseOverlayButtonPressed,
  parsePointerQuery,
  parseRightClickedVideo,
  parseShowEntryResult,
} from "../src/shared/entry-validation.ts";

describe("entry-validation", () => {
  it("видео: метка документа и ключ ≥ 1, лишние поля — отказ", () => {
    assert.deepEqual(parseRightClickedVideo({ document: "abc_-1", element: 3 }), { document: "abc_-1", element: 3 });
    for (const junk of [{ document: "a b", element: 3 }, { document: "doc", element: 0 }, { document: "doc", element: 1.5 }, { document: "doc", element: 1, extra: 1 }, null, [], "doc"]) {
      assert.equal(parseRightClickedVideo(junk), undefined, JSON.stringify(junk));
    }
  });

  it("вопросы про указатель: форма и конечные координаты", () => {
    assert.deepEqual(parsePointerQuery({ kind: "describe_right_clicked", request_id: 1 }), { kind: "describe_right_clicked", request_id: 1 });
    assert.deepEqual(parsePointerQuery({ kind: "video_at_point", request_id: 2, x: -3.5, y: 4 }), { kind: "video_at_point", request_id: 2, x: -3.5, y: 4 });
    for (const junk of [
      { kind: "video_at_point", request_id: 2, x: Number.NaN, y: 4 },
      { kind: "video_at_point", request_id: 2, x: Infinity, y: 4 },
      { kind: "video_at_point", request_id: 2, x: 1 },
      { kind: "video_at_point", request_id: -1, x: 1, y: 1 },
      { kind: "describe_right_clicked", request_id: 2 ** 32 },
      { kind: "describe_candidates", request_id: 1 },
    ]) {
      assert.equal(parsePointerQuery(junk), undefined, JSON.stringify(junk));
    }
  });

  it("нажатие кнопки поверх видео: строгая форма", () => {
    assert.deepEqual(parseOverlayButtonPressed({ kind: "overlay_button_pressed", document: "doc-1", element: 2 }), { kind: "overlay_button_pressed", document: "doc-1", element: 2 });
    assert.equal(parseOverlayButtonPressed({ kind: "overlay_button_pressed", document: "doc-1" }), undefined);
    assert.equal(parseOverlayButtonPressed({ kind: "overlay_button_pressed", document: "doc-1", element: 2, frame_id: 3 }), undefined, "фрейм называет браузер, не страница");
  });

  it("плашка: известный тон, непустые короткие строки, не больше восьми деталей", () => {
    const good = { id: "opened", tone: "success", headline: "Opened in Fastiplayer", details: [] };
    assert.deepEqual(parseShowEntryResult({ kind: "show_entry_result", report: good }), { kind: "show_entry_result", report: good });
    for (const report of [{ ...good, tone: "info" }, { ...good, headline: "" }, { ...good, headline: "x".repeat(301) }, { ...good, details: ["a", ""] }, { ...good, details: Array.from({ length: 9 }, () => "a") }, { ...good, extra: 1 }]) {
      assert.equal(parseShowEntryResult({ kind: "show_entry_result", report }), undefined, JSON.stringify(report).slice(0, 60));
    }
  });
});
