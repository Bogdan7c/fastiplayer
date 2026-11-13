#!/usr/bin/env python3
"""Генерирует общие фикстуры протокола моста Fastiplayer Bridge.

Фикстуры — общий договор Rust (тесты crate-а) и TypeScript (тесты экстеншена,
сессия 07): обе стороны разбирают одни и те же файлы и ждут одного и того же
итога из manifest.json. Формат описан в fixtures/README.md.

Скрипт — намеренно вторая, независимая реализация формата: двоичные рамки он
собирает через struct, а не через код crate-а. Если Rust и этот скрипт поймут
формат по-разному, тесты фикстур упадут.

Запуск (из любого места):
    crates/browser-bridge-protocol/fixtures/generate_fixtures.py

Нужны только медиа из fixtures/media/ (их делает media/generate.sh). Скрипт
стирает и пересоздаёт control/, session/, data/, streams/, manifest.json,
resource-addresses.json, message-versions.json, value-versions.json,
field-versions.json и page-subtitle-gate.json.
"""

from __future__ import annotations

import json
import shutil
import struct
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

FIXTURES_ROOT = Path(__file__).resolve().parent
MEDIA_ROOT = FIXTURES_ROOT / "media"
GENERATED_DIRECTORIES = ("control", "session", "data", "streams")

# Пропуск для фикстур: 43 символа base64url, как настоящий (32 случайных байта).
FIXTURE_TOKEN = "Zml4dHVyZS10b2tlbi1icmlkZ2UtMDEtcHJvdG9jb2w"
# Коды вида двоичной рамки из crate::data_frame.
SOURCE_BUFFER_BYTES_KIND = 1
RANGE_BYTES_KIND = 2
# Протокол 1.5: байты документа субтитров страницы.
PAGE_SUBTITLE_BYTES_KIND = 3
MAX_JSON_SAFE_INTEGER = (1 << 53) - 1
# Лимиты протокола 1.1 (crate::limits): ресурс HLS за один запрос.
MAX_RESOURCE_FETCH_BYTES = 128 * 1024 * 1024
U32_MAX = 0xFFFF_FFFF
# Лимиты протокола 1.5 (crate::limits): субтитры страницы.
MAX_PAGE_SUBTITLE_DOCUMENT_BYTES = 8 * 1024 * 1024
MAX_PAGE_SUBTITLE_TRACKS = 64
MAX_SUBTITLE_LABEL_BYTES = 256
MAX_SUBTITLE_LANGUAGE_TAG_BYTES = 35
MAX_PAGE_TITLE_BYTES = 1024
# Адрес-заглушка номерка ресурса HLS (crate::resource_address), независимо от Rust.
RESOURCE_ADDRESS_PREFIX = "https://resource.fastiplayer-bridge.invalid/"

VIDEO_MIME = 'video/mp4; codecs="avc1.42c00a"'
AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"'
MUXED_MIME = 'video/webm; codecs="vp9,opus"'


def compact_json(message: dict[str, Any]) -> bytes:
    """Канонический JSON: без пробелов, не-ASCII как есть — так пишет serde_json."""
    return json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def source_buffer_frame(
    generation: int,
    source_buffer: int,
    sequence: int,
    payload: bytes,
    *,
    kind: int = SOURCE_BUFFER_BYTES_KIND,
    flags: int = 0,
    reserved_tail: int = 0,
) -> bytes:
    """Рамка вида 1: 24 байта заголовка little-endian + груз."""
    header = struct.pack(
        "<BBHIHHIQ", kind, flags, 0, generation, source_buffer, 0, reserved_tail, sequence
    )
    return header + payload


def range_frame(request_id: int, offset: int, payload: bytes, *, reserved: int = 0) -> bytes:
    """Рамка вида 2: байты файла <video src>."""
    header = struct.pack("<BBHIQQ", RANGE_BYTES_KIND, 0, 0, request_id, offset, reserved)
    return header + payload


def page_subtitle_frame(track: int, offset: int, payload: bytes, *, reserved: int = 0) -> bytes:
    """Рамка вида 3 (протокол 1.5): байты документа субтитров страницы."""
    header = struct.pack("<BBHIQQ", PAGE_SUBTITLE_BYTES_KIND, 0, 0, track, offset, reserved)
    return header + payload


@dataclass
class Manifest:
    """Список случаев: файл, схема разбора и ожидаемый итог."""

    cases: list[dict[str, Any]] = field(default_factory=list)
    streams: list[str] = field(default_factory=list)

    def add(self, relative_path: str, schema: str, content: bytes, expect: dict[str, Any]) -> None:
        target = FIXTURES_ROOT / relative_path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)
        self.cases.append({"file": relative_path, "schema": schema, "expect": expect})

    def valid(self, relative_path: str, schema: str, message: dict[str, Any]) -> None:
        self.add(relative_path, schema, compact_json(message), {"result": "ok"})

    def invalid(self, relative_path: str, schema: str, content: bytes, error_kind: str) -> None:
        self.add(relative_path, schema, content, {"result": "error", "kind": error_kind})


def version(major: int, minor: int) -> dict[str, int]:
    return {"major": major, "minor": minor}


def extension_hello(major: int = 1) -> dict[str, Any]:
    return {
        "type": "hello",
        "protocol": version(major, 0),
        "software_version": "0.1.0",
        "capabilities": [],
    }


def open_session(**overrides: Any) -> dict[str, Any]:
    message: dict[str, Any] = {
        "type": "open_session",
        "request_id": 1,
        "session_key": "tab-12-video-3",
        "page": {"title": "Шрек 2 — серія 3 🎬", "domain": "uaserials.com"},
        "intent": "window",
        "hints": {
            "duration": {"kind": "finite", "duration_us": 1_325_000_000},
            "position": {"kind": "unknown"},
        },
    }
    message.update(overrides)
    return message


def write_extension_control(manifest: Manifest) -> None:
    schema = "extension_control"
    manifest.valid("control/valid/hello-extension.json", schema, extension_hello())
    # Другой major разбирается нормально: несовместимость решает договорённость о версии.
    manifest.valid("control/valid/hello-extension-major-2.json", schema, extension_hello(major=2))
    manifest.valid("control/valid/open-session-window.json", schema, open_session())
    manifest.valid(
        "control/valid/open-session-pip-live-rtl.json",
        schema,
        open_session(
            request_id=2,
            session_key="tab-4-video-1",
            page={"title": "بث مباشر — שידור חי", "domain": "www.youtube.com"},
            intent="picture_in_picture",
            hints={
                "duration": {"kind": "live"},
                "position": {"kind": "at", "position_us": 46_800_000_000},
            },
        ),
    )
    manifest.valid(
        "control/valid/open-session-in-page-untitled.json",
        schema,
        open_session(
            request_id=3,
            page={"title": "", "domain": "[::1]"},
            intent="in_page",
            hints={"duration": {"kind": "unknown"}, "position": {"kind": "unknown"}},
        ),
    )

    hello = json.dumps(extension_hello(), separators=(",", ":"))
    invalid = "control/invalid/"
    manifest.invalid(f"{invalid}unknown-type.json", schema, b'{"type":"open_window"}', "unknown_type")
    manifest.invalid(
        f"{invalid}unknown-field.json",
        schema,
        compact_json({**extension_hello(), "extra": 1}),
        "unknown_field",
    )
    without_capabilities = extension_hello()
    del without_capabilities["capabilities"]
    manifest.invalid(
        f"{invalid}missing-field.json", schema, compact_json(without_capabilities), "missing_field"
    )
    manifest.invalid(
        f"{invalid}duplicate-field.json",
        schema,
        hello.replace('"protocol"', '"protocol":{"major":1,"minor":0},"protocol"', 1).encode(),
        "duplicate_field",
    )
    manifest.invalid(
        f"{invalid}duplicate-type.json",
        schema,
        hello.replace('"type":"hello"', '"type":"hello","type":"hello"', 1).encode(),
        "duplicate_field",
    )
    manifest.invalid(
        f"{invalid}missing-type.json",
        schema,
        b'{"protocol":{"major":1,"minor":0}}',
        "missing_type",
    )
    manifest.invalid(f"{invalid}not-an-object.json", schema, b'["hello"]', "not_an_object")
    manifest.invalid(f"{invalid}type-not-a-string.json", schema, b'{"type":7}', "wrong_value_type")
    manifest.invalid(f"{invalid}truncated.json", schema, hello[:20].encode(), "syntax")
    manifest.invalid(f"{invalid}trailing-garbage.json", schema, (hello + "{}").encode(), "syntax")
    manifest.invalid(f"{invalid}not-utf8.json", schema, b'{"type":"hello\xff"}', "not_utf8")
    manifest.invalid(
        f"{invalid}unknown-intent.json", schema, compact_json(open_session(intent="teleport")), "unknown_value"
    )
    manifest.invalid(
        f"{invalid}request-id-as-string.json",
        schema,
        compact_json(open_session(request_id="1")),
        "wrong_value_type",
    )
    manifest.invalid(
        f"{invalid}fractional-duration.json",
        schema,
        compact_json(
            open_session(
                hints={
                    "duration": {"kind": "finite", "duration_us": 1.5},
                    "position": {"kind": "unknown"},
                }
            )
        ),
        "wrong_value_type",
    )
    manifest.invalid(
        f"{invalid}negative-position.json",
        schema,
        compact_json(
            open_session(
                hints={
                    "duration": {"kind": "unknown"},
                    "position": {"kind": "at", "position_us": -1},
                }
            )
        ),
        "number_out_of_range",
    )
    manifest.invalid(
        f"{invalid}position-beyond-js-safe-integer.json",
        schema,
        compact_json(
            open_session(
                hints={
                    "duration": {"kind": "unknown"},
                    "position": {"kind": "at", "position_us": MAX_JSON_SAFE_INTEGER + 1},
                }
            )
        ),
        "number_out_of_range",
    )
    manifest.invalid(
        f"{invalid}field-in-unit-variant.json",
        schema,
        compact_json(
            open_session(
                hints={"duration": {"kind": "live", "since": 0}, "position": {"kind": "unknown"}}
            )
        ),
        "unknown_field",
    )
    manifest.invalid(
        f"{invalid}title-with-bidi-override.json",
        schema,
        compact_json(open_session(page={"title": "movie‮4pm.exe", "domain": "example.com"})),
        "text_forbidden_character",
    )
    manifest.invalid(
        f"{invalid}title-with-nul.json",
        schema,
        compact_json(open_session(page={"title": "a\u0000b", "domain": "example.com"})),
        "text_forbidden_character",
    )
    manifest.invalid(
        f"{invalid}title-too-long.json",
        schema,
        compact_json(open_session(page={"title": "ї" * 513, "domain": "example.com"})),
        "text_too_long",
    )
    manifest.invalid(
        f"{invalid}domain-with-path.json",
        schema,
        compact_json(open_session(page={"title": "x", "domain": "example.com/watch"})),
        "text_forbidden_character",
    )
    manifest.invalid(
        f"{invalid}empty-session-key.json",
        schema,
        compact_json(open_session(session_key="")),
        "text_empty",
    )
    manifest.invalid(
        f"{invalid}duplicate-capability.json",
        schema,
        compact_json({**extension_hello(), "capabilities": ["cheap_codec.h264", "cheap_codec.h264"]}),
        "duplicate_id",
    )


def write_player_control(manifest: Manifest) -> None:
    schema = "player_control"
    manifest.valid(
        "control/valid/hello-accepted.json",
        schema,
        {
            "type": "hello_accepted",
            "agreed_protocol": version(1, 0),
            "player": {
                "protocol": version(1, 2),
                "software_version": "0.2.0",
                "capabilities": ["cheap_codec.h264", "feature.from_the_future"],
            },
        },
    )
    manifest.valid(
        "control/valid/session-granted.json",
        schema,
        {
            "type": "session_granted",
            "request_id": 1,
            "session_key": "tab-12-video-3",
            "port": 40213,
            "token": FIXTURE_TOKEN,
            "outcome": "started",
        },
    )
    manifest.valid(
        "control/valid/refused-incompatible-version.json",
        schema,
        {
            "type": "refused",
            "in_reply_to": {"kind": "hello"},
            "reason": {"kind": "incompatible_version", "update": "extension"},
        },
    )
    manifest.valid(
        "control/valid/refused-malformed.json",
        schema,
        {
            "type": "refused",
            "in_reply_to": {"kind": "unparsed_message"},
            "reason": {"kind": "malformed_message", "error": "unknown_field"},
        },
    )
    manifest.valid(
        "control/valid/refused-protected-content.json",
        schema,
        {
            "type": "refused",
            "in_reply_to": {"kind": "open_session", "request_id": 2},
            "reason": {"kind": "protected_content"},
        },
    )
    invalid = "control/invalid/"
    manifest.invalid(
        f"{invalid}port-zero.json",
        schema,
        compact_json(
            {
                "type": "session_granted",
                "request_id": 1,
                "session_key": "k",
                "port": 0,
                "token": FIXTURE_TOKEN,
                "outcome": "started",
            }
        ),
        "number_out_of_range",
    )
    manifest.invalid(
        f"{invalid}token-too-short.json",
        schema,
        compact_json(
            {
                "type": "session_granted",
                "request_id": 1,
                "session_key": "k",
                "port": 40213,
                "token": "short",
                "outcome": "started",
            }
        ),
        "text_too_short",
    )


def page_message(message_type: str, **fields: Any) -> dict[str, Any]:
    return {"type": message_type, **fields}


def resource_finished(request_id: int, outcome: dict[str, Any]) -> dict[str, Any]:
    return page_message("resource_fetch_finished", request_id=request_id, outcome=outcome)


