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
стирает и пересоздаёт control/, session/, data/, streams/ и manifest.json.
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
MAX_JSON_SAFE_INTEGER = (1 << 53) - 1

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
    ]:
        manifest.valid(f"{valid}{name}.json", schema, message)

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
        ("fetch-range", {"type": "fetch_range", "request_id": 7, "offset": 0, "length": 1_048_576}),
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
        "range-bytes-max-offset",
        range_frame(9, MAX_JSON_SAFE_INTEGER, b"abc"),
        {"kind": "range_bytes", "request_id": 9, "offset": MAX_JSON_SAFE_INTEGER, "payload_len": 3},
    )

    def invalid_frame(name: str, content: bytes, error_kind: str) -> None:
        manifest.add(f"data/invalid/{name}.bin", schema, content, {"result": "error", "kind": error_kind})

    good = source_buffer_frame(1, 0, 0, payload)
    invalid_frame("shorter-than-header", good[:23], "frame_too_short")
    invalid_frame("header-without-payload", good[:24], "payload_empty")
    invalid_frame("unknown-kind-3", source_buffer_frame(1, 0, 0, payload, kind=3), "unknown_frame_kind")
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

    def await_clock(self, position_us: int) -> None:
        """Пауза записи: страница ждала, пока часы плеера дойдут до `position_us`.

        Не сообщение и не рамка, а темп воспроизведения записи: смена качества
        случилась посреди просмотра. Проигрыватель записи (тест) ждёт `clock` плеера
        с позицией не меньше этой; сторож протокола такие шаги пропускает.
        """
        self.steps.append({"await_clock_us": position_us})

    def finish(self, manifest: Manifest, expect: dict[str, Any]) -> None:
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


def main() -> None:
    for directory in GENERATED_DIRECTORIES:
        shutil.rmtree(FIXTURES_ROOT / directory, ignore_errors=True)
    manifest = Manifest()
    write_extension_control(manifest)
    write_player_control(manifest)
    write_page_session(manifest)
    write_player_session(manifest)
    write_data_frames(manifest)
    write_streams(manifest)
    (FIXTURES_ROOT / "manifest.json").write_bytes(
        json.dumps({"cases": manifest.cases, "streams": manifest.streams}, ensure_ascii=False, indent=2).encode()
        + b"\n"
    )
    print(f"cases: {len(manifest.cases)}, streams: {len(manifest.streams)}")


if __name__ == "__main__":
    main()