def resource_failed(request_id: int, failure: dict[str, Any]) -> dict[str, Any]:
    return resource_finished(request_id, {"kind": "failed", "failure": failure})


def resource_fetch_finished_cases() -> list[tuple[str, dict[str, Any]]]:
    """Верные итоги запроса ресурса HLS (протокол 1.1): каждый вид итога и края."""
    return [
        ("resource-fetch-completed", resource_finished(10, {"kind": "completed", "size_bytes": 1834})),
        ("resource-fetch-completed-empty-body", resource_finished(11, {"kind": "completed", "size_bytes": 0})),
        (
            "resource-fetch-completed-max-body",
            resource_finished(12, {"kind": "completed", "size_bytes": MAX_RESOURCE_FETCH_BYTES}),
        ),
        ("resource-fetch-network-error", resource_failed(13, {"kind": "network_error"})),
        ("resource-fetch-http-404", resource_failed(14, {"kind": "http_error", "status": 404})),
        ("resource-fetch-http-100", resource_failed(15, {"kind": "http_error", "status": 100})),
        ("resource-fetch-http-599", resource_failed(16, {"kind": "http_error", "status": 599})),
        ("resource-fetch-refused", resource_failed(17, {"kind": "refused"})),
        ("resource-fetch-too-large", resource_failed(18, {"kind": "too_large"})),
    ]


def write_invalid_hls_page_messages(manifest: Manifest) -> None:
    """Неверные `hls_source_added` и `resource_fetch_finished` (протокол 1.1)."""
    schema = "page_session"
    invalid = "session/invalid/page-"
    for name, message, kind in [
        ("hls-source-added-missing-root", page_message("hls_source_added", generation=1), "missing_field"),
        (
            "hls-source-added-unknown-field",
            page_message("hls_source_added", generation=1, root=0, url="https://example.com/a.m3u8"),
            "unknown_field",
        ),
        ("hls-source-added-generation-zero", page_message("hls_source_added", generation=0, root=0), "number_out_of_range"),
        ("hls-source-added-root-negative", page_message("hls_source_added", generation=1, root=-1), "number_out_of_range"),
        (
            "hls-source-added-root-beyond-u32",
            page_message("hls_source_added", generation=1, root=U32_MAX + 1),
            "number_out_of_range",
        ),
        ("resource-fetch-http-99", resource_failed(7, {"kind": "http_error", "status": 99}), "number_out_of_range"),
        ("resource-fetch-http-600", resource_failed(7, {"kind": "http_error", "status": 600}), "number_out_of_range"),
        ("resource-fetch-http-without-status", resource_failed(7, {"kind": "http_error"}), "missing_field"),
        ("resource-fetch-refused-with-status", resource_failed(7, {"kind": "refused", "status": 403}), "unknown_field"),
        ("resource-fetch-unknown-failure", resource_failed(7, {"kind": "timeout"}), "unknown_value"),
        ("resource-fetch-unknown-outcome", resource_finished(7, {"kind": "partial", "size_bytes": 10}), "unknown_value"),
        (
            "resource-fetch-body-above-limit",
            resource_finished(7, {"kind": "completed", "size_bytes": MAX_RESOURCE_FETCH_BYTES + 1}),
            "number_out_of_range",
        ),
        (
            # Размер ресурса — у `range_fetch_finished`; у ресурса HLS только размер тела.
            "resource-fetch-completed-with-resource-size",
            resource_finished(7, {"kind": "completed", "size_bytes": 10, "resource_size": {"kind": "unknown"}}),
            "unknown_field",
        ),
        ("resource-fetch-missing-outcome", page_message("resource_fetch_finished", request_id=7), "missing_field"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", schema, compact_json(message), kind)


def write_invalid_tab_gone_reasons(manifest: Manifest) -> None:
    """Неверные причины `tab_gone`: метка строгая, без нормализации (протокол 1.3).

    `page_player_failed` — ровно так, в нижнем регистре: другой регистр, дефис вместо
    подчёркивания и похожие слова — незнакомое значение, а не «почти та же причина».
    Внешняя форма serde (`{"page_player_failed":null}`) — ошибка формы, её пишет
    `write_shape_strictness` рядом с другими `tab-gone-reason-as-*`.
    """
    schema = "page_session"
    invalid = "session/invalid/page-tab-gone-"
    for name, reason in [
        ("reason-unknown", "teleported"),
        ("page-player-failed-upper-case", "Page_Player_Failed"),
        ("page-player-failed-with-dashes", "page-player-failed"),
        ("page-player-failed-with-spaces", " page_player_failed"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", schema, compact_json(page_message("tab_gone", reason=reason)), "unknown_value")


PAGE_LEFT_REASONS = ["closed", "navigated_away", "browser_closing", "page_player_failed"]
PAGE_REOPEN_FAILURES = ["no_browser_window", "page_did_not_load", "not_the_same_video", "video_did_not_start"]


def page_return_cases() -> list[tuple[str, dict[str, Any]]]:
    """Верные `page_left`, `page_returned`, `page_reopen_failed` (протокол 1.4)."""
    return [
        *[(f"page-left-{reason.replace('_', '-')}", page_message("page_left", reason=reason)) for reason in PAGE_LEFT_REASONS],
        ("page-returned", page_message("page_returned")),
        *[
            (f"page-reopen-failed-{failure.replace('_', '-')}", page_message("page_reopen_failed", reason=failure))
            for failure in PAGE_REOPEN_FAILURES
        ],
    ]


def write_invalid_page_return(manifest: Manifest) -> None:
    """Неверные сообщения возврата вкладки (протокол 1.4): метки строгие, без нормализации.

    Внешняя форма serde (`{"no_browser_window":null}`) — ошибка формы, её пишет
    `write_shape_strictness` рядом с другими `*-as-map`.
    """
    schema = "page_session"
    invalid = "session/invalid/page-"
    for name, message, kind in [
        ("page-reopen-failed-reason-unknown", page_message("page_reopen_failed", reason="teleported"), "unknown_value"),
        ("page-reopen-failed-upper-case", page_message("page_reopen_failed", reason="No_Browser_Window"), "unknown_value"),
        ("page-reopen-failed-with-dashes", page_message("page_reopen_failed", reason="no-browser-window"), "unknown_value"),
        ("page-reopen-failed-with-spaces", page_message("page_reopen_failed", reason=" no_browser_window"), "unknown_value"),
        ("page-reopen-failed-reason-missing", page_message("page_reopen_failed"), "missing_field"),
        ("page-left-reason-unknown", page_message("page_left", reason="teleported"), "unknown_value"),
        ("page-left-upper-case", page_message("page_left", reason="Closed"), "unknown_value"),
        ("page-left-with-dashes", page_message("page_left", reason="page-player-failed"), "unknown_value"),
        ("page-left-reason-missing", page_message("page_left"), "missing_field"),
        # Метка причины ухода и метка отказа возврата - разные словари: чужая метка не подходит.
        ("page-left-reopen-failure-label", page_message("page_left", reason="no_browser_window"), "unknown_value"),
        ("page-reopen-failed-gone-label", page_message("page_reopen_failed", reason="closed"), "unknown_value"),
        ("page-returned-with-field", page_message("page_returned", reason="closed"), "unknown_field"),
    ]:
        manifest.invalid(f"session/invalid/{name}.json", schema, compact_json(message), kind)


def write_page_session(manifest: Manifest) -> None:
    schema = "page_session"
    valid = "session/valid/page-"
    menu = {
        "kind": "audio",
        "options": [
            {"id": "voice-1", "label": "Українська (дубляж)", "selection": "selected"},
            {"id": "voice-2", "label": "English", "selection": "available"},
        ],
    }
    for name, message in [
        ("attach", page_message("attach", session_key="tab-12-video-3", token=FIXTURE_TOKEN)),
        ("generation-started", page_message("generation_started", generation=1)),
        ("file-source-added", page_message("file_source_added", generation=1)),
        ("hls-source-added", page_message("hls_source_added", generation=1, root=0)),
        ("hls-source-added-max-root", page_message("hls_source_added", generation=2, root=U32_MAX)),
        (
            "source-buffer-added",
            page_message("source_buffer_added", generation=1, source_buffer=0, mime_type=VIDEO_MIME),
        ),
        (
            "source-buffer-type-changed",
            page_message(
                "source_buffer_type_changed",
                generation=1,
                source_buffer=0,
                mime_type='video/mp4; codecs="av01.0.05M.08"',
            ),
        ),
        (
            "timestamp-offset-negative",
            page_message("timestamp_offset_changed", generation=1, source_buffer=1, offset_us=-21_333),
        ),
        (
            "source-buffer-removed-range",
            page_message(
                "source_buffer_removed",
                generation=1,
                source_buffer=0,
                start_us=0,
                end={"kind": "at", "time_us": 30_000_000},
            ),
        ),
        (
            "source-buffer-removed-to-end",
            page_message(
                "source_buffer_removed",
                generation=1,
                source_buffer=1,
                start_us=60_000_000,
                end={"kind": "end_of_timeline"},
            ),
        ),
        ("source-buffer-aborted", page_message("source_buffer_aborted", generation=1, source_buffer=0)),
        (
            "seek-started-by-player",
            page_message("seek_started", target_us=600_000_000, cause={"kind": "player_seek", "seek_id": 4}),
        ),
        ("seek-started-by-page", page_message("seek_started", target_us=0, cause={"kind": "page_seek"})),
        (
            "duration-changed",
            page_message("duration_changed", generation=2, duration={"kind": "finite", "duration_us": 15_000_000}),
        ),
        ("end-of-stream", page_message("end_of_stream", generation=2)),
        (
            "page-playback-set-rate",
            page_message("page_playback", request={"kind": "set_rate", "rate": 2000}),
        ),
        ("page-playback-pause", page_message("page_playback", request={"kind": "pause"})),
        ("variant-menu-changed", page_message("variant_menu_changed", menu=menu)),
        ("ad-state-none", page_message("ad_state", ad={"kind": "none"})),
        (
            "ad-state-unknown-organs",
            page_message(
                "ad_state",
                ad={"kind": "showing", "pod": {"kind": "unknown"}, "skip": {"kind": "unavailable"}},
            ),
        ),
        (
            "ad-state-countdown",
            page_message(
                "ad_state",
                ad={
                    "kind": "showing",
                    "pod": {"kind": "known", "index": 1, "count": 2},
                    "skip": {"kind": "countdown", "seconds": 5},
                },
            ),
        ),
        (
            "ad-state-available",
            page_message(
                "ad_state",
                ad={
                    "kind": "showing",
                    "pod": {"kind": "known", "index": 2, "count": 2},
                    "skip": {"kind": "available"},
                },
            ),
        ),
        (
            "ad-state-on-page",
            page_message(
                "ad_state",
                ad={"kind": "showing", "pod": {"kind": "unknown"}, "skip": {"kind": "on_page"}},
            ),
        ),
        (
            "range-fetch-completed",
            page_message(
                "range_fetch_finished",
                request_id=7,
                outcome={"kind": "completed", "resource_size": {"kind": "known", "size_bytes": 48_211_005}},
            ),
        ),
        (
            "range-fetch-failed",
            page_message("range_fetch_finished", request_id=8, outcome={"kind": "failed", "failure": "http_error"}),
        ),
        ("tab-gone", page_message("tab_gone", reason="navigated_away")),
        # Протокол 1.3 (решение 237): плеер сайта сломался во время передачи.
        ("tab-gone-page-player-failed", page_message("tab_gone", reason="page_player_failed")),
        # Протокол 1.4 (решения 240-241): возврат вкладки.
        *page_return_cases(),
        *resource_fetch_finished_cases(),
    ]:
        manifest.valid(f"{valid}{name}.json", schema, message)
    write_invalid_hls_page_messages(manifest)
    write_invalid_tab_gone_reasons(manifest)
    write_invalid_page_return(manifest)

    invalid = "session/invalid/page-"
    manifest.invalid(
        f"{invalid}seek-epoch-started-is-not-a-message.json",
        schema,
        compact_json(page_message("seek_epoch_started", generation=1, epoch=1)),
        "unknown_type",
    )
    manifest.invalid(
        f"{invalid}source-buffer-index-8.json",
        schema,
        compact_json(page_message("source_buffer_aborted", generation=1, source_buffer=8)),
        "number_out_of_range",
    )
    manifest.invalid(
        f"{invalid}generation-zero.json",
        schema,
        compact_json(page_message("generation_started", generation=0)),
        "number_out_of_range",
    )
    manifest.invalid(
        f"{invalid}file-source-added-generation-zero.json",
        schema,
        compact_json(page_message("file_source_added", generation=0)),
        "number_out_of_range",
    )
    manifest.invalid(
        f"{invalid}file-source-added-unknown-field.json",
        schema,
        compact_json(page_message("file_source_added", generation=1, mime_type=VIDEO_MIME)),
        "unknown_field",
    )
    manifest.invalid(
        f"{invalid}empty-removal-range.json",
        schema,
        compact_json(
            page_message(
                "source_buffer_removed",
                generation=1,
                source_buffer=0,
                start_us=5_000_000,
                end={"kind": "at", "time_us": 5_000_000},
            )
        ),
        "empty_range",
    )
    manifest.invalid(
        f"{invalid}menu-two-selected.json",
        schema,
        compact_json(
            page_message(
                "variant_menu_changed",
                menu={
                    "kind": "quality",
                    "options": [
                        {"id": "q1080", "label": "1080p", "selection": "selected"},
                        {"id": "q720", "label": "720p", "selection": "selected"},
                    ],
                },
            )
        ),
        "conflicting_selection",
    )
    manifest.invalid(
        f"{invalid}menu-duplicate-id.json",
        schema,
        compact_json(
            page_message(
                "variant_menu_changed",
                menu={
                    "kind": "subtitles",
                    "options": [
                        {"id": "en", "label": "English", "selection": "available"},
                        {"id": "en", "label": "English (auto)", "selection": "available"},
                    ],
                },
            )
        ),
        "duplicate_id",
    )
    manifest.invalid(
        f"{invalid}menu-label-with-line-break.json",
        schema,
        compact_json(
            page_message(
                "variant_menu_changed",
                menu={"kind": "audio", "options": [{"id": "a", "label": "line\nbreak", "selection": "available"}]},
            )
        ),
        "text_forbidden_character",
    )
    manifest.invalid(
        f"{invalid}rate-zero.json",
        schema,
        compact_json(page_message("page_playback", request={"kind": "set_rate", "rate": 0})),
        "number_out_of_range",
    )
    manifest.invalid(
        f"{invalid}rate-above-16x.json",
        schema,
        compact_json(page_message("page_playback", request={"kind": "set_rate", "rate": 16_001})),
        "number_out_of_range",
    )
    no_skip = {"kind": "unavailable"}
    for name, pod in [
        ("ad-index-zero", {"kind": "known", "index": 0, "count": 2}),
        ("ad-count-above-limit", {"kind": "known", "index": 1, "count": 65}),
        ("ad-index-above-count", {"kind": "known", "index": 3, "count": 2}),
    ]:
        manifest.invalid(
            f"{invalid}{name}.json",
            schema,
            compact_json(page_message("ad_state", ad={"kind": "showing", "pod": pod, "skip": no_skip})),
            "number_out_of_range",
        )
    manifest.invalid(
        f"{invalid}ad-countdown-above-limit.json",
        schema,
        compact_json(
            page_message(
                "ad_state",
                ad={
                    "kind": "showing",
                    "pod": {"kind": "unknown"},
                    "skip": {"kind": "countdown", "seconds": 3601},
                },
            )
        ),
        "number_out_of_range",
    )


def write_player_session(manifest: Manifest) -> None:
    schema = "player_session"
    valid = "session/valid/player-"
    for name, message in [
        ("attached", {"type": "attached"}),
        (
            "clock-playing",
            {"type": "clock", "seq": 812, "position_us": 1_029_250_000, "state": "playing", "rate": 1000},
        ),
        ("clock-waiting", {"type": "clock", "seq": 813, "position_us": 1_029_500_000, "state": "waiting", "rate": 1500}),
        # Протокол 1.10 (решение 311): тик называет поколение, которое описывает. Поле последнее.
        ("clock-with-generation", clock_with_generation(2)),
        ("clock-ended-of-generation-1", {**clock_with_generation(1), "state": "ended", "position_us": 12_000_000}),
        ("clock-generation-u32-max", clock_with_generation(U32_MAX)),
        (
            "buffer-state-full",
            {"type": "buffer_state", "generation": 3, "ahead_us": 118_000_000, "intake": "full"},
        ),
        ("play", {"type": "play"}),
        ("pause", {"type": "pause"}),
        ("seek", {"type": "seek", "seek_id": 4, "target_us": 600_000_000}),
        ("set-rate", {"type": "set_rate", "rate": 2000}),
        ("return-to-page", {"type": "return_to_page", "position_us": 1_029_000_000, "resume": "paused"}),
        ("select-variant", {"type": "select_variant", "kind": "audio", "variant_id": "voice-2"}),
        ("next-episode", {"type": "next_episode"}),
        ("skip-ad", {"type": "skip_ad"}),
        ("fetch-range", {"type": "fetch_range", "request_id": 7, "offset": 0, "length": 1_048_576}),
        ("fetch-resource-whole", fetch_resource(request_id=10, resource=0)),
        (
            "fetch-resource-slice",
            fetch_resource(
                request_id=11,
                resource=2,
                byte_range={"kind": "slice", "offset": 733, "length": 65_536},
                max_bytes=65_536,
            ),
        ),
        (
            "fetch-resource-max-fields",
            fetch_resource(
                request_id=U32_MAX,
                resource=U32_MAX,
                byte_range={"kind": "slice", "offset": MAX_JSON_SAFE_INTEGER, "length": MAX_RESOURCE_FETCH_BYTES},
                max_bytes=MAX_RESOURCE_FETCH_BYTES,
            ),
        ),
        # Протокол 1.4 (решение 241): зритель нажал play, пока страница ушла.
        ("reopen-page", {"type": "reopen_page", "position_us": 123}),
        ("reopen-page-zero", {"type": "reopen_page", "position_us": 0}),
        ("reopen-page-max", {"type": "reopen_page", "position_us": MAX_JSON_SAFE_INTEGER}),
        # Протокол 1.6 (решение 278): зритель хочет увидеть страницу.
        ("show-page", {"type": "show_page"}),
        ("live-window-range", live_window()),
        ("live-window-none", live_window(seekable={"kind": "none"})),
        # Края договора включительно: окно в одну точку ровно на краю эфира.
        ("live-window-point-at-edge", live_window(edge_us=61_000_000, seekable=window_range(61_000_000, 61_000_000))),
        (
            "live-window-max-fields",
            live_window(
                generation=U32_MAX,
                edge_us=MAX_JSON_SAFE_INTEGER,
                seekable=window_range(0, MAX_JSON_SAFE_INTEGER),
            ),
        ),
        # Протокол 1.11 (решения 325–327): шкала поколения стала записью.
        ("recording-window-range", recording_window()),
        ("recording-window-none", recording_window(seekable={"kind": "none"})),
        # Края договора включительно: окно в одну точку ровно на конце записи.
        (
            "recording-window-point-at-end",
            recording_window(end_us=61_000_000, seekable=window_range(61_000_000, 61_000_000)),
        ),
        ("stream-rejected", {"type": "stream_rejected", "generation": 1, "reason": "protected_content"}),
        (
            "stream-rejected-file-unavailable",
            {"type": "stream_rejected", "generation": 2, "reason": "file_unavailable"},
        ),
        (
            "stream-rejected-playback-failed",
            {"type": "stream_rejected", "generation": 3, "reason": "playback_failed"},
        ),
    ]:
        manifest.valid(f"{valid}{name}.json", schema, message)

    invalid = "session/invalid/player-"
    manifest.invalid(
        f"{invalid}field-from-a-future-minor.json",
        schema,
        compact_json(
            {"type": "clock", "seq": 1, "position_us": 0, "state": "paused", "rate": 1000, "latency_us": 5}
        ),
        "unknown_field",
    )
    manifest.invalid(f"{invalid}field-in-unit-message.json", schema, b'{"type":"attached","ok":true}', "unknown_field")
    manifest.invalid(
        f"{invalid}fetch-range-zero-length.json",
        schema,
        compact_json({"type": "fetch_range", "request_id": 7, "offset": 0, "length": 0}),
        "number_out_of_range",
    )
    manifest.invalid(
        f"{invalid}unknown-clock-state.json",
        schema,
        compact_json({"type": "clock", "seq": 1, "position_us": 0, "state": "rewinding", "rate": 1000}),
        "unknown_value",
    )
    write_invalid_clock_generation(manifest)
    write_invalid_fetch_resource(manifest)
    write_invalid_live_window(manifest)
    write_invalid_recording_window(manifest)
    write_invalid_reopen_page(manifest)
    write_invalid_show_page(manifest)


def clock_with_generation(generation: Any) -> dict[str, Any]:
    """Тик часов протокола 1.10: прежние поля и `generation` ПОСЛЕДНИМ (порядок записи обеих сторон)."""
    return {"type": "clock", "seq": 42, "position_us": 11_100_000, "state": "playing", "rate": 1000, "generation": generation}


def write_invalid_clock_generation(manifest: Manifest) -> None:
    """Плохой номер поколения в тике (протокол 1.10).

    Поле необязательное, но «номера нет» на проводе — только отсутствие ключа: `null` —
    `wrong_value_type`, как у любого поля протокола. Поколения считаются с 1: ноль,
    отрицательное и больше `u32` — `number_out_of_range`; строка и дробь — `wrong_value_type`.
    """
    invalid = "session/invalid/player-clock-generation-"
    for name, generation, kind in [
        ("as-null", None, "wrong_value_type"),
        ("as-string", "2", "wrong_value_type"),
        ("fractional", 1.5, "wrong_value_type"),
        ("zero", 0, "number_out_of_range"),
        ("negative", -1, "number_out_of_range"),
        ("beyond-u32", U32_MAX + 1, "number_out_of_range"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", "player_session", compact_json(clock_with_generation(generation)), kind)


def write_invalid_reopen_page(manifest: Manifest) -> None:
    """Неверные `reopen_page` (протокол 1.4): позиция обязательна, неотрицательная, целая."""
    schema = "player_session"
    invalid = "session/invalid/player-reopen-page-"
    for name, message, kind in [
        ("position-missing", {"type": "reopen_page"}, "missing_field"),
        ("position-negative", {"type": "reopen_page", "position_us": -1}, "number_out_of_range"),
        ("position-string", {"type": "reopen_page", "position_us": "123"}, "wrong_value_type"),
        ("position-fractional", {"type": "reopen_page", "position_us": 1.5}, "wrong_value_type"),
        ("position-null", {"type": "reopen_page", "position_us": None}, "wrong_value_type"),
        (
            "position-beyond-js-safe-integer",
            {"type": "reopen_page", "position_us": MAX_JSON_SAFE_INTEGER + 1},
            "number_out_of_range",
        ),
        ("unknown-field", {"type": "reopen_page", "position_us": 1, "page": "x"}, "unknown_field"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", schema, compact_json(message), kind)


def write_invalid_show_page(manifest: Manifest) -> None:
    """Неверный `show_page` (протокол 1.6): у сообщения нет полей, лишнее — отказ."""
    manifest.invalid(
        "session/invalid/player-show-page-unknown-field.json",
        "player_session",
        compact_json({"type": "show_page", "tab": 1}),
        "unknown_field",
    )


def window_range(start_us: int, end_us: int) -> dict[str, Any]:
    """Окно перемотки эфира `{"kind":"range",...}` (протокол 1.2)."""
    return {"kind": "range", "start_us": start_us, "end_us": end_us}


def live_window(**overrides: Any) -> dict[str, Any]:
    """`live_window` (протокол 1.2, решение 232) в каноническом порядке полей."""
    message: dict[str, Any] = {
        "type": "live_window",
        "generation": 3,
        "edge_us": 61_000_000,
        "seekable": window_range(40_000_000, 60_500_000),
    }
    message.update(overrides)
    return message


def write_invalid_live_window(manifest: Manifest) -> None:
    """Неверные `live_window`: инварианты `start ≤ end ≤ edge`, числа, форма `seekable`.

    Вывернутое окно (начало позже конца) — `empty_range`, как у вывернутого
    промежутка `source_buffer_removed`; конец дальше края — `number_out_of_range`.
    `seekable` — не необязательное поле, а метка `none` / `range`.
    """
    schema = "player_session"
    invalid = "session/invalid/player-live-window-"
    whole = live_window()
    without_seekable = {key: value for key, value in whole.items() if key != "seekable"}
    without_edge = {key: value for key, value in whole.items() if key != "edge_us"}
    for name, message, kind in [
        ("start-after-end", live_window(seekable=window_range(50_000_001, 50_000_000)), "empty_range"),
        ("end-beyond-edge", live_window(edge_us=60_000_000, seekable=window_range(40_000_000, 60_000_001)), "number_out_of_range"),
        ("edge-negative", live_window(edge_us=-1, seekable={"kind": "none"}), "number_out_of_range"),
        ("start-negative", live_window(seekable=window_range(-1, 60_500_000)), "number_out_of_range"),
        ("edge-fractional", live_window(edge_us=61_000_000.5), "wrong_value_type"),
        ("end-fractional", live_window(seekable=window_range(40_000_000, 60_500_000.5)), "wrong_value_type"),
        (
            "edge-beyond-js-safe-integer",
            live_window(edge_us=MAX_JSON_SAFE_INTEGER + 1, seekable={"kind": "none"}),
            "number_out_of_range",
        ),
        (
            "end-beyond-js-safe-integer",
            live_window(edge_us=MAX_JSON_SAFE_INTEGER, seekable=window_range(0, MAX_JSON_SAFE_INTEGER + 1)),
            "number_out_of_range",
        ),
        ("generation-beyond-u32", live_window(generation=U32_MAX + 1), "number_out_of_range"),
        ("seekable-missing", without_seekable, "missing_field"),
        ("edge-missing", without_edge, "missing_field"),
        ("seekable-null", live_window(seekable=None), "wrong_value_type"),
        ("seekable-unknown-kind", live_window(seekable={"kind": "dvr", "start_us": 0, "end_us": 1}), "unknown_value"),
        ("seekable-none-with-field", live_window(seekable={"kind": "none", "start_us": 0}), "unknown_field"),
        ("seekable-range-without-end", live_window(seekable={"kind": "range", "start_us": 0}), "missing_field"),
        ("seekable-range-unknown-field", live_window(seekable={**window_range(0, 1), "duration_us": 1}), "unknown_field"),
        ("unknown-field", {**whole, "duration_us": 0}, "unknown_field"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", schema, compact_json(message), kind)


def recording_window(**overrides: Any) -> dict[str, Any]:
    """`recording_window` (протокол 1.11, решения 325–327) в каноническом порядке полей."""
    message: dict[str, Any] = {
        "type": "recording_window",
        "generation": 3,
        "end_us": 61_000_000,
        "seekable": window_range(0, 61_000_000),
    }
    message.update(overrides)
    return message


def write_invalid_recording_window(manifest: Manifest) -> None:
    """Неверные `recording_window`: тот же инвариант `start ≤ end ≤ конца записи`.

    Вывернутое окно — `empty_range`, конец окна дальше конца записи —
    `number_out_of_range` (как у `live_window`); форма — строгая.
    """
    schema = "player_session"
    invalid = "session/invalid/player-recording-window-"
    whole = recording_window()
    without_end = {key: value for key, value in whole.items() if key != "end_us"}
    for name, message, kind in [
        ("start-after-end", recording_window(seekable=window_range(50_000_001, 50_000_000)), "empty_range"),
        (
            "window-end-beyond-end",
            recording_window(end_us=60_000_000, seekable=window_range(0, 60_000_001)),
            "number_out_of_range",
        ),
        ("end-missing", without_end, "missing_field"),
        ("unknown-field", {**whole, "edge_us": 0}, "unknown_field"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", schema, compact_json(message), kind)


def fetch_resource(**overrides: Any) -> dict[str, Any]:
    """`fetch_resource` (протокол 1.1) в каноническом порядке полей."""
    message: dict[str, Any] = {
        "type": "fetch_resource",
        "request_id": 10,
        "resource": 0,
        "byte_range": {"kind": "whole"},
        "max_bytes": 16 * 1024 * 1024,
    }
    message.update(overrides)
    return message


def write_invalid_fetch_resource(manifest: Manifest) -> None:
    """Неверные `fetch_resource`: пределы, форма `byte_range`, лишние и недостающие поля.

    `byte_range` — не необязательное поле, а метка `whole` / `slice`: его отсутствие —
    `missing_field`, а `null` — значение не того типа (`wrong_value_type`).
    """
    schema = "player_session"
    invalid = "session/invalid/player-fetch-resource-"
    whole = fetch_resource()
    without_range = {key: value for key, value in whole.items() if key != "byte_range"}
    without_limit = {key: value for key, value in whole.items() if key != "max_bytes"}
    slice_of = lambda **fields: {"kind": "slice", "offset": 0, "length": 1024, **fields}  # noqa: E731
    for name, message, kind in [
        ("max-bytes-zero", fetch_resource(max_bytes=0), "number_out_of_range"),
        ("max-bytes-above-limit", fetch_resource(max_bytes=MAX_RESOURCE_FETCH_BYTES + 1), "number_out_of_range"),
        ("slice-length-zero", fetch_resource(byte_range=slice_of(length=0)), "number_out_of_range"),
        (
            "slice-length-above-limit",
            fetch_resource(byte_range=slice_of(length=MAX_RESOURCE_FETCH_BYTES + 1)),
            "number_out_of_range",
        ),
        (
            # Кусок длиннее потолка ответа — запрос сам себе противоречит (проверка между полями).
            "slice-longer-than-max-bytes",
            fetch_resource(byte_range=slice_of(length=2048), max_bytes=1024),
            "number_out_of_range",
        ),
        (
            "slice-offset-beyond-js-safe-integer",
            fetch_resource(byte_range=slice_of(offset=MAX_JSON_SAFE_INTEGER + 1)),
            "number_out_of_range",
        ),
        ("resource-negative", fetch_resource(resource=-1), "number_out_of_range"),
        ("resource-beyond-u32", fetch_resource(resource=U32_MAX + 1), "number_out_of_range"),
        ("request-id-beyond-u32", fetch_resource(request_id=U32_MAX + 1), "number_out_of_range"),
        ("byte-range-missing", without_range, "missing_field"),
        ("max-bytes-missing", without_limit, "missing_field"),
        ("byte-range-null", fetch_resource(byte_range=None), "wrong_value_type"),
        ("byte-range-unknown-kind", fetch_resource(byte_range={"kind": "suffix", "length": 10}), "unknown_value"),
        ("byte-range-whole-with-field", fetch_resource(byte_range={"kind": "whole", "length": 10}), "unknown_field"),
        ("slice-without-length", fetch_resource(byte_range={"kind": "slice", "offset": 0}), "missing_field"),
        ("unknown-field", {**whole, "url": "https://example.com/a.m3u8"}, "unknown_field"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", schema, compact_json(message), kind)


def write_shape_strictness(manifest: Manifest) -> None:
    """Строгость формы (сессия 07): значение обязано иметь ровно ту форму JSON, что пишет отправитель.

    serde умеет читать одни и те же данные в нескольких формах; протокол знает
    только одну. Каждый случай здесь — «знакомые данные в чужой форме», и все
    они отвергаются как `wrong_value_type` (не тот тип JSON-значения):
    - метка варианта числом (`"intent":0`, `"kind":1`, `"kind":7`) — serde
      понял бы его как номер варианта;
    - объект-структура массивом (`"protocol":[1,0]`, `"end":["at",5]`) — serde
      понял бы его как поля по порядку;
    - метка-перечисление объектом (`{"closed":null}`) — внешняя форма serde.
    """
    wrong = "wrong_value_type"

    control = "control/invalid/shape-"
    extension_schema = "extension_control"
    hints = open_session()["hints"]
    for name, message in [
        ("hello-protocol-as-array", {**extension_hello(), "protocol": [1, 0]}),
        ("open-session-intent-as-index", open_session(intent=0)),
        ("open-session-intent-as-map", open_session(intent={"window": None})),
        ("open-session-page-as-array", open_session(page=["Шрек 2", "uaserials.com"])),
        ("open-session-hints-as-array", open_session(hints=[{"kind": "unknown"}, {"kind": "unknown"}])),
        (
            "open-session-position-kind-as-index",
            open_session(hints={**hints, "position": {"kind": 1, "position_us": 5_000_000}}),
        ),
        (
            # Номер варианта вне предела — та же ошибка формы, а не «число вне диапазона».
            "open-session-position-kind-index-out-of-range",
            open_session(hints={**hints, "position": {"kind": 7}}),
        ),
        (
            "open-session-duration-as-array",
            open_session(hints={**hints, "duration": ["finite", 1_325_000_000]}),
        ),
    ]:
        manifest.invalid(f"{control}{name}.json", extension_schema, compact_json(message), wrong)

    player_control_schema = "player_control"
    granted = {
        "type": "session_granted",
        "request_id": 1,
        "session_key": "tab-12-video-3",
        "port": 40213,
        "token": FIXTURE_TOKEN,
        "outcome": "started",
    }
    player_hello = {"protocol": version(1, 0), "software_version": "0.2.0", "capabilities": []}
    for name, message in [
        (
            "hello-accepted-agreed-protocol-as-array",
            {"type": "hello_accepted", "agreed_protocol": [1, 0], "player": player_hello},
        ),
        (
            "hello-accepted-player-as-array",
            {"type": "hello_accepted", "agreed_protocol": version(1, 0), "player": [[1, 0], "0.2.0", []]},
        ),
        ("session-granted-outcome-as-index", {**granted, "outcome": 0}),
        ("session-granted-outcome-as-map", {**granted, "outcome": {"started": None}}),
        (
            "refused-in-reply-to-as-array",
            {"type": "refused", "in_reply_to": ["open_session", 2], "reason": {"kind": "protected_content"}},
        ),
        (
            "refused-reason-kind-as-index",
            {"type": "refused", "in_reply_to": {"kind": "hello"}, "reason": {"kind": 4, "update": "extension"}},
        ),
        (
            "refused-update-as-map",
            {
                "type": "refused",
                "in_reply_to": {"kind": "hello"},
                "reason": {"kind": "incompatible_version", "update": {"extension": None}},
            },
        ),
        (
            "refused-malformed-error-as-index",
            {
                "type": "refused",
                "in_reply_to": {"kind": "unparsed_message"},
                "reason": {"kind": "malformed_message", "error": 6},
            },
        ),
    ]:
        manifest.invalid(f"{control}{name}.json", player_control_schema, compact_json(message), wrong)

    page = "session/invalid/page-shape-"
    page_schema = "page_session"
    removed = {"generation": 1, "source_buffer": 0, "start_us": 0}
    menu_option = {"id": "voice-1", "label": "English", "selection": "available"}
    for name, message in [
        ("removed-end-as-array", page_message("source_buffer_removed", **removed, end=["at", 5_000_000])),
        (
            "removed-end-kind-as-index",
            page_message("source_buffer_removed", **removed, end={"kind": 0, "time_us": 5_000_000}),
        ),
        ("seek-cause-kind-as-index", page_message("seek_started", target_us=0, cause={"kind": 1})),
        ("duration-kind-as-index", page_message("duration_changed", generation=1, duration={"kind": 2})),
        ("page-playback-request-as-array", page_message("page_playback", request=["pause"])),
        ("ad-state-as-array", page_message("ad_state", ad=["none"])),
        ("ad-skip-kind-as-index", page_message("ad_state", ad={"kind": "showing", "pod": {"kind": "unknown"}, "skip": {"kind": 2}})),
        ("tab-gone-reason-as-index", page_message("tab_gone", reason=0)),
        ("tab-gone-reason-as-map", page_message("tab_gone", reason={"closed": None})),
        # Незнакомый ключ в той же внешней форме — тоже ошибка формы, а не «незнакомое значение»:
        # форма проверяется раньше значения (иначе Rust и TS могли бы разойтись в виде причины).
        ("tab-gone-reason-unknown-key-as-map", page_message("tab_gone", reason={"teleported": None})),
        # Новая причина 1.3 в той же чужой форме — тоже ошибка формы.
        ("tab-gone-page-player-failed-as-map", page_message("tab_gone", reason={"page_player_failed": None})),
        # Протокол 1.4: метки возврата вкладки - тоже только строкой.
        ("page-left-reason-as-map", page_message("page_left", reason={"closed": None})),
        ("page-left-reason-as-index", page_message("page_left", reason=0)),
        ("page-reopen-failed-reason-as-map", page_message("page_reopen_failed", reason={"no_browser_window": None})),
        ("page-reopen-failed-reason-as-index", page_message("page_reopen_failed", reason=1)),
        ("menu-as-array", page_message("variant_menu_changed", menu=["audio", []])),
        (
            "menu-option-as-array",
            page_message(
                "variant_menu_changed",
                menu={"kind": "audio", "options": [["voice-1", "English", "available"]]},
            ),
        ),
        (
            "menu-selection-as-map",
            page_message(
                "variant_menu_changed",
                menu={"kind": "audio", "options": [{**menu_option, "selection": {"available": None}}]},
            ),
        ),
        (
            "range-fetch-failure-as-index",
            page_message("range_fetch_finished", request_id=7, outcome={"kind": "failed", "failure": 0}),
        ),
        (
            "range-fetch-resource-size-as-array",
            page_message(
                "range_fetch_finished",
                request_id=7,
                outcome={"kind": "completed", "resource_size": ["known", 1_048_576]},
            ),
        ),
        ("resource-fetch-outcome-as-array", resource_finished(7, ["completed", 10])),
        ("resource-fetch-outcome-kind-as-index", resource_finished(7, {"kind": 0, "size_bytes": 10})),
        # Причина ресурса — объект с меткой (у неё бывает `status`), а не строка, как у `range_fetch_finished`.
        ("resource-fetch-failure-as-label", resource_finished(7, {"kind": "failed", "failure": "refused"})),
        ("resource-fetch-failure-kind-as-index", resource_failed(7, {"kind": 1, "status": 404})),
        ("resource-fetch-status-as-text", resource_failed(7, {"kind": "http_error", "status": "404"})),
    ]:
        manifest.invalid(f"{page}{name}.json", page_schema, compact_json(message), wrong)

    player = "session/invalid/player-shape-"
    player_schema = "player_session"
    clock = {"type": "clock", "seq": 1, "position_us": 0, "rate": 1000}
    for name, message in [
        ("clock-state-as-index", {**clock, "state": 0}),
        ("clock-state-as-map", {**clock, "state": {"playing": None}}),
        ("buffer-intake-as-index", {"type": "buffer_state", "generation": 1, "ahead_us": 0, "intake": 1}),
        (
            "return-to-page-resume-as-map",
            {"type": "return_to_page", "position_us": 0, "resume": {"paused": None}},
        ),
        ("select-variant-kind-as-index", {"type": "select_variant", "kind": 0, "variant_id": "voice-2"}),
        (
            "stream-rejected-reason-as-map",
            {"type": "stream_rejected", "generation": 1, "reason": {"unsupported_codec": None}},
        ),
        ("fetch-resource-byte-range-as-array", fetch_resource(byte_range=["slice", 0, 10])),
        ("fetch-resource-byte-range-as-label", fetch_resource(byte_range="whole")),
        ("fetch-resource-byte-range-kind-as-index", fetch_resource(byte_range={"kind": 1, "offset": 0, "length": 10})),
        ("live-window-seekable-as-array", live_window(seekable=["range", 0, 10])),
        ("live-window-seekable-as-label", live_window(seekable="none")),
        ("live-window-seekable-kind-as-index", live_window(seekable={"kind": 1, "start_us": 0, "end_us": 10})),
    ]:
        manifest.invalid(f"{player}{name}.json", player_schema, compact_json(message), wrong)


def write_data_frames(manifest: Manifest) -> None:
    schema = "data_frame"
    payload = b"\x00\x00\x00\x1cftypiso5"

    def valid_frame(name: str, content: bytes, frame: dict[str, Any]) -> None:
        manifest.add(f"data/valid/{name}.bin", schema, content, {"result": "ok", "frame": frame})

    valid_frame(
        "source-buffer-first-frame",
        source_buffer_frame(1, 0, 0, payload),
        {"kind": "source_buffer_bytes", "generation": 1, "source_buffer": 0, "sequence": 0, "payload_len": len(payload)},
    )
    valid_frame(
        "source-buffer-max-fields",
        source_buffer_frame(0xFFFF_FFFF, 7, 0xFFFF_FFFF_FFFF_FFFF, b"\xff"),
        {
            "kind": "source_buffer_bytes",
            "generation": 0xFFFF_FFFF,
            "source_buffer": 7,
            # 2^64 − 1 не помещается в number JavaScript: строкой, TypeScript читает через BigInt.
            "sequence": str(0xFFFF_FFFF_FFFF_FFFF),
            "payload_len": 1,
        },
    )
    valid_frame(
        # Тело ресурса HLS (протокол 1.1): та же рамка вида 2, смещение — внутри тела с 0.
        "range-bytes-resource-body-start",
        range_frame(10, 0, b"#EXTM3U\n"),
        {"kind": "range_bytes", "request_id": 10, "offset": 0, "payload_len": 8},
    )
    valid_frame(
        "range-bytes-max-offset",
        range_frame(9, MAX_JSON_SAFE_INTEGER, b"abc"),
        {"kind": "range_bytes", "request_id": 9, "offset": MAX_JSON_SAFE_INTEGER, "payload_len": 3},
    )

    def invalid_frame(name: str, content: bytes, error_kind: str) -> None:
        manifest.add(f"data/invalid/{name}.bin", schema, content, {"result": "error", "kind": error_kind})

    good = source_buffer_frame(1, 0, 0, payload)
    invalid_frame("shorter-than-header", good[:23], "frame_too_short")
    invalid_frame("header-without-payload", good[:24], "payload_empty")
    # С протокола 1.5 вид 3 занят (субтитры страницы): незнакомый — следующий.
    invalid_frame("unknown-kind-4", source_buffer_frame(1, 0, 0, payload, kind=4), "unknown_frame_kind")
    invalid_frame("kind-zero", source_buffer_frame(1, 0, 0, payload, kind=0), "unknown_frame_kind")
    invalid_frame("flags-set", source_buffer_frame(1, 0, 0, payload, flags=1), "reserved_bits_set")
    invalid_frame(
        "source-buffer-reserved-set",
        source_buffer_frame(1, 0, 0, payload, reserved_tail=1),
        "reserved_bits_set",
    )
    invalid_frame("range-reserved-set", range_frame(1, 0, payload, reserved=1), "reserved_bits_set")
    invalid_frame("generation-zero", source_buffer_frame(0, 0, 0, payload), "number_out_of_range")
    invalid_frame("source-buffer-8", source_buffer_frame(1, 8, 0, payload), "number_out_of_range")
    invalid_frame("offset-beyond-js-safe-integer", range_frame(1, MAX_JSON_SAFE_INTEGER + 1, payload), "number_out_of_range")
    write_page_subtitle_frames(manifest)


def write_page_subtitle_frames(manifest: Manifest) -> None:
    """Рамки вида 3 (протокол 1.5): разбор формы. Версию сеанса разбор не знает — её и
    порядок документа проверяет сторож (случаи — page-subtitle-gate.json)."""
    schema = "data_frame"
    document = "WEBVTT\n\n00:00.000 --> 00:01.000\nПривіт\n".encode()
    manifest.add(
        "data/valid/page-subtitle-first-frame.bin",
        schema,
        page_subtitle_frame(1, 0, document),
        {"result": "ok", "frame": {"kind": "page_subtitle_bytes", "track": 1, "offset": 0, "payload_len": len(document)}},
    )
    manifest.add(
        "data/valid/page-subtitle-max-fields.bin",
        schema,
        page_subtitle_frame(U32_MAX, MAX_JSON_SAFE_INTEGER, b"\xff"),
        {
            "result": "ok",
            "frame": {"kind": "page_subtitle_bytes", "track": U32_MAX, "offset": MAX_JSON_SAFE_INTEGER, "payload_len": 1},
        },
    )
    for name, content, kind in [
        ("page-subtitle-track-zero", page_subtitle_frame(0, 0, document), "number_out_of_range"),
        (
            "page-subtitle-offset-beyond-js-safe-integer",
            page_subtitle_frame(1, MAX_JSON_SAFE_INTEGER + 1, document),
            "number_out_of_range",
        ),
        ("page-subtitle-reserved-set", page_subtitle_frame(1, 0, document, reserved=1), "reserved_bits_set"),
        ("page-subtitle-without-payload", page_subtitle_frame(1, 0, b""), "payload_empty"),
    ]:
        manifest.add(f"data/invalid/{name}.bin", schema, content, {"result": "error", "kind": kind})


@dataclass
class StreamWriter:
    """Записанный поток сеанса: шаги по порядку и ожидаемый итог сторожа."""

    name: str
    steps: list[dict[str, str]] = field(default_factory=list)
    sequences: dict[tuple[int, int], int] = field(default_factory=dict)

    @property
    def directory(self) -> Path:
        return FIXTURES_ROOT / "streams" / self.name

    def _next_file(self, suffix: str) -> str:
        return f"{len(self.steps) + 1:03d}{suffix}"

    def text(self, message: dict[str, Any]) -> None:
        file_name = self._next_file(".json")
        self.directory.mkdir(parents=True, exist_ok=True)
        (self.directory / file_name).write_bytes(compact_json(message))
        self.steps.append({"text": file_name})

    def frame(self, generation: int, source_buffer: int, payload: bytes) -> None:
        key = (generation, source_buffer)
        sequence = self.sequences.get(key, 0)
        self.sequences[key] = sequence + 1
        file_name = self._next_file(".bin")
        self.directory.mkdir(parents=True, exist_ok=True)
        (self.directory / file_name).write_bytes(source_buffer_frame(generation, source_buffer, sequence, payload))
        self.steps.append({"binary": file_name})

    def range_bytes(self, request_id: int, offset: int, payload: bytes) -> None:
        """Рамка вида 2: кусок тела ответа на `fetch_range` / `fetch_resource`."""
        file_name = self._next_file(".bin")
        self.directory.mkdir(parents=True, exist_ok=True)
        (self.directory / file_name).write_bytes(range_frame(request_id, offset, payload))
        self.steps.append({"binary": file_name})

    def side_file(self, name: str, content: bytes) -> str:
        """Файл рядом с записью, не шаг (например, тело ресурса для ожидания); путь от корня фикстур."""
        self.directory.mkdir(parents=True, exist_ok=True)
        (self.directory / name).write_bytes(content)
        return f"streams/{self.name}/{name}"

    def await_clock(self, position_us: int) -> None:
        """Пауза записи: страница ждала, пока часы плеера дойдут до `position_us`.

        Не сообщение и не рамка, а темп воспроизведения записи: смена качества
        случилась посреди просмотра. Проигрыватель записи (тест) ждёт `clock` плеера
        с позицией не меньше этой; сторож протокола такие шаги пропускает.
        """
        self.steps.append({"await_clock_us": position_us})

    def finish(self, manifest: Manifest, expect: dict[str, Any]) -> None:
        # Тела ответов рамками вида 2 есть только у потоков с запросами; у остальных — пусто.
        expect = {**expect, "resource_bodies": expect.get("resource_bodies", [])}
        (self.directory / "stream.json").write_bytes(
            json.dumps({"steps": self.steps, "expect": expect}, ensure_ascii=False, indent=2).encode() + b"\n"
        )
        manifest.streams.append(self.name)


@dataclass
class Fragment:
    """Фрагмент fMP4 (`moof` + `mdat`): время начала и место в файле."""

    start_us: int
    offset: int
    end: int


def split_fragments(mp4: bytes) -> tuple[int, list[Fragment]]:
    """Конец init-сегмента и фрагменты fMP4 с временем начала (`tfdt` / `mdhd`).

    Независимый от Rust разбор: только верхние боксы и два поля времени.
    """
    offset, init_end, timescale = 0, 0, 0
    fragments: list[Fragment] = []
    while offset < len(mp4):
        size, box_type = struct.unpack_from(">I4s", mp4, offset)
        box = mp4[offset : offset + size]
        if box_type == b"moov":
            mdhd = box.find(b"mdhd")
            version = box[mdhd + 4]
            timescale_at = mdhd + (16 if version == 0 else 24)
            timescale = struct.unpack_from(">I", box, timescale_at)[0]
            init_end = offset + size
        elif box_type == b"moof":
            tfdt = box.find(b"tfdt")
            version = box[tfdt + 4]
            decode_time = struct.unpack_from(">I" if version == 0 else ">Q", box, tfdt + 8)[0]
            fragments.append(Fragment(decode_time * 1_000_000 // timescale, offset, offset + size))
        elif box_type == b"mdat" and fragments:
            fragments[-1].end = offset + size
        offset += size
    return init_end, fragments


def fragments_from(fragments: list[Fragment], start_us: int, end_us: int) -> list[Fragment]:
    """Фрагменты, нужные странице для показа [start_us, end_us): с последнего,
    начатого не позже `start_us` (страница кладёт коробку, содержащую её позицию)."""
    first = max(index for index, fragment in enumerate(fragments) if fragment.start_us <= start_us)
    return [fragment for fragment in fragments[first:] if fragment.start_us < end_us]


def write_streams(manifest: Manifest) -> None:
    video = (MEDIA_ROOT / "video.mp4").read_bytes()
    audio = (MEDIA_ROOT / "audio.mp4").read_bytes()
    muxed = (MEDIA_ROOT / "muxed.webm").read_bytes()

    # 1. YouTube-образный поток: два SourceBuffer, куски режутся посреди боксов
    #    (733 — граница moov/moof, 5774 — середина второго moof видео).
    stream = StreamWriter("two-source-buffers-mid-box-cuts")
    stream.text(page_message("generation_started", generation=1))
    stream.text(page_message("source_buffer_added", generation=1, source_buffer=0, mime_type=VIDEO_MIME))
    stream.text(page_message("source_buffer_added", generation=1, source_buffer=1, mime_type=AUDIO_MIME))
    stream.text(page_message("duration_changed", generation=1, duration={"kind": "finite", "duration_us": 400_000}))
    video_cuts = [0, 500, 3000, 5774, len(video)]
    audio_cuts = [0, 700, 1750, len(audio)]
    for step in range(max(len(video_cuts), len(audio_cuts)) - 1):
        if step + 1 < len(video_cuts):
            stream.frame(1, 0, video[video_cuts[step] : video_cuts[step + 1]])
        if step + 1 < len(audio_cuts):
            stream.frame(1, 1, audio[audio_cuts[step] : audio_cuts[step + 1]])
    stream.text(page_message("end_of_stream", generation=1))
    stream.finish(
        manifest,
        {
            "admitted": [
                {"generation": 1, "source_buffer": 0, "media": "video.mp4", "ranges": [[0, len(video)]]},
                {"generation": 1, "source_buffer": 1, "media": "audio.mp4", "ranges": [[0, len(audio)]]},
            ],
            "dropped_frames": 0,
            "dropped_messages": 0,
        },
    )

    # 2. Реклама → фильм: новое поколение, хвост рекламы приходит после него и выбрасывается.
    stream = StreamWriter("generation-switch-drops-stale")
    stream.text(page_message("generation_started", generation=1))
    stream.text(page_message("source_buffer_added", generation=1, source_buffer=0, mime_type=VIDEO_MIME))
    stream.frame(1, 0, video[:733])
    stream.frame(1, 0, video[733:3000])
    stream.text(page_message("generation_started", generation=2))
    stream.text(page_message("source_buffer_added", generation=2, source_buffer=0, mime_type=VIDEO_MIME))
    stream.text(page_message("source_buffer_added", generation=2, source_buffer=1, mime_type=AUDIO_MIME))
    stream.frame(1, 0, video[3000:5714])  # устаревший хвост рекламы
    stream.text(page_message("end_of_stream", generation=1))  # устаревшее сообщение
    stream.frame(2, 0, video[:4000])
    stream.frame(2, 1, audio)
    stream.text(page_message("seek_started", target_us=200_000, cause={"kind": "page_seek"}))
    stream.frame(2, 0, video[4000:])
    stream.finish(
        manifest,
        {
            "admitted": [
                {"generation": 1, "source_buffer": 0, "media": "video.mp4", "ranges": [[0, 3000]]},
                {"generation": 2, "source_buffer": 0, "media": "video.mp4", "ranges": [[0, len(video)]]},
                {"generation": 2, "source_buffer": 1, "media": "audio.mp4", "ranges": [[0, len(audio)]]},
            ],
            "dropped_frames": 1,
            "dropped_messages": 1,
        },
    )

    # 3. Один SourceBuffer на звук и видео (PeerTube, Twitch live); перемотка назад
    #    с abort: страница заново кладёт файл с начала, и ничего не выбрасывается.
    stream = StreamWriter("muxed-av-single-source-buffer")
    stream.text(page_message("generation_started", generation=1))
    stream.text(page_message("source_buffer_added", generation=1, source_buffer=0, mime_type=MUXED_MIME))
    for start, end in [(0, 200), (200, 1500), (1500, 3000)]:
        stream.frame(1, 0, muxed[start:end])
    stream.text(page_message("seek_started", target_us=0, cause={"kind": "player_seek", "seek_id": 1}))
    stream.text(page_message("source_buffer_aborted", generation=1, source_buffer=0))
    for start, end in [(0, 4000), (4000, len(muxed))]:
        stream.frame(1, 0, muxed[start:end])
    stream.finish(
        manifest,
        {
            "admitted": [
                {
                    "generation": 1,
                    "source_buffer": 0,
                    "media": "muxed.webm",
                    "ranges": [[0, 3000], [0, len(muxed)]],
                }
            ],
            "dropped_frames": 0,
            "dropped_messages": 0,
        },
    )

    # 4. Файл `<video src>`: поколение без SourceBuffer; перемотка, пауза, итог запроса
    #    байтов и уход вкладки проходят, байтов рамок вида 1 нет.
    stream = StreamWriter("file-source-generation")
    stream.text(page_message("generation_started", generation=1))
    stream.text(page_message("file_source_added", generation=1))
    stream.text(page_message("page_playback", request={"kind": "pause"}))
    stream.text(page_message("seek_started", target_us=30_000_000, cause={"kind": "page_seek"}))
    stream.text(
        page_message(
            "range_fetch_finished",
            request_id=1,
            outcome={"kind": "completed", "resource_size": {"kind": "known", "size_bytes": 48_211_005}},
        )
    )
    stream.text(page_message("tab_gone", reason="closed"))
    stream.finish(manifest, {"admitted": [], "dropped_frames": 0, "dropped_messages": 0})

    write_quality_switch_stream(manifest)
    write_hls_stream(manifest, video)


def resource_address(handle: int) -> str:
    """Адрес-заглушка номерка — своя реализация договора, не через Rust."""
    return f"{RESOURCE_ADDRESS_PREFIX}{handle}"


def write_hls_stream(manifest: Manifest, video: bytes) -> None:
    """6. Нативный HLS (протокол 1.1, решение 228): поколение без SourceBuffer.

    Страница объявляет HLS-поколение с корневым плейлистом №0 и отвечает на запросы
    ресурсов плеера (`fetch_resource` в поток не записан — это слова плеера):
    тело каждого ответа — рамки вида 2 со смещениями ВНУТРИ тела с 0, затем итог.
    Плейлист уже переписан фоном: вместо адресов — адреса-заглушки номерков.
    Запрос 3 — кусок файла (`EXT-X-BYTERANGE`): тело всё равно считается с 0.
    """
    playlist = "\n".join(
        [
            "#EXTM3U",
            "#EXT-X-VERSION:7",
            "#EXT-X-TARGETDURATION:1",
            "#EXT-X-PLAYLIST-TYPE:VOD",
            f'#EXT-X-MAP:URI="{resource_address(1)}"',
            "#EXTINF:0.4,",
            f"#EXT-X-BYTERANGE:{len(video) - 733}@733",
            resource_address(2),
            "#EXT-X-ENDLIST",
            "",
        ]
    ).encode()
    init_end = 733  # граница moov/moof у video.mp4 (как в потоке 1)
    stream = StreamWriter("hls-source-generation")
    playlist_source = stream.side_file("root.m3u8", playlist)
    stream.text(page_message("generation_started", generation=1))
    stream.text(page_message("hls_source_added", generation=1, root=0))
    # Запрос 1 — корневой плейлист, разрезан посреди строки.
    stream.range_bytes(1, 0, playlist[:40])
    stream.range_bytes(1, 40, playlist[40:])
    stream.text(resource_finished(1, {"kind": "completed", "size_bytes": len(playlist)}))
    # Запросы 2 (init) и 3 (сегмент) идут параллельно: рамки перемежаются.
    segment = video[init_end:]
    stream.range_bytes(2, 0, video[:500])
    stream.range_bytes(3, 0, segment[:2000])
    stream.range_bytes(2, 500, video[500:init_end])
    stream.text(resource_finished(2, {"kind": "completed", "size_bytes": init_end}))
    stream.range_bytes(3, 2000, segment[2000:])
    stream.text(resource_finished(3, {"kind": "completed", "size_bytes": len(segment)}))
    # Запрос 4 — плейлист, которого у сервера нет: честный отказ без тела.
    stream.text(resource_failed(4, {"kind": "http_error", "status": 404}))
    stream.text(page_message("seek_started", target_us=200_000, cause={"kind": "page_seek"}))
    stream.text(page_message("tab_gone", reason="closed"))
    stream.finish(
        manifest,
        {
            "admitted": [],
            "dropped_frames": 0,
            "dropped_messages": 0,
            "resource_bodies": [
                {"request_id": 1, "source": playlist_source, "ranges": [[0, len(playlist)]]},
                {"request_id": 2, "source": "media/video.mp4", "ranges": [[0, init_end]]},
                {"request_id": 3, "source": "media/video.mp4", "ranges": [[init_end, len(video)]]},
            ],
        },
    )


def write_resource_addresses() -> None:
    """Договор адреса-заглушки (crate::resource_address): верные и неверные адреса."""
    valid = [
        {"address": resource_address(handle), "handle": handle} for handle in [0, 1, 7, 42, 1_000_000, U32_MAX]
    ]
    invalid = [
        "",
        RESOURCE_ADDRESS_PREFIX,
        RESOURCE_ADDRESS_PREFIX + "007",
        RESOURCE_ADDRESS_PREFIX + "00",
        RESOURCE_ADDRESS_PREFIX + "+7",
        RESOURCE_ADDRESS_PREFIX + "-7",
        RESOURCE_ADDRESS_PREFIX + " 7",
        RESOURCE_ADDRESS_PREFIX + "7 ",
        RESOURCE_ADDRESS_PREFIX + "7.0",
        RESOURCE_ADDRESS_PREFIX + "1e3",
        RESOURCE_ADDRESS_PREFIX + "0x10",
        RESOURCE_ADDRESS_PREFIX + "%37",
        RESOURCE_ADDRESS_PREFIX + "٣",
        RESOURCE_ADDRESS_PREFIX + str(U32_MAX + 1),
        RESOURCE_ADDRESS_PREFIX + "99999999999999999999",
        RESOURCE_ADDRESS_PREFIX + "7/",
        RESOURCE_ADDRESS_PREFIX + "7/8",
        RESOURCE_ADDRESS_PREFIX + "7?",
        RESOURCE_ADDRESS_PREFIX + "7?token=1",
        RESOURCE_ADDRESS_PREFIX + "7#",
        RESOURCE_ADDRESS_PREFIX + "7#frag",
        RESOURCE_ADDRESS_PREFIX + "seg7.ts",
        "https://resource.fastiplayer-bridge.invalid:443/7",
        "https://resource.fastiplayer-bridge.invalid./7",
        "https://user@resource.fastiplayer-bridge.invalid/7",
        "https://user:pass@resource.fastiplayer-bridge.invalid/7",
        "https://RESOURCE.FASTIPLAYER-BRIDGE.INVALID/7",
        "HTTPS://resource.fastiplayer-bridge.invalid/7",
        "http://resource.fastiplayer-bridge.invalid/7",
        "wss://resource.fastiplayer-bridge.invalid/7",
        "//resource.fastiplayer-bridge.invalid/7",
        "resource.fastiplayer-bridge.invalid/7",
        "https://evil.example/https://resource.fastiplayer-bridge.invalid/7",
        "https://resource.fastiplayer-bridge.invalid.evil.example/7",
        "https://sub.resource.fastiplayer-bridge.invalid/7",
        "https://resource.fastiplayer-bridge.invalid//7",
        "https://resource.fastiplayer-bridge.invalid/./7",
        " https://resource.fastiplayer-bridge.invalid/7",
        "https://resource.fastiplayer-bridge.invalid/7\n",
        "https://example.com/7",
    ]
    (FIXTURES_ROOT / "resource-addresses.json").write_bytes(
        json.dumps({"valid": valid, "invalid": invalid}, ensure_ascii=False, indent=2).encode() + b"\n"
    )


# В какой версии протокола появилось каждое сообщение сеанса (crate::version, правило 5).
# Сообщение новее общей версии сеанса получатель отвергает, отправитель не шлёт.
MESSAGE_VERSIONS: dict[str, dict[str, str]] = {
    "page_session": {
        **{
            name: "1.0"
            for name in [
                "attach",
                "generation_started",
                "file_source_added",
                "source_buffer_added",
                "source_buffer_type_changed",
                "timestamp_offset_changed",
                "source_buffer_removed",
                "source_buffer_aborted",
                "seek_started",
                "duration_changed",
                "end_of_stream",
                "page_playback",
                "variant_menu_changed",
                "ad_state",
                "range_fetch_finished",
                "tab_gone",
            ]
        },
        "hls_source_added": "1.1",
        "resource_fetch_finished": "1.1",
        "page_left": "1.4",
        "page_returned": "1.4",
        "page_reopen_failed": "1.4",
        "subtitle_track_offered": "1.5",
        "page_navigation": "1.7",
        "subtitle_preference": "1.7",
        "variant_outcome": "1.8",
        "awaiting_next_episode": "1.9",
        "page_title_changed": "1.9",
    },
    "player_session": {
        **{
            name: "1.0"
            for name in [
                "attached",
                "clock",
                "buffer_state",
                "play",
                "pause",
                "seek",
                "set_rate",
                "return_to_page",
                "select_variant",
                "next_episode",
                "skip_ad",
                "fetch_range",
                "stream_rejected",
            ]
        },
        "fetch_resource": "1.1",
        "live_window": "1.2",
        "recording_window": "1.11",
        "reopen_page": "1.4",
        "show_page": "1.6",
        "subtitle_choice": "1.7",
    },
}


def write_message_versions() -> None:
    (FIXTURES_ROOT / "message-versions.json").write_bytes(
        json.dumps(MESSAGE_VERSIONS, ensure_ascii=False, indent=2).encode() + b"\n"
    )


# В какой версии протокола появилось каждое ЗНАЧЕНИЕ старого сообщения, у которого
# значения добавлялись (с протокола 1.3). Ключ — «сообщение.поле»; список значений полный:
# новое значение без версии — ошибка, которую ловят тесты обеих сторон. Значение новее
# общей версии сеанса получатель отвергает, отправитель не шлёт (решение 237).
VALUE_VERSIONS: dict[str, dict[str, str]] = {
    "tab_gone.reason": {
        "closed": "1.0",
        "navigated_away": "1.0",
        "browser_closing": "1.0",
        "page_player_failed": "1.3",
    },
    # Значения page_left.reason - те же TabGoneReason, их версии в tab_gone.reason выше.
    "page_reopen_failed.reason": {
        "no_browser_window": "1.4",
        "page_did_not_load": "1.4",
        "not_the_same_video": "1.4",
        "video_did_not_start": "1.4",
    },
}


def write_value_versions() -> None:
    (FIXTURES_ROOT / "value-versions.json").write_bytes(
        json.dumps(VALUE_VERSIONS, ensure_ascii=False, indent=2).encode() + b"\n"
    )


# В какой версии протокола появилось каждое НЕОБЯЗАТЕЛЬНОЕ ПОЛЕ старого сообщения (с 1.10).
# Сторона → «сообщение.поле» → версия. Сообщение с таким полем требует общей версии не ниже
# версии поля (`required_version` обеих сторон), без поля — версии своего типа. Версии здесь
# текстом, а сравнивать их надо ЧИСЛАМИ: "1.10" как строка меньше "1.9".
FIELD_VERSIONS: dict[str, dict[str, str]] = {
    "page_session": {},
    "player_session": {
        # Сессия 16a2, решение 311: номер поколения, которое описывает тик часов.
        "clock.generation": "1.10",
    },
}


def write_field_versions() -> None:
    (FIXTURES_ROOT / "field-versions.json").write_bytes(
        json.dumps(FIELD_VERSIONS, ensure_ascii=False, indent=2).encode() + b"\n"
    )


def write_quality_switch_stream(manifest: Manifest) -> None:
    """5. Смена качества новым MediaSource три раза подряд (сессия 03e).

    Образец — живая запись YouTube (results/03e.md): каждая смена — новое поколение;
    страница объявляет поколение и SourceBuffer, кладёт init-сегменты, ставит
    `currentTime` на позицию зрителя (`seek_started{page_seek}`) и только потом кладёт
    коробки — видео с ключевого кадра до позиции, звук с ещё более раннего фрагмента.
    Позиция зрителя чуть впереди часов плеера на момент смены: старт на позиции
    страницы не отбрасывает плеер назад.
    """
    qualities = {
        64: (MEDIA_ROOT / "video-64-6s.mp4").read_bytes(),
        96: (MEDIA_ROOT / "video-96-6s.mp4").read_bytes(),
    }
    audio = (MEDIA_ROOT / "audio-6s.mp4").read_bytes()
    audio_init_end, audio_fragments = split_fragments(audio)
    duration_us = 6_000_000
    # (поколение, качество, ждать часов, позиция страницы, конец того, что кладём)
    # Ключевые кадры видео — каждые 1,2 с, поэтому коробки нового поколения начинаются
    # почти на секунду раньше позиции страницы: старт «с первой коробки» вместо позиции
    # отбросил бы часы назад заметно больше задержки установки поколения.
    switches = [
        (1, 64, None, 0, 2_400_000),
        (2, 96, 1_800_000, 2_200_000, 3_600_000),
        (3, 64, 3_200_000, 3_500_000, 4_800_000),
        (4, 96, 4_400_000, 4_700_000, duration_us),
    ]
    stream = StreamWriter("quality-switch-new-media-source")
    admitted: list[dict[str, Any]] = []
    for generation, quality, await_us, page_position_us, end_us in switches:
        video = qualities[quality]
        video_init_end, video_fragments = split_fragments(video)
        if await_us is not None:
            stream.await_clock(await_us)
        stream.text(page_message("generation_started", generation=generation))
        stream.text(
            page_message("duration_changed", generation=generation, duration={"kind": "finite", "duration_us": duration_us})
        )
        stream.text(page_message("source_buffer_added", generation=generation, source_buffer=0, mime_type=AUDIO_MIME))
        stream.text(page_message("source_buffer_added", generation=generation, source_buffer=1, mime_type=VIDEO_MIME))
        stream.frame(generation, 0, audio[:audio_init_end])
        stream.frame(generation, 1, video[:video_init_end])
        if generation > 1:
            stream.text(page_message("seek_started", target_us=page_position_us, cause={"kind": "page_seek"}))
        # Звук — с фрагмента на целый шаг раньше ключевого кадра видео (как YouTube).
        video_part = fragments_from(video_fragments, page_position_us, end_us)
        audio_part = fragments_from(audio_fragments, video_part[0].start_us, end_us)
        for fragment in audio_part:
            stream.frame(generation, 0, audio[fragment.offset : fragment.end])
        for fragment in video_part:
            stream.frame(generation, 1, video[fragment.offset : fragment.end])
        if end_us == duration_us:
            stream.text(page_message("end_of_stream", generation=generation))
        admitted.append(
            {
                "generation": generation,
                "source_buffer": 0,
                "media": "audio-6s.mp4",
                "ranges": [[0, audio_init_end], [audio_part[0].offset, audio_part[-1].end]],
            }
        )
        admitted.append(
            {
                "generation": generation,
                "source_buffer": 1,
                "media": f"video-{quality}-6s.mp4",
                "ranges": [[0, video_init_end], [video_part[0].offset, video_part[-1].end]],
            }
        )
    stream.finish(manifest, {"admitted": admitted, "dropped_frames": 0, "dropped_messages": 0})


# Значение-пометка «убрать поле» для `subtitle_offer` (`None` — это JSON `null`).
OMIT_FIELD = object()


def subtitle_offer(**overrides: Any) -> dict[str, Any]:
    """`subtitle_track_offered` (протокол 1.5) с полями по умолчанию; `OMIT_FIELD` убирает поле."""
    fields: dict[str, Any] = {
        "track": 1,
        "language": {"kind": "known", "tag": "en"},
        "label": {"kind": "text", "text": "English"},
        "format_hint": "web_vtt",
        "origin": "page_download",
        "page_display": "showing",
        "byte_length": 1234,
    }
    fields.update(overrides)
    return page_message("subtitle_track_offered", **{name: value for name, value in fields.items() if value is not OMIT_FIELD})


def write_page_subtitle_messages(manifest: Manifest) -> None:
    """Заголовок документа субтитров страницы `subtitle_track_offered` (протокол 1.5).

    Необязательные язык и подпись — метки (`{"kind":"unknown"}`, `{"kind":"none"}`), а не
    отсутствующие поля; все поля обязательны. Тег языка — форма BCP-47 (буквы, цифры,
    дефис между частями, 1…35 байт), подпись — текст для человека до 256 байт.
    """
    schema = "page_session"
    valid = "session/valid/page-subtitle-track-offered-"
    longest_label = "ї" * (MAX_SUBTITLE_LABEL_BYTES // 2)
    for name, message in [
        ("full", subtitle_offer()),
        (
            "unknown-language-no-label",
            subtitle_offer(
                track=2,
                language={"kind": "unknown"},
                label={"kind": "none"},
                format_hint="srv3",
                origin="text_track_cues",
                page_display="not_showing",
                byte_length=1,
            ),
        ),
        (
            "max-fields",
            subtitle_offer(
                track=U32_MAX,
                language={"kind": "known", "tag": "zh-Hant-TW"},
                label={"kind": "text", "text": longest_label},
                format_hint="json3",
                origin="track_element",
                byte_length=MAX_PAGE_SUBTITLE_DOCUMENT_BYTES,
            ),
        ),
        ("format-srt", subtitle_offer(format_hint="srt")),
        ("format-ttml", subtitle_offer(format_hint="ttml", language={"kind": "known", "tag": "pt-BR"})),
        ("format-unknown", subtitle_offer(format_hint="unknown", label={"kind": "text", "text": "Українська 🎬"})),
    ]:
        manifest.valid(f"{valid}{name}.json", schema, message)

    invalid = "session/invalid/page-subtitle-track-offered-"
    too_long_tag = "a" * (MAX_SUBTITLE_LANGUAGE_TAG_BYTES + 1)
    too_long_label = longest_label + "a"
    for name, message, kind in [
        # Форма сообщения строгая: лишнее поле и лишнее поле внутри метки — ошибки.
        ("unknown-field", subtitle_offer(url="https://example.com/subs.vtt"), "unknown_field"),
        ("language-extra-field", subtitle_offer(language={"kind": "known", "tag": "en", "script": "Latn"}), "unknown_field"),
        ("language-unknown-with-tag", subtitle_offer(language={"kind": "unknown", "tag": "en"}), "unknown_field"),
        ("label-none-with-text", subtitle_offer(label={"kind": "none", "text": "English"}), "unknown_field"),
        ("language-missing", subtitle_offer(language=OMIT_FIELD), "missing_field"),
        ("label-missing", subtitle_offer(label=OMIT_FIELD), "missing_field"),
        ("byte-length-missing", subtitle_offer(byte_length=OMIT_FIELD), "missing_field"),
        ("language-tag-missing", subtitle_offer(language={"kind": "known"}), "missing_field"),
        ("language-kind-unknown", subtitle_offer(language={"kind": "guessed", "tag": "en"}), "unknown_value"),
        # Тег языка: пустой, длинный, чужие символы, дефис не между частями.
        ("language-tag-empty", subtitle_offer(language={"kind": "known", "tag": ""}), "text_empty"),
        ("language-tag-too-long", subtitle_offer(language={"kind": "known", "tag": too_long_tag}), "text_too_long"),
        ("language-tag-underscore", subtitle_offer(language={"kind": "known", "tag": "en_US"}), "text_forbidden_character"),
        ("language-tag-space", subtitle_offer(language={"kind": "known", "tag": "en US"}), "text_forbidden_character"),
        ("language-tag-cyrillic", subtitle_offer(language={"kind": "known", "tag": "ук"}), "text_forbidden_character"),
        ("language-tag-leading-dash", subtitle_offer(language={"kind": "known", "tag": "-en"}), "text_forbidden_character"),
        ("language-tag-trailing-dash", subtitle_offer(language={"kind": "known", "tag": "en-"}), "text_forbidden_character"),
        ("language-tag-double-dash", subtitle_offer(language={"kind": "known", "tag": "en--US"}), "text_forbidden_character"),
        # Подпись: пустой нет (её отсутствие — метка `none`), длиннее 256 байт и управляющие — нет.
        ("label-empty", subtitle_offer(label={"kind": "text", "text": ""}), "text_empty"),
        ("label-too-long", subtitle_offer(label={"kind": "text", "text": too_long_label}), "text_too_long"),
        ("label-line-break", subtitle_offer(label={"kind": "text", "text": "Eng\nlish"}), "text_forbidden_character"),
        ("label-bidi-override", subtitle_offer(label={"kind": "text", "text": "spoof‮lmth.exe"}), "text_forbidden_character"),
        # Числа: длина 1…8 МиБ, номер дорожки 1…u32.
        ("byte-length-zero", subtitle_offer(byte_length=0), "number_out_of_range"),
        ("byte-length-above-limit", subtitle_offer(byte_length=MAX_PAGE_SUBTITLE_DOCUMENT_BYTES + 1), "number_out_of_range"),
        ("byte-length-negative", subtitle_offer(byte_length=-1), "number_out_of_range"),
        ("byte-length-fractional", subtitle_offer(byte_length=10.5), "wrong_value_type"),
        ("byte-length-string", subtitle_offer(byte_length="1234"), "wrong_value_type"),
        ("track-zero", subtitle_offer(track=0), "number_out_of_range"),
        ("track-beyond-u32", subtitle_offer(track=U32_MAX + 1), "number_out_of_range"),
        # Метки строгие, без нормализации.
        ("format-hint-unknown", subtitle_offer(format_hint="vtt"), "unknown_value"),
        ("format-hint-upper-case", subtitle_offer(format_hint="WebVTT"), "unknown_value"),
        ("format-hint-with-dashes", subtitle_offer(format_hint="web-vtt"), "unknown_value"),
        ("origin-unknown", subtitle_offer(origin="network"), "unknown_value"),
        ("page-display-unknown", subtitle_offer(page_display="hidden"), "unknown_value"),
        # Чужая форма значения — ошибка формы (`crate::strict_shape`).
        ("language-null", subtitle_offer(language=None), "wrong_value_type"),
        ("language-as-string", subtitle_offer(language="en"), "wrong_value_type"),
        ("language-as-array", subtitle_offer(language=["known", "en"]), "wrong_value_type"),
        ("language-kind-as-index", subtitle_offer(language={"kind": 1, "tag": "en"}), "wrong_value_type"),
        ("label-as-string", subtitle_offer(label="English"), "wrong_value_type"),
        ("format-hint-as-map", subtitle_offer(format_hint={"web_vtt": None}), "wrong_value_type"),
        ("format-hint-as-index", subtitle_offer(format_hint=0), "wrong_value_type"),
        ("page-display-as-map", subtitle_offer(page_display={"showing": None}), "wrong_value_type"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", schema, compact_json(message), kind)


def write_variant_protocol_messages(manifest: Manifest) -> None:
    """Сообщения протокола 1.7 (сессия 14a): `page_navigation`, `subtitle_preference`, `subtitle_choice`.

    Выбор субтитров `choice` — метка с видом: `{"kind":"page_track","track":N}` (N от 1) или
    `{"kind":"off"}`. Форма строгая: лишнее поле, чужой вид и номер 0 — ошибки.
    """
    page_valid = "session/valid/page-"
    page_invalid = "session/invalid/page-"
    player_valid = "session/valid/player-"
    player_invalid = "session/invalid/player-"
    page_track = {"kind": "page_track", "track": 3}
    off = {"kind": "off"}
    for name, message in [
        ("navigation-available", page_message("page_navigation", next_episode="available")),
        ("navigation-unavailable", page_message("page_navigation", next_episode="unavailable")),
        ("subtitle-preference-page-track", page_message("subtitle_preference", choice=page_track)),
        ("subtitle-preference-max-track", page_message("subtitle_preference", choice={"kind": "page_track", "track": U32_MAX})),
        ("subtitle-preference-off", page_message("subtitle_preference", choice=off)),
    ]:
        manifest.valid(f"{page_valid}{name}.json", "page_session", message)
    for name, message in [
        ("subtitle-choice-page-track", page_message("subtitle_choice", choice=page_track)),
        ("subtitle-choice-off", page_message("subtitle_choice", choice=off)),
    ]:
        manifest.valid(f"{player_valid}{name}.json", "player_session", message)

    for name, message, kind in [
        ("navigation-unknown-field", page_message("page_navigation", next_episode="available", url="https://example.com/2"), "unknown_field"),
        ("navigation-missing-field", page_message("page_navigation"), "missing_field"),
        ("navigation-unknown-value", page_message("page_navigation", next_episode="maybe"), "unknown_value"),
        ("navigation-as-bool", page_message("page_navigation", next_episode=True), "wrong_value_type"),
        ("subtitle-preference-unknown-field", page_message("subtitle_preference", choice=off, remember=True), "unknown_field"),
        ("subtitle-preference-missing-choice", page_message("subtitle_preference"), "missing_field"),
        ("subtitle-preference-choice-extra-field", page_message("subtitle_preference", choice={"kind": "off", "track": 1}), "unknown_field"),
        ("subtitle-preference-page-track-extra-field", page_message("subtitle_preference", choice={"kind": "page_track", "track": 1, "label": "x"}), "unknown_field"),
        ("subtitle-preference-page-track-missing-track", page_message("subtitle_preference", choice={"kind": "page_track"}), "missing_field"),
        ("subtitle-preference-track-zero", page_message("subtitle_preference", choice={"kind": "page_track", "track": 0}), "number_out_of_range"),
        ("subtitle-preference-track-beyond-u32", page_message("subtitle_preference", choice={"kind": "page_track", "track": U32_MAX + 1}), "number_out_of_range"),
        ("subtitle-preference-track-string", page_message("subtitle_preference", choice={"kind": "page_track", "track": "3"}), "wrong_value_type"),
        ("subtitle-preference-kind-unknown", page_message("subtitle_preference", choice={"kind": "own_file"}), "unknown_value"),
        ("subtitle-preference-choice-as-string", page_message("subtitle_preference", choice="off"), "wrong_value_type"),
    ]:
        manifest.invalid(f"{page_invalid}{name}.json", "page_session", compact_json(message), kind)
    for name, message, kind in [
        ("subtitle-choice-unknown-field", page_message("subtitle_choice", choice=off, auto=True), "unknown_field"),
        ("subtitle-choice-missing-choice", page_message("subtitle_choice"), "missing_field"),
        ("subtitle-choice-track-zero", page_message("subtitle_choice", choice={"kind": "page_track", "track": 0}), "number_out_of_range"),
        ("subtitle-choice-kind-unknown", page_message("subtitle_choice", choice={"kind": "embedded", "track": 1}), "unknown_value"),
        ("subtitle-choice-choice-extra-field", page_message("subtitle_choice", choice={"kind": "off", "track": 1}), "unknown_field"),
    ]:
        manifest.invalid(f"{player_invalid}{name}.json", "player_session", compact_json(message), kind)


def write_variant_outcome_messages(manifest: Manifest) -> None:
    """Сообщение протокола 1.8 (сессия 15c2): `variant_outcome` — исход выбора зрителя на странице.

    `request` — метка с видом: `{"kind":"select_variant","menu":<вид меню>,"variant_id":<пункт>}`
    или `{"kind":"next_episode"}`; `outcome` — `done | no_such_episode | person_only | failed`.
    Форма строгая: лишнее поле, чужой вид просьбы, пустой пункт и незнакомый исход — ошибки.
    """
    valid = "session/valid/page-variant-outcome-"
    invalid = "session/invalid/page-variant-outcome-"
    select_dub = {"kind": "select_variant", "menu": "audio", "variant_id": "dub-b"}
    next_episode = {"kind": "next_episode"}
    for name, message in [
        ("select-done", page_message("variant_outcome", request={"kind": "select_variant", "menu": "quality", "variant_id": "q1_a7"}, outcome="done")),
        ("select-no-such-episode", page_message("variant_outcome", request=select_dub, outcome="no_such_episode")),
        ("select-person-only", page_message("variant_outcome", request={"kind": "select_variant", "menu": "subtitles", "variant_id": "x-uk"}, outcome="person_only")),
        ("next-done", page_message("variant_outcome", request=next_episode, outcome="done")),
        ("next-failed", page_message("variant_outcome", request=next_episode, outcome="failed")),
    ]:
        manifest.valid(f"{valid}{name}.json", "page_session", message)

    for name, message, kind in [
        ("unknown-field", page_message("variant_outcome", request=next_episode, outcome="done", label="Dub B"), "unknown_field"),
        ("missing-outcome", page_message("variant_outcome", request=next_episode), "missing_field"),
        ("missing-request", page_message("variant_outcome", outcome="done"), "missing_field"),
        ("unknown-outcome", page_message("variant_outcome", request=select_dub, outcome="ignored"), "unknown_value"),
        ("outcome-as-bool", page_message("variant_outcome", request=select_dub, outcome=True), "wrong_value_type"),
        ("request-kind-unknown", page_message("variant_outcome", request={"kind": "skip_ad"}, outcome="done"), "unknown_value"),
        ("request-as-string", page_message("variant_outcome", request="next_episode", outcome="done"), "wrong_value_type"),
        ("select-missing-menu", page_message("variant_outcome", request={"kind": "select_variant", "variant_id": "dub-b"}, outcome="done"), "missing_field"),
        ("select-menu-unknown", page_message("variant_outcome", request={"kind": "select_variant", "menu": "chapters", "variant_id": "dub-b"}, outcome="done"), "unknown_value"),
        ("select-empty-variant-id", page_message("variant_outcome", request={"kind": "select_variant", "menu": "audio", "variant_id": ""}, outcome="done"), "text_empty"),
        ("select-extra-field", page_message("variant_outcome", request={**select_dub, "label": "Dub B"}, outcome="done"), "unknown_field"),
        ("next-extra-field", page_message("variant_outcome", request={"kind": "next_episode", "menu": "audio"}, outcome="done"), "unknown_field"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", "page_session", compact_json(message), kind)


def write_episode_change_messages(manifest: Manifest) -> None:
    """Сообщения протокола 1.9 (сессия 16a): смена серии на той же странице.

    `awaiting_next_episode {patience_ms}` — срок ожидания нового источника, любое `u32`
    миллисекунд (отрицательное и больше `u32` — `number_out_of_range`, дробное и строка —
    `wrong_value_type`). `page_title_changed {title}` — те же правила, что у `title` в
    `open_session`: до 1024 байт UTF-8, без управляющих символов и переворота направления,
    ПУСТОЙ ДОПУСТИМ (страница без `<title>`). Форма строгая: лишнее поле — `unknown_field`.
    """
    valid = "session/valid/page-"
    invalid = "session/invalid/page-"
    title_at_limit = "ї" * (MAX_PAGE_TITLE_BYTES // 2)  # «ї» — 2 байта UTF-8: ровно 1024 байта
    for name, message in [
        ("awaiting-next-episode-60s", page_message("awaiting_next_episode", patience_ms=60_000)),
        ("awaiting-next-episode-zero", page_message("awaiting_next_episode", patience_ms=0)),
        ("awaiting-next-episode-u32-max", page_message("awaiting_next_episode", patience_ms=U32_MAX)),
        ("title-changed-episode", page_message("page_title_changed", title="Шрек 2 — серія 4 🎬")),
        ("title-changed-rtl", page_message("page_title_changed", title="بث مباشر — שידור חי")),
        ("title-changed-untitled", page_message("page_title_changed", title="")),
        ("title-changed-at-limit", page_message("page_title_changed", title=title_at_limit)),
    ]:
        manifest.valid(f"{valid}{name}.json", "page_session", message)

    for name, message, kind in [
        ("awaiting-next-episode-unknown-field", page_message("awaiting_next_episode", patience_ms=60_000, title="x"), "unknown_field"),
        ("awaiting-next-episode-missing-patience", page_message("awaiting_next_episode"), "missing_field"),
        ("awaiting-next-episode-patience-as-string", page_message("awaiting_next_episode", patience_ms="60000"), "wrong_value_type"),
        ("awaiting-next-episode-patience-as-null", page_message("awaiting_next_episode", patience_ms=None), "wrong_value_type"),
        ("awaiting-next-episode-patience-fractional", page_message("awaiting_next_episode", patience_ms=1.5), "wrong_value_type"),
        ("awaiting-next-episode-patience-negative", page_message("awaiting_next_episode", patience_ms=-1), "number_out_of_range"),
        ("awaiting-next-episode-patience-beyond-u32", page_message("awaiting_next_episode", patience_ms=U32_MAX + 1), "number_out_of_range"),
        ("title-changed-unknown-field", page_message("page_title_changed", title="Серія 4", domain="example.com"), "unknown_field"),
        ("title-changed-missing-title", page_message("page_title_changed"), "missing_field"),
        ("title-changed-title-as-number", page_message("page_title_changed", title=4), "wrong_value_type"),
        ("title-changed-title-as-null", page_message("page_title_changed", title=None), "wrong_value_type"),
        ("title-changed-too-long", page_message("page_title_changed", title=title_at_limit + "x"), "text_too_long"),
        ("title-changed-with-bidi-override", page_message("page_title_changed", title="movie\u202e4pm.exe"), "text_forbidden_character"),
        ("title-changed-with-nul", page_message("page_title_changed", title="a\u0000b"), "text_forbidden_character"),
        ("title-changed-with-newline", page_message("page_title_changed", title="Серія 4\nОзвучка"), "text_forbidden_character"),
    ]:
        manifest.invalid(f"{invalid}{name}.json", "page_session", compact_json(message), kind)


def gate_text(message: dict[str, Any]) -> dict[str, str]:
    """Шаг случая сторожа: текстовая рамка — ровно байты сообщения на проводе."""
    return {"text": compact_json(message).decode("utf-8")}


def gate_binary(frame: bytes) -> dict[str, str]:
    """Шаг случая сторожа: двоичная рамка шестнадцатеричной строкой."""
    return {"binary_hex": frame.hex()}


def gate_ok(name: str, steps: list[dict[str, str]], documents: list[tuple[int, bytes]], agreed: str = "1.5") -> dict[str, Any]:
    """Случай, который сторож принимает целиком; `documents` — готовые документы по дорожкам."""
    return {
        "name": name,
        "agreed": agreed,
        "steps": steps,
        "expect": {
            "result": "ok",
            "documents": [{"track": track, "utf8": content.decode("utf-8")} for track, content in documents],
        },
    }


def gate_violation(name: str, steps: list[dict[str, str]], violation: str, agreed: str = "1.5") -> dict[str, Any]:
    """Случай, где ПОСЛЕДНИЙ шаг — нарушение (сеанс закрывается 4003), а все до него приняты."""
    return {
        "name": name,
        "agreed": agreed,
        "steps": steps,
        "expect": {"result": "violation", "at_step": len(steps) - 1, "violation": violation},
    }


def write_page_subtitle_gate_cases() -> None:
    """Правила сторожа для субтитров страницы (протокол 1.5) — общие случаи порядка.

    В отличие от `streams/` здесь есть и отвергаемые последовательности: каждое правило
    договора — свой случай. Шаги — байты на проводе (JSON-текст или рамка в hex), имена
    нарушений — `StreamViolation` в snake_case. Сторож есть только у плеера (Rust); TypeScript
    сверяет, что каждый шаг разбирается его кодеками.
    """
    first = "WEBVTT\n\n00:00.000 --> 00:01.000\nПривіт\n".encode()
    second = "WEBVTT\n\n00:01.000 --> 00:02.000\nHello\n".encode()
    half = len(first) // 2

    def offer(track: int, document: bytes, **overrides: Any) -> dict[str, str]:
        return gate_text(subtitle_offer(track=track, byte_length=len(document), **overrides))

    def piece(track: int, offset: int, payload: bytes) -> dict[str, str]:
        return gate_binary(page_subtitle_frame(track, offset, payload))

    whole_first = [offer(1, first), piece(1, 0, first[:half]), piece(1, half, first[half:])]
    page_left = gate_text(page_message("page_left", reason="closed"))
    cases = [
        gate_ok("document-in-two-frames", whole_first, [(1, first)]),
        gate_ok(
            # Документ не трогает поколений: MSE идёт своим чередом между кусками.
            "document-between-media-source-bytes",
            [
                gate_text(page_message("generation_started", generation=1)),
                gate_text(page_message("source_buffer_added", generation=1, source_buffer=0, mime_type=VIDEO_MIME)),
                offer(1, first),
                gate_binary(source_buffer_frame(1, 0, 0, b"\x00\x00\x00\x08free")),
                piece(1, 0, first[:half]),
                gate_text(page_message("generation_started", generation=2)),
                piece(1, half, first[half:]),
            ],
            [(1, first)],
        ),
        gate_ok(
            "two-tracks-interleaved",
            [offer(1, first), offer(2, second), piece(2, 0, second), piece(1, 0, first)],
            [(1, first), (2, second)],
        ),
        gate_ok(
            "re-offer-replaces-partial",
            [offer(1, first), piece(1, 0, first[:half]), offer(1, second), piece(1, 0, second)],
            [(1, second)],
        ),
        gate_ok("re-offer-replaces-complete", [*whole_first, offer(1, second), piece(1, 0, second)], [(1, second)]),
        gate_ok(
            "max-tracks-and-replacing-one",
            [*[offer(track, b"x") for track in range(1, MAX_PAGE_SUBTITLE_TRACKS + 1)], offer(1, first), piece(1, 0, first)],
            [(1, first)],
        ),
        gate_violation("frame-for-unknown-track", [offer(1, first), piece(2, 0, first)], "page_subtitle_track_not_offered"),
        gate_violation("frame-before-any-offer", [piece(1, 0, first)], "page_subtitle_track_not_offered"),
        gate_violation("frame-for-complete-document", [*whole_first, piece(1, len(first), b"x")], "page_subtitle_document_complete"),
        gate_violation("offset-gap", [offer(1, first), piece(1, 0, first[:half]), piece(1, half + 1, first[half + 1 :])], "page_subtitle_offset_gap"),
        gate_violation("offset-repeat", [offer(1, first), piece(1, 0, first[:half]), piece(1, 0, first[:half])], "page_subtitle_offset_gap"),
        gate_violation("offset-after-re-offer-continues-old", [offer(1, first), piece(1, 0, first[:half]), offer(1, first), piece(1, half, first[half:])], "page_subtitle_offset_gap"),
        gate_violation("overflow-past-byte-length", [offer(1, first), piece(1, 0, first + b"x")], "page_subtitle_beyond_byte_length"),
        gate_violation(
            "sixty-fifth-track",
            [offer(track, b"x") for track in range(1, MAX_PAGE_SUBTITLE_TRACKS + 2)],
            "too_many_page_subtitle_tracks",
        ),
        gate_violation("offer-under-1-4", [offer(1, first)], "message_not_in_agreed_version", agreed="1.4"),
        gate_violation("frame-under-1-4", [piece(1, 0, first)], "data_frame_not_in_agreed_version", agreed="1.4"),
        gate_violation("offer-while-page-away", [page_left, offer(1, first)], "page_is_away"),
        gate_violation("frame-while-page-away", [offer(1, first), page_left, piece(1, 0, first)], "page_is_away"),
    ]
    (FIXTURES_ROOT / "page-subtitle-gate.json").write_bytes(
        json.dumps({"cases": cases}, ensure_ascii=False, indent=2).encode() + b"\n"
    )


def main() -> None:
    for directory in GENERATED_DIRECTORIES:
        shutil.rmtree(FIXTURES_ROOT / directory, ignore_errors=True)
    manifest = Manifest()
    write_extension_control(manifest)
    write_player_control(manifest)
    write_page_session(manifest)
    write_player_session(manifest)
    write_data_frames(manifest)
    write_shape_strictness(manifest)
    write_page_subtitle_messages(manifest)
    write_variant_protocol_messages(manifest)
    write_variant_outcome_messages(manifest)
    write_episode_change_messages(manifest)
    write_page_subtitle_gate_cases()
    write_streams(manifest)
    write_resource_addresses()
    write_message_versions()
    write_value_versions()
    write_field_versions()
    (FIXTURES_ROOT / "manifest.json").write_bytes(
        json.dumps({"cases": manifest.cases, "streams": manifest.streams}, ensure_ascii=False, indent=2).encode()
        + b"\n"
    )
    print(f"cases: {len(manifest.cases)}, streams: {len(manifest.streams)}")


if __name__ == "__main__":
    main()
