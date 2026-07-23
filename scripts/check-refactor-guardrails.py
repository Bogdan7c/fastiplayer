#!/usr/bin/env python3
"""Проверяет архитектурные dependency guardrails для refactoring PR.

Скрипт намеренно проверяет direct manifest-dependencies из
`cargo metadata --no-deps --format-version 1`. Boundary rules смотрят normal
dependencies, а isolation rules для FFmpeg/libav и neutral tempo crates
проверяются по всем direct dependency kinds.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any


METADATA_COMMAND = ("cargo", "metadata", "--no-deps", "--format-version", "1")

CONTRACT_CRATES = frozenset(
    {
        "atomic-file-store",
        "audio-core",
        "bounded-xml-reader",
        "media-core",
        "natural-sort-key",
        "playlist-core",
        "playlist-io",
        "playlist-state",
        "codec-core",
        "demux-api",
        "settings-core",
        "video-frame-contract",
        "video-core",
        "video-backend-api",
        "video-present-core",
        "render-core",
        "capability-core",
        "frame-server-core",
        "web-media-core",
        "web-media-playback-plan",
        "web-media-transport-api",
    }
)

REQUIRED_ROLE_CRATES = frozenset(
    {
        "animation-core",
        "app-egui",
        "atomic-file-store",
        "audio",
        "audio-core",
        "bounded-xml-reader",
        "capability-core",
        "codec-core",
        "desktop-integration",
        "demux-api",
        "frame-server-core",
        "flv-demux",
        "media-prefetch",
        "media-core",
        "natural-sort-key",
        "mpeg-ts-demux",
        "player-core",
        "playlist-core",
        "playlist-io",
        "playlist-discovery",
        "playlist-state",
        "render-core",
        "render-wgpu-shell",
        "render-wgpu-video",
        "rustiplayer-config",
        "rustiplayer-settings",
        "service-direct-media",
        "service-ytdlp",
        "settings-core",
        "settings-derive",
        "source-core",
        "symphonia-demux",
        "video-frame-contract",
        "video-core",
        "video-backend-api",
        "video-present-core",
        "video-ffmpeg",
        "video-vaapi",
        "web-media-core",
        "web-media-http",
        "web-media-playback-plan",
        "web-media-transport-api",
    }
)

# Crates из этого списка были удалены из workspace и не должны возвращаться
# как "reference" backend-ы без отдельного архитектурного решения.
REMOVED_WORKSPACE_CRATES = frozenset({"video-vulkan"})

VIDEO_FRAME_CONTRACT_ALLOWED_DEPENDENCIES = frozenset({"serde"})

VIDEO_PRESENT_CORE_ALLOWED_DEPENDENCIES = frozenset(
    {
        "crossbeam-channel",
        "thiserror",
        "tracing",
        "video-backend-api",
        "video-core",
    }
)

FRAME_SERVER_CORE_ALLOWED_DEPENDENCIES = frozenset(
    {
        "media-core",
        "video-present-core",
    }
)

# Playlist domain переиспользует neutral media metadata vocabulary и RNG.
# UI/player/filesystem/serde/service edges должны появляться в верхних owners.
PLAYLIST_CORE_ALLOWED_DEPENDENCIES = frozenset(
    {"media-core", "natural-sort-key", "rand"}
)

# Playlist document parser видит neutral draft/time contracts, hardened XML
# boundary и узкие URI/NFC helpers; filesystem/network/app/service/player edges запрещены.
PLAYLIST_IO_ALLOWED_DEPENDENCIES = frozenset(
    {
        "bounded-xml-reader",
        "media-core",
        "playlist-core",
        "unicode-normalization",
        "url",
    }
)

# Общий natural comparator остаётся std-only и не знает path/domain owners.
NATURAL_SORT_KEY_ALLOWED_DEPENDENCIES = frozenset()

# Atomic file store владеет только std filesystem durability protocol.
ATOMIC_FILE_STORE_ALLOWED_DEPENDENCIES = frozenset()

# Hardened XML boundary не должен получить filesystem/network/domain dependencies.
BOUNDED_XML_READER_ALLOWED_DEPENDENCIES = frozenset({"quick-xml", "thiserror"})

# Neutral web-media values не должны напрямую знать process/service/network/app owners.
WEB_MEDIA_CORE_ALLOWED_DEPENDENCIES = frozenset()

# Pure web-media planner пересекает только neutral snapshots и не знает
# concrete provider/service/app/player implementations.
WEB_MEDIA_PLAYBACK_PLAN_ALLOWED_DEPENDENCIES = frozenset(
    {
        "audio-core",
        "capability-core",
        "codec-core",
        "demux-api",
        "web-media-core",
    }
)

# Neutral web transport API переиспользует только source HTTP primitives и
# normalized web identities; concrete provider/demux/player/service edges запрещены.
WEB_MEDIA_TRANSPORT_API_ALLOWED_DEPENDENCIES = frozenset(
    {"source-core", "thiserror", "web-media-core"}
)

# Concrete HTTP provider переиспользует единственные владельцы HTTP source и
# prefetch policy; service/demux/player/client dependencies здесь запрещены.
WEB_MEDIA_HTTP_ALLOWED_DEPENDENCIES = frozenset(
    {"media-prefetch", "source-core", "web-media-transport-api"}
)

# yt-dlp service заканчивается на extractor/descriptor/neutral request mapping.
# Concrete HTTP, cache, demux и player ownership остаются у composition owners.
SERVICE_YTDLP_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "demux-api",
        "media-prefetch",
        "player-core",
        "reqwest",
        "service-direct-media",
        "symphonia-demux",
        "web-media-http",
    }
)

# Neutral demux composition владеет только typed input/probe/runtime contracts.
# Concrete container backends, player, services и UI остаются внешними adapters.
DEMUX_API_ALLOWED_DEPENDENCIES = frozenset(
    {
        "anyhow",
        "bytes",
        "media-core",
        "source-core",
        "thiserror",
        "tracing",
    }
)

# First-party MPEG-TS parser зависит только от neutral byte/demux/media/codec contracts.
# HLS, HTTP, player, UI, Symphonia и FFmpeg не должны протекать в reusable container owner.
MPEG_TS_DEMUX_ALLOWED_DEPENDENCIES = frozenset(
    {
        "anyhow",
        "bytes",
        "codec-core",
        "demux-api",
        "media-core",
        "source-core",
        "thiserror",
    }
)

# First-party FLV/F4F parser зависит только от neutral byte/demux/media/codec contracts.
# HDS/RTMP network state, player, UI, Symphonia и FFmpeg не входят в container owner.
FLV_DEMUX_ALLOWED_DEPENDENCIES = frozenset(
    {
        "anyhow",
        "bytes",
        "codec-core",
        "demux-api",
        "media-core",
        "source-core",
        "thiserror",
    }
)

# Existing Symphonia adapter не получает второй parser для уже принадлежащих
# exact format patches ISO BMFF, Matroska/WebM и CAF. Новые container families
# проходят отдельный architecture gate и не маскируются dependency fallback-ом.
SYMPHONIA_DEMUX_FORBIDDEN_ALTERNATIVE_PARSER_DEPENDENCIES = frozenset(
    {
        "ebml",
        "ebml-iterable",
        "matroska",
        "matroska-demuxer",
        "mp4",
        "mp4parse",
        "webm-demux",
        "webm-iterable",
    }
)

# Single-file discovery владеет filesystem/cancellation orchestration, но видит
# Symphonia только через узкий neutral snapshot boundary в symphonia-demux.
PLAYLIST_DISCOVERY_ALLOWED_DEPENDENCIES = frozenset(
    {
        "media-core",
        "natural-sort-key",
        "source-core",
        "symphonia-demux",
        "thiserror",
    }
)

# Persistence owner может видеть только neutral playlist/media contracts и
# минимальный набор serde/hash/platform dependencies для bounded JSON I/O.
PLAYLIST_STATE_ALLOWED_DEPENDENCIES = frozenset(
    {
        "atomic-file-store",
        "libc",
        "media-core",
        "playlist-core",
        "serde",
        "serde_json",
        "sha2",
    }
)

FFMPEG_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "ac-ffmpeg",
        "ffmpeg",
        "ffmpeg-next",
        "ffmpeg-sys",
        "ffmpeg-sys-next",
        "ffmpeg-the-third",
        "ffmpeg4-sys",
        "ffmpeg5-sys",
        "ffmpeg6-sys",
        "ffmpeg7-sys",
        "ffmpeg8-sys",
        "libav",
        "libav-sys",
        "libavcodec",
        "libavcodec-sys",
        "libavfilter",
        "libavfilter-sys",
        "libavformat",
        "libavformat-sys",
        "libavutil",
        "libavutil-sys",
        "rsmpeg",
    }
)

TEMPO_WORKSPACE_FORBIDDEN_DEPENDENCIES = frozenset({"timestretch"})

# Нейтральные crates не должны обходить `audio-core` boundary ни через внешний
# DSP crate, ни через локальный concrete adapter; composition root при этом
# по-прежнему вправе собирать runtime graph через `audio-signalsmith`.
TEMPO_NEUTRAL_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "audio-signalsmith",
        "audio-timestretch",
        "signalsmith-stretch",
        "timestretch",
    }
)

TEMPO_NEUTRAL_CRATES = frozenset({"audio-core", "player-core"})

CONTRACT_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "audio",
        "ash",
        "cros-codecs",
        "cros-libva",
        "desktop-integration",
        "egui",
        "egui-wgpu",
        "egui-winit",
        "ffmpeg-next",
        "ffmpeg-sys-next",
        "gbm",
        "gbm-sys",
        "player-core",
        "render-wgpu-shell",
        "render-wgpu-video",
        "rustiplayer-config",
        "rustiplayer-settings",
        "service-direct-media",
        "service-ytdlp",
        "settings-derive",
        "symphonia-demux",
        "video-vaapi",
        "video-ffmpeg",
        "video-vulkan",
        "webm-demux",
        "wgpu",
        "wgpu-types",
        "winit",
    }
)

LOW_LEVEL_CRATES = frozenset(
    {
        "audio",
        "codec-core",
        "demux-api",
        "media-core",
        "symphonia-demux",
        "webm-demux",
    }
)

LOW_LEVEL_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "ash",
        "render-wgpu-shell",
        "render-wgpu-video",
        "video-ffmpeg",
        "video-vulkan",
        "video-vaapi",
        "wgpu",
        "wgpu-types",
    }
)

PLAYER_CORE_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "desktop-integration",
        "egui",
        "egui-wgpu",
        "egui-winit",
        "render-wgpu-shell",
        "render-wgpu-video",
        "service-ytdlp",
        "source-core",
        "symphonia-demux",
        "video-ffmpeg",
        "video-vaapi",
        "video-vulkan",
        "webm-demux",
        "wgpu",
        "wgpu-types",
        "ash",
        "winit",
    }
)

VIDEO_BACKEND_CRATES = frozenset(
    {
        "video-ffmpeg",
        "video-vaapi",
    }
)

VIDEO_BACKEND_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "ash",
        "player-core",
        "render-core",
        "render-wgpu-shell",
        "render-wgpu-video",
        "wgpu",
        "wgpu-types",
    }
)

RENDER_WGPU_SHELL_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "audio",
        "player-core",
        "service-ytdlp",
        "source-core",
        "symphonia-demux",
        "video-ffmpeg",
        "video-vaapi",
        "video-vulkan",
        "webm-demux",
    }
)

RENDER_WGPU_VIDEO_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "audio",
        "desktop-integration",
        "egui",
        "egui-wgpu",
        "egui-winit",
        "player-core",
        "pollster",
        "render-wgpu-shell",
        "service-ytdlp",
        "source-core",
        "symphonia-demux",
        "video-ffmpeg",
        "video-vaapi",
        "video-vulkan",
        "webm-demux",
        "winit",
    }
)

MEDIA_PREFETCH_CRATES = frozenset({"media-prefetch"})

MEDIA_PREFETCH_ALLOWED_DEPENDENCIES = frozenset(
    {
        "source-core",
        "thiserror",
        "tracing",
    }
)

KNOWN_DEBT_EDGES: dict[tuple[str, str], str] = {}

PUBLIC_CONFIG_SCAN_ROOTS = (
    "crates/app-egui",
    "crates/config",
    "crates/rustiplayer-settings",
    "crates/settings-core",
    "crates/settings-derive",
)

TEXT_SOURCE_SUFFIXES = frozenset(
    {
        ".rs",
        ".toml",
        ".ron",
        ".json",
        ".snap",
    }
)

RUST_SOURCE_SUFFIXES = frozenset({".rs"})

SOURCE_POLICY_SCAN_ROOTS = ("crates",)

DIRECT_FFMPEG_TYPE_ALLOWED_ROOTS = (Path("crates/video-ffmpeg"),)

DIRECT_FFMPEG_TYPE_PATTERNS = (
    (
        re.compile(
            r"\bAV(?:Frame|Packet|Codec|CodecContext|PixelFormat|Rational|Dictionary|BufferRef)\b"
        ),
        "raw FFmpeg/libav types должны оставаться внутри video-ffmpeg",
    ),
    (
        re.compile(r"\bAVERROR\b"),
        "raw FFmpeg/libav error macros должны оставаться внутри video-ffmpeg",
    ),
    (
        re.compile(r"\bffmpeg_sys_next::|\bffmpeg_next::|\brsmpeg::"),
        "raw FFmpeg Rust bindings должны использоваться только внутри video-ffmpeg",
    ),
)

CPU_RGB_CONVERSION_PATTERNS = (
    (
        re.compile(r"\bsws_scale\b|\bsws_getContext\b|\bSwsContext\b"),
        "swscale CPU conversion запрещён в playback/source tree",
    ),
    (
        re.compile(r"\blibswscale\b|\bav_image_convert\b|\bavpicture_"),
        "CPU RGB/YUV conversion helpers запрещены в playback/source tree",
    ),
)

FFMPEG_HARDWARE_DECODE_PATTERNS = (
    (
        re.compile(r"\bav_hwdevice_|\bav_hwframe_|\bAVHW(?:Device|Frames)"),
        "FFmpeg hardware decode/device API запрещён: native hardware path живёт вне FFmpeg",
    ),
    (
        re.compile(r"\bhwaccel\b|\bhw_frames\b"),
        "FFmpeg hwaccel path запрещён: video-ffmpeg остаётся software-decode-only",
    ),
)

DIRECT_VAAPI_DISPLAY_ALLOWED_ROOTS = (
    Path("crates/video-vaapi"),
    Path("crates/cros-codecs-patch"),
    Path("crates/cros-libva-patch"),
)

DIRECT_VAAPI_DISPLAY_PATTERNS = (
    (
        re.compile(r"\b(?:cros_codecs::libva|libva)::Display::open(?:_drm_display)?\b"),
        "VA display/session открывается только внутри video-vaapi owner boundary",
    ),
    (
        re.compile(r"\bopen_drm_display\s*\("),
        "DRM VA display/session открывается только внутри video-vaapi owner boundary",
    ),
    (
        re.compile(r"\bVADisplay\b|\bvaGetDisplay\b|\bvaInitialize\b"),
        "raw VA display handle/API не должен выходить за video-vaapi boundary",
    ),
)

MAIN_VIDEO_REUSED_DECODER_SCAN_PATHS = (
    Path("crates/player-core/src/session/scrub_driver.rs"),
    Path("crates/player-core/src/session/prepared_seek.rs"),
)

MAIN_VIDEO_SECOND_SESSION_PATTERNS = (
    (
        re.compile(
            r"\b(PlayerSession::new|PlayerWorker::new|VideoBackendFactory|StartedVideoBackend)\b"
        ),
        "main-video SeekLanding/LiveScrub должен reuse playback session/decoder, а не стартовать вторую session/backend",
    ),
    (
        re.compile(
            r"\b(VaapiVideoBackendFactory|FfmpegSoftwareVideoBackendFactory)::new"
        ),
        "main-video scrub/prepared seek не должен создавать второй decoder backend",
    ),
)

PREPARED_BRANCH_PROMOTION_ALLOWED_ROOTS = (
    Path("crates/frame-server-core"),
    Path("crates/player-core/src/session/prepared_seek.rs"),
)

PREPARED_BRANCH_PROMOTION_PATTERNS = (
    (
        re.compile(r"\bpromote_prepared_frame\s*\("),
        "prepared branch promotion разрешён только frame-server-core boundary и S17 player-core owner-у",
    ),
)

# S18 закрепляет canonical top-level read boundary у каждого structural consumer-а.
PLAYLIST_TOPOLOGY_SOURCE_ANCHORS = (
    (
        Path("crates/playlist-state/src/dto/v2.rs"),
        ("iter_top_level_entries()",),
        "playlist-state v2 обязан сохранять canonical top-level entry order",
    ),
    (
        Path("crates/playlist-io/src/export/snapshot.rs"),
        ("iter_top_level_entries()",),
        "playlist export обязан снимать canonical top-level entry snapshot",
    ),
    (
        Path("crates/app-egui/src/playlist_runtime/view.rs"),
        ("iter_top_level_entries()",),
        "Playlist UI read model обязан строиться из canonical top-level entries",
    ),
    (
        Path("crates/app-egui/src/playlist_runtime/compound_view.rs"),
        ("iter_top_level_entries()",),
        "compound view обязан выводить child rows из canonical top-level entries",
    ),
    (
        Path("crates/app-egui/src/playlist_runtime/external_projection.rs"),
        ("top_level_entry(entry_id)",),
        "external projection обязан разрешать compound context через canonical top-level entry",
    ),
)

# Structural consumers не должны снова подменять top-level order derived playable traversal-ом.
PLAYLIST_TOPOLOGY_DERIVED_READ_PATTERNS = (
    (
        re.compile(r"\biter_playable_(?:items|ids)\s*\("),
        "persistence/UI/external structural consumer не должен flatten canonical top-level entries",
    ),
    (
        re.compile(r"\bowned_playable_items_snapshot\s*\("),
        "persistence/UI/external structural consumer не должен хранить derived playable snapshot как canonical state",
    ),
)

# Presentation и external-control owners получают только redacted values, не raw identities.
PLAYLIST_SECRET_PRESENTATION_PATHS = (
    Path("crates/app-egui/src/ui/playlist"),
    Path("crates/app-egui/src/playlist_runtime/external_projection.rs"),
    Path("crates/desktop-integration/src"),
)

# Intent-named raw access остаётся только persistence/open/service owners выше presentation слоя.
PLAYLIST_SECRET_PRESENTATION_PATTERNS = (
    (
        re.compile(r"\bexpose_(?:secret|payload)_for_(?:open|persistence)\s*\("),
        "Playlist UI/external projection не должны раскрывать secret-bearing identity",
    ),
)

# S27 фиксирует три production consumer-а единственной app-owned web-open composition.
PROGRESSIVE_WEB_SOURCE_ANCHORS = (
    (
        Path("crates/app-egui/src/startup_media.rs"),
        ("crate::web_media_open::prepare_yt_dlp_web_media(",),
        "CLI startup обязан использовать единый progressive web-open composition path",
    ),
    (
        Path("crates/app-egui/src/media_open/preparation.rs"),
        ("crate::web_media_open::prepare_yt_dlp_web_media(",),
        "queue media-open обязан использовать единый progressive web-open composition path",
    ),
    (
        Path("crates/app-egui/src/frame_prepare/settings_runtime_adapter.rs"),
        ("crate::web_media_open::prepare_yt_dlp_web_media(",),
        "settings rebuild обязан использовать единый progressive web-open composition path",
    ),
    (
        Path("crates/service-ytdlp/src/candidate/tests.rs"),
        ("public_surface_and_manifest_have_no_legacy_webm_opener",),
        "service-ytdlp обязан сохранять focused proof удаления legacy WebM opener-а",
    ),
    (
        Path("scripts/progressive-web-smoke.sh"),
        ("--url", "redact_runtime_log", "MANUAL REVIEW REQUIRED"),
        "manual progressive runner обязан принимать explicit URL и писать только redacted report",
    ),
    (
        Path("scripts/tests/progressive-web-smoke-self-test.sh"),
        ("<redacted-url>", "<redacted-secret-line>"),
        "manual progressive runner обязан иметь focused redaction self-test",
    ),
)

# Старые service-owned WebM opener symbols не должны вернуться ни в Rust, ни в runtime scripts.
PROGRESSIVE_WEB_LEGACY_SCAN_PATHS = (
    Path("crates/app-egui/src"),
    Path("crates/service-ytdlp/src"),
    Path("crates/service-ytdlp/Cargo.toml"),
    Path("scripts/media-regression.sh"),
    Path("scripts/playback-smoke.sh"),
    Path("scripts/progressive-web-smoke.sh"),
    Path("scripts/runtime-acceptance.sh"),
)

PROGRESSIVE_WEB_LEGACY_PATTERNS = (
    (
        re.compile(
            r"\b(?:open_streaming_media_from|open_seekable_vod_from|"
            r"YtDlpStreamingMedia|YtDlpSelectedStreamIdentity|"
            r"selected_webm_(?:opens|falls_back|live))"
        ),
        "legacy service-owned WebM opener/runtime scenario запрещён после S27",
    ),
)

# Durable state, config и URL presentation не должны знать transient transport/auth types.
PROGRESSIVE_WEB_TRANSIENT_SECRET_SCAN_PATHS = (
    Path("crates/config/src"),
    Path("crates/playlist-core/src"),
    Path("crates/playlist-io/src"),
    Path("crates/playlist-state/src"),
    Path("crates/app-egui/src/ui/url_sidebar.rs"),
    Path("crates/app-egui/src/web_media_stream_model.rs"),
    Path("crates/app-egui/src/web_media_stream_model"),
)

PROGRESSIVE_WEB_TRANSIENT_SECRET_PATTERNS = (
    (
        re.compile(
            r"\b(?:HttpRequestTarget|ScopedHttpCookieJar|SecretRequestContext|"
            r"TransportOpenRequest|YtDlpRequestMaterial)\b"
        ),
        "durable state/config/URL UI не должны зависеть от transient transport secrets",
    ),
)

# S28G включается только в repository, где существует concrete Symphonia factory.
EXISTING_DEMUX_BOUNDARY_MARKER = Path("crates/symphonia-demux/src/factory.rs")

# Эти crates участвуют в reuse foundation и поэтому остаются blocking coverage owners.
EXISTING_DEMUX_REQUIRED_BLOCKING_CRATES = frozenset(
    {"demux-api", "symphonia-demux", "web-media-http"}
)

# Exact anchors связывают registration inventory, отдельный Matroska DocType proof,
# bounded scanner exception и human evidence artifact в один hardening gate.
EXISTING_DEMUX_SOURCE_ANCHORS = (
    (
        Path("crates/symphonia-demux/src/factory.rs"),
        (
            "symphonia/generated-fmp4-s28a",
            "symphonia/generated-matroska-ordered-s28b",
            "symphonia/s28c-mpeg-layer-3",
        ),
        "S28G factory descriptor обязан сохранять exact S28A/B/C fixture inventory",
    ),
    (
        Path("crates/symphonia-demux/src/factory/tests.rs"),
        ("descriptor_lists_exact_s28_foundation_fixture_ids",),
        "S28G fixture inventory обязан иметь focused exact regression test",
    ),
    (
        Path("crates/symphonia-demux/src/factory/tests/matroska.rs"),
        ("exact_matroska_doctype_opens_local_and_ordered_inputs",),
        "Matroska registration нельзя доказывать только WebM DocType fixture-ом",
    ),
    (
        Path("crates/symphonia-demux/src/matroska_metadata.rs"),
        ("cluster_payload_is_opaque_to_bounded_metadata_and_cue_indexer",),
        "bounded Matroska metadata/cue indexer обязан сохранять opaque Cluster proof",
    ),
    (
        Path("docs/existing-demux-s28g.md"),
        (
            "# S28G: existing-demux hardening gate",
            "symphonia/generated-fmp4-s28a",
            "symphonia/generated-matroska-ordered-s28b",
            "Known coverage limitation",
        ),
        "S28G aggregate evidence artifact обязан оставаться полным и честным",
    ),
)

# Проверяются только объявления production parser-а. Комментарии, assertions и
# test-only corpus builders могут свободно называть Cluster/Block/lacing.
EXISTING_DEMUX_PACKET_PARSER_DECLARATION_PATTERNS = (
    (
        re.compile(
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+"
            r"(?:ID_)?(?:CLUSTER|SIMPLE_BLOCK|BLOCK_GROUP|BLOCK|CODEC_STATE)\b",
            re.IGNORECASE,
        ),
        "Matroska packet element IDs принадлежат exact symphonia-format-mkv patch",
    ),
    (
        re.compile(
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+\w*"
            r"(?:cluster|simple_block|block_group|matroska_block|lacing|packet_payload)"
            r"\w*\s*\(",
            re.IGNORECASE,
        ),
        "Matroska Cluster/Block/lacing parsing принадлежит exact symphonia-format-mkv patch",
    ),
)

RUST_TEST_MODULE_START = re.compile(
    r"(?m)^\s*#\[cfg\(test\)\]\s*\n\s*mod\s+tests\s*\{"
)

REQUIRED_SOURCE_ANCHORS = ()

RequiredSourceAnchor = tuple[Path, tuple[str, ...], str]


class GuardrailError(RuntimeError):
    """Ошибка входных данных или запуска Cargo, а не нарушение архитектурной policy."""


@dataclass(frozen=True)
class DependencyViolation:
    """Одно прямое dependency-нарушение с объяснением правила."""

    owner: str
    dependency: str
    rule: str


@dataclass(frozen=True)
class SourcePolicyViolation:
    """Одно нарушение source/string policy guardrail."""

    path: Path
    line_number: int
    rule: str
    matched_text: str


@dataclass(frozen=True)
class DependencyGraphPolicyResult:
    """Полный результат policies, которым нужен только Cargo dependency graph."""

    missing_role_crates: list[str]
    reintroduced_workspace_crates: list[str]
    dependency_violations: list[DependencyViolation]


def repository_root() -> Path:
    """Возвращает корень репозитория относительно текущего скрипта."""

    return Path(__file__).resolve().parents[1]


def load_cargo_metadata(repo_root: Path) -> dict[str, Any]:
    """Запускает Cargo и возвращает разобранный JSON metadata."""

    completed_process = subprocess.run(
        METADATA_COMMAND,
        cwd=repo_root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed_process.returncode != 0:
        raise GuardrailError(format_failed_command(completed_process))

    try:
        metadata = json.loads(completed_process.stdout)
    except json.JSONDecodeError as error:
        raise GuardrailError(f"cargo metadata вернул невалидный JSON: {error}") from error

    if not isinstance(metadata, dict):
        raise GuardrailError("cargo metadata должен вернуть JSON object верхнего уровня")

    if metadata.get("version") != 1:
        raise GuardrailError("cargo metadata вернул format-version, отличный от ожидаемого 1")

    return metadata


def format_failed_command(completed_process: subprocess.CompletedProcess[str]) -> str:
    """Формирует диагностическое сообщение без потери stdout/stderr Cargo."""

    command_text = " ".join(METADATA_COMMAND)
    stdout_text = completed_process.stdout.strip()
    stderr_text = completed_process.stderr.strip()
    details = [f"команда `{command_text}` завершилась с кодом {completed_process.returncode}"]
    if stdout_text:
        details.append(f"stdout:\n{stdout_text}")
    if stderr_text:
        details.append(f"stderr:\n{stderr_text}")
    return "\n".join(details)


def workspace_packages(metadata: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Возвращает workspace packages по package name и проверяет целостность metadata."""

    packages = metadata.get("packages")
    workspace_members = metadata.get("workspace_members")
    if not isinstance(packages, list):
        raise GuardrailError("cargo metadata не содержит массив packages")
    if not isinstance(workspace_members, list):
        raise GuardrailError("cargo metadata не содержит массив workspace_members")

    packages_by_id = collect_packages_by_id(packages)
    package_names: dict[str, dict[str, Any]] = {}
    for package_id in workspace_members:
        if not isinstance(package_id, str):
            raise GuardrailError("workspace_members должен содержать строковые package id")
        package = packages_by_id.get(package_id)
        if package is None:
            raise GuardrailError(f"workspace member `{package_id}` отсутствует в packages")
        package_name = read_string_field(package, "name", f"package `{package_id}`")
        if package_name in package_names:
            raise GuardrailError(f"workspace содержит duplicate package name `{package_name}`")
        package_names[package_name] = package

    return package_names


def collect_packages_by_id(packages: list[Any]) -> dict[str, dict[str, Any]]:
    """Индексирует packages по Cargo package id."""

    packages_by_id: dict[str, dict[str, Any]] = {}
    for package in packages:
        if not isinstance(package, dict):
            raise GuardrailError("каждый элемент packages должен быть JSON object")
        package_id = read_string_field(package, "id", "package")
        if package_id in packages_by_id:
            raise GuardrailError(f"cargo metadata содержит duplicate package id `{package_id}`")
        packages_by_id[package_id] = package
    return packages_by_id


def read_string_field(source: dict[str, Any], field_name: str, context: str) -> str:
    """Читает обязательное строковое поле из JSON object."""

    value = source.get(field_name)
    if not isinstance(value, str):
        raise GuardrailError(f"{context} должен содержать строковое поле `{field_name}`")
    return value


def direct_normal_dependencies(packages: dict[str, dict[str, Any]]) -> dict[str, frozenset[str]]:
    """Строит map direct normal-dependencies для каждого workspace package."""

    return direct_dependencies(packages, normal_only=True)


def direct_all_manifest_dependencies(
    packages: dict[str, dict[str, Any]],
) -> dict[str, frozenset[str]]:
    """Строит map всех direct dependencies, включая dev/build dependency."""

    return direct_dependencies(packages, normal_only=False)


def direct_dependencies(
    packages: dict[str, dict[str, Any]],
    *,
    normal_only: bool,
) -> dict[str, frozenset[str]]:
    """Строит map direct dependencies из Cargo manifest metadata."""

    dependency_map: dict[str, frozenset[str]] = {}
    for package_name, package in packages.items():
        dependencies = package.get("dependencies")
        if not isinstance(dependencies, list):
            raise GuardrailError(f"package `{package_name}` не содержит массив dependencies")

        direct_dependency_names = {
            read_string_field(dependency, "name", f"dependency package `{package_name}`")
            for dependency in dependencies
            if not normal_only or is_normal_dependency(dependency, package_name)
        }
        dependency_map[package_name] = frozenset(direct_dependency_names)

    return dependency_map


def workspace_dependency_names(repo_root: Path) -> frozenset[str]:
    """Читает root `[workspace.dependencies]`, потому что Cargo metadata показывает только package deps."""

    manifest_path = repo_root / "Cargo.toml"
    try:
        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as error:
        raise GuardrailError(f"`{manifest_path}` содержит невалидный TOML: {error}") from error
    except OSError as error:
        raise GuardrailError(f"`{manifest_path}` нельзя прочитать: {error}") from error

    workspace = manifest.get("workspace", {})
    if not isinstance(workspace, dict):
        raise GuardrailError("root Cargo.toml `[workspace]` должен быть TOML table")

    dependencies = workspace.get("dependencies", {})
    if not isinstance(dependencies, dict):
        raise GuardrailError("root Cargo.toml `[workspace.dependencies]` должен быть TOML table")

    return frozenset(dependencies)


def is_normal_dependency(dependency: Any, package_name: str) -> bool:
    """Отличает normal dependency от dev/build dependency."""

    if not isinstance(dependency, dict):
        raise GuardrailError(f"dependency package `{package_name}` должен быть JSON object")
    return dependency.get("kind") is None


def find_missing_role_crates(packages: dict[str, dict[str, Any]]) -> list[str]:
    """Находит зафиксированные архитектурные роли, пропавшие из workspace."""

    return sorted(REQUIRED_ROLE_CRATES.difference(packages))


def find_reintroduced_workspace_crates(packages: dict[str, dict[str, Any]]) -> list[str]:
    """Находит удалённые workspace crates, которые нельзя вернуть молча."""

    return sorted(REMOVED_WORKSPACE_CRATES.intersection(packages))


def find_dependency_violations(
    dependency_map: dict[str, frozenset[str]],
    all_dependency_map: dict[str, frozenset[str]],
    workspace_dependencies: frozenset[str],
) -> list[DependencyViolation]:
    """Проверяет dependency rules из документа guardrails."""

    violations: list[DependencyViolation] = []
    for dependency in sorted(workspace_dependencies.intersection(FFMPEG_FORBIDDEN_DEPENDENCIES)):
        violations.append(
            DependencyViolation(
                owner="workspace.dependencies",
                dependency=dependency,
                rule="FFmpeg/libav crates не должны быть общими workspace dependencies",
            )
        )
    for dependency in sorted(
        workspace_dependencies.intersection(TEMPO_WORKSPACE_FORBIDDEN_DEPENDENCIES)
    ):
        violations.append(
            DependencyViolation(
                owner="workspace.dependencies",
                dependency=dependency,
                rule="tempo backend crates не должны быть общими workspace dependencies до concrete adapter boundary",
            )
        )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"video-frame-contract"}),
            VIDEO_FRAME_CONTRACT_ALLOWED_DEPENDENCIES,
            "video-frame-contract остаётся leaf contract crate и зависит только от serde",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"video-present-core"}),
            VIDEO_PRESENT_CORE_ALLOWED_DEPENDENCIES,
            "video-present-core normal-deps остаются нейтральным present boundary без player/render/backend/UI deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"frame-server-core"}),
            FRAME_SERVER_CORE_ALLOWED_DEPENDENCIES,
            "frame-server-core остаётся нейтральным frame-server boundary без player/app/render/backend/service deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"natural-sort-key"}),
            NATURAL_SORT_KEY_ALLOWED_DEPENDENCIES,
            "natural-sort-key остаётся std-only neutral comparator",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"atomic-file-store"}),
            ATOMIC_FILE_STORE_ALLOWED_DEPENDENCIES,
            "atomic-file-store остаётся std-only neutral durability boundary",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"bounded-xml-reader"}),
            BOUNDED_XML_READER_ALLOWED_DEPENDENCIES,
            "bounded-xml-reader не получает filesystem/network/domain dependencies",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-core"}),
            WEB_MEDIA_CORE_ALLOWED_DEPENDENCIES,
            "web-media-core остаётся std-only neutral value contract",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-playback-plan"}),
            WEB_MEDIA_PLAYBACK_PLAN_ALLOWED_DEPENDENCIES,
            "web-media-playback-plan остаётся pure neutral capability planner",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-transport-api"}),
            WEB_MEDIA_TRANSPORT_API_ALLOWED_DEPENDENCIES,
            "web-media-transport-api остаётся neutral provider/secret/network boundary",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-http"}),
            WEB_MEDIA_HTTP_ALLOWED_DEPENDENCIES,
            "web-media-http переиспользует только neutral transport API, source-core и media-prefetch",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"service-ytdlp"}),
            SERVICE_YTDLP_FORBIDDEN_DEPENDENCIES,
            "service-ytdlp не владеет concrete HTTP/cache/demux/player playback stack",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"demux-api"}),
            DEMUX_API_ALLOWED_DEPENDENCIES,
            "demux-api остаётся neutral registry/composition boundary без concrete backend/player/service/UI deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"mpeg-ts-demux"}),
            MPEG_TS_DEMUX_ALLOWED_DEPENDENCIES,
            "mpeg-ts-demux остаётся reusable container owner без HLS/network/player/UI/FFmpeg deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"flv-demux"}),
            FLV_DEMUX_ALLOWED_DEPENDENCIES,
            "flv-demux остаётся reusable FLV/F4F owner без HDS/RTMP network/player/UI/FFmpeg deps",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"symphonia-demux"}),
            SYMPHONIA_DEMUX_FORBIDDEN_ALTERNATIVE_PARSER_DEPENDENCIES,
            "symphonia-demux переиспользует exact Symphonia format patches и не добавляет второй container parser",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"playlist-core"}),
            PLAYLIST_CORE_ALLOWED_DEPENDENCIES,
            "playlist-core зависит только от neutral metadata/natural-key contracts и rand",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"playlist-io"}),
            PLAYLIST_IO_ALLOWED_DEPENDENCIES,
            "playlist-io остаётся neutral parser boundary без filesystem/network/app/player/service deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"playlist-discovery"}),
            PLAYLIST_DISCOVERY_ALLOWED_DEPENDENCIES,
            "playlist-discovery остаётся UI/player/config-neutral local probe owner",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"playlist-state"}),
            PLAYLIST_STATE_ALLOWED_DEPENDENCIES,
            "playlist-state остаётся отдельным JSON/I/O owner без app/player/UI/service dependencies",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            all_dependency_map,
            frozenset(all_dependency_map).difference({"video-ffmpeg"}),
            FFMPEG_FORBIDDEN_DEPENDENCIES,
            "FFmpeg/libav crates разрешены только внутри video-ffmpeg",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            all_dependency_map,
            TEMPO_NEUTRAL_CRATES,
            TEMPO_NEUTRAL_FORBIDDEN_DEPENDENCIES,
            "audio-core/player-core видят только neutral tempo boundary, без direct concrete tempo dependency",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            CONTRACT_CRATES,
            CONTRACT_FORBIDDEN_DEPENDENCIES,
            "contract crates не зависят от shell/backend/player/UI crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            LOW_LEVEL_CRATES,
            LOW_LEVEL_FORBIDDEN_DEPENDENCIES,
            "media/codec/audio/demux слой не зависит от GPU/video backend crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"player-core"}),
            PLAYER_CORE_FORBIDDEN_DEPENDENCIES,
            "player-core не добавляет direct dependency на shell/service/demux/video backend/GPU crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            VIDEO_BACKEND_CRATES,
            VIDEO_BACKEND_FORBIDDEN_DEPENDENCIES,
            "concrete video backend crates используют video-backend-api и не владеют renderer/GPU import crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"render-wgpu-shell"}),
            RENDER_WGPU_SHELL_FORBIDDEN_DEPENDENCIES,
            "render-wgpu-shell не зависит от demux/source/audio/player/service/concrete video backend crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"render-wgpu-video"}),
            RENDER_WGPU_VIDEO_FORBIDDEN_DEPENDENCIES,
            "render-wgpu-video не зависит от shell/UI/app/player/service/concrete video backend crates",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            MEDIA_PREFETCH_CRATES,
            MEDIA_PREFETCH_ALLOWED_DEPENDENCIES,
            "media-prefetch зависит только от source-core плюс tracing/thiserror",
        )
    )
    return sorted(violations, key=lambda violation: (violation.owner, violation.dependency))


def evaluate_dependency_graph_policies(
    packages: dict[str, dict[str, Any]],
    workspace_dependencies: frozenset[str],
) -> DependencyGraphPolicyResult:
    """Проверяет единым entrypoint только manifest/dependency-graph policies."""

    dependency_map = direct_normal_dependencies(packages)
    all_dependency_map = direct_all_manifest_dependencies(packages)
    return DependencyGraphPolicyResult(
        missing_role_crates=find_missing_role_crates(packages),
        reintroduced_workspace_crates=find_reintroduced_workspace_crates(packages),
        dependency_violations=find_dependency_violations(
            dependency_map,
            all_dependency_map,
            workspace_dependencies,
        ),
    )


def find_source_policy_violations(
    repo_root: Path,
    *,
    required_source_anchors: tuple[RequiredSourceAnchor, ...] = REQUIRED_SOURCE_ANCHORS,
) -> list[SourcePolicyViolation]:
    """Проверяет source-level guardrails, которые нельзя выразить Cargo graph-ом."""

    violations: list[SourcePolicyViolation] = []
    violations.extend(find_public_video_backend_option_violations(repo_root))
    violations.extend(find_direct_ffmpeg_type_violations(repo_root))
    violations.extend(find_cpu_rgb_conversion_violations(repo_root))
    violations.extend(find_ffmpeg_hardware_decode_violations(repo_root))
    violations.extend(find_direct_vaapi_display_violations(repo_root))
    violations.extend(find_main_video_second_session_violations(repo_root))
    violations.extend(find_prepared_branch_promotion_violations(repo_root))
    violations.extend(find_app_egui_custom_paint_violations(repo_root))
    violations.extend(find_playlist_topology_boundary_violations(repo_root))
    violations.extend(find_progressive_web_boundary_violations(repo_root))
    violations.extend(find_existing_demux_boundary_violations(repo_root))
    violations.extend(
        find_required_source_anchor_violations(repo_root, required_source_anchors)
    )
    return sorted(
        violations,
        key=lambda violation: (str(violation.path), violation.line_number, violation.rule),
    )


def find_playlist_topology_boundary_violations(
    repo_root: Path,
) -> list[SourcePolicyViolation]:
    """Закрепляет S18 canonical-read и secret-safe presentation boundaries."""

    # Временные unit-test repositories без playlist-core не обязаны собирать весь S18 tree.
    playlist_read_boundary = repo_root / "crates/playlist-core/src/queue/read.rs"
    if not playlist_read_boundary.is_file():
        return []

    # Exact anchors делают исчезновение одного из canonical consumers явным failure.
    violations = find_required_source_anchor_violations(
        repo_root,
        PLAYLIST_TOPOLOGY_SOURCE_ANCHORS,
    )
    # В этих structural owners derived playable traversal не может стать authority.
    structural_consumer_paths = tuple(
        relative_path for relative_path, _, _ in PLAYLIST_TOPOLOGY_SOURCE_ANCHORS
    )
    violations.extend(
        find_regex_violations_in_paths(
            repo_root,
            structural_consumer_paths,
            PLAYLIST_TOPOLOGY_DERIVED_READ_PATTERNS,
        )
    )
    # UI и external transport никогда не получают intent для raw secret access.
    violations.extend(
        find_regex_violations_in_paths(
            repo_root,
            PLAYLIST_SECRET_PRESENTATION_PATHS,
            PLAYLIST_SECRET_PRESENTATION_PATTERNS,
        )
    )
    return violations


def find_progressive_web_boundary_violations(
    repo_root: Path,
) -> list[SourcePolicyViolation]:
    """Закрепляет S27 single-open и durable-vs-transient web boundaries."""

    # Минимальные unit-test repositories без app web composition не собирают весь S27 tree.
    progressive_web_boundary = repo_root / "crates/app-egui/src/web_media_open.rs"
    if not progressive_web_boundary.is_file():
        return []

    # Exact anchors удерживают startup, queue и settings на одном composition path-е.
    violations = find_required_source_anchor_violations(
        repo_root,
        PROGRESSIVE_WEB_SOURCE_ANCHORS,
    )
    # Legacy symbols проверяются и в Rust source, и в executable manual tooling.
    violations.extend(
        find_regex_violations_in_paths(
            repo_root,
            PROGRESSIVE_WEB_LEGACY_SCAN_PATHS,
            PROGRESSIVE_WEB_LEGACY_PATTERNS,
            suffixes=frozenset({".rs", ".sh", ".toml"}),
        )
    )
    # Durable/presentation owners получают только acknowledged locator и safe projection.
    violations.extend(
        find_regex_violations_in_paths(
            repo_root,
            PROGRESSIVE_WEB_TRANSIENT_SECRET_SCAN_PATHS,
            PROGRESSIVE_WEB_TRANSIENT_SECRET_PATTERNS,
        )
    )
    return violations


def find_existing_demux_boundary_violations(
    repo_root: Path,
) -> list[SourcePolicyViolation]:
    """Закрепляет S28G parser ownership, evidence и coverage classification."""

    # Минимальные unit-test repositories без Symphonia factory не собирают S28G tree.
    if not (repo_root / EXISTING_DEMUX_BOUNDARY_MARKER).is_file():
        return []

    violations = find_required_source_anchor_violations(
        repo_root,
        EXISTING_DEMUX_SOURCE_ANCHORS,
    )
    violations.extend(find_existing_demux_packet_parser_violations(repo_root))
    violations.extend(find_existing_demux_coverage_policy_violations(repo_root))
    return violations


def find_existing_demux_packet_parser_violations(
    repo_root: Path,
) -> list[SourcePolicyViolation]:
    """Ловит production Matroska packet parser declarations вне exact format patch."""

    source_root = repo_root / "crates/symphonia-demux/src"
    if not source_root.is_dir():
        return []

    violations: list[SourcePolicyViolation] = []
    for source_path in sorted(source_root.rglob("*.rs")):
        relative_path = source_path.relative_to(repo_root)
        if "tests" in relative_path.parts or relative_path.name == "tests.rs":
            continue

        source_text = read_text_lossy(source_path)
        test_module_match = RUST_TEST_MODULE_START.search(source_text)
        if test_module_match is not None:
            source_text = source_text[: test_module_match.start()]

        for line_number, line in enumerate(source_text.splitlines(), start=1):
            for pattern, rule in EXISTING_DEMUX_PACKET_PARSER_DECLARATION_PATTERNS:
                if pattern.search(line):
                    violations.append(
                        SourcePolicyViolation(
                            path=relative_path,
                            line_number=line_number,
                            rule=rule,
                            matched_text=line.strip(),
                        )
                    )
    return violations


def find_existing_demux_coverage_policy_violations(
    repo_root: Path,
) -> list[SourcePolicyViolation]:
    """Проверяет blocking classification owners, входящих в S28G foundation."""

    relative_path = Path("coverage/policy.json")
    policy_path = repo_root / relative_path
    if not policy_path.is_file():
        return [
            SourcePolicyViolation(
                path=relative_path,
                line_number=0,
                rule="S28G требует checked-in coverage policy",
                matched_text="coverage policy is missing",
            )
        ]

    try:
        policy = json.loads(read_text_lossy(policy_path))
    except (json.JSONDecodeError, OSError) as error:
        return [
            SourcePolicyViolation(
                path=relative_path,
                line_number=0,
                rule="S28G coverage policy должна быть валидным JSON object",
                matched_text=str(error),
            )
        ]

    if not isinstance(policy, dict):
        return [
            SourcePolicyViolation(
                path=relative_path,
                line_number=0,
                rule="S28G coverage policy должна быть JSON object",
                matched_text=f"получен {type(policy).__name__}",
            )
        ]

    blocking_rows = policy.get("blocking_crates")
    informational_rows = policy.get("informational_crates")
    if not isinstance(blocking_rows, list) or not all(
        isinstance(crate_name, str) for crate_name in blocking_rows
    ):
        return [
            SourcePolicyViolation(
                path=relative_path,
                line_number=0,
                rule="S28G coverage policy требует string array `blocking_crates`",
                matched_text=repr(blocking_rows),
            )
        ]
    if not isinstance(informational_rows, list) or not all(
        isinstance(crate_name, str) for crate_name in informational_rows
    ):
        return [
            SourcePolicyViolation(
                path=relative_path,
                line_number=0,
                rule="S28G coverage policy требует string array `informational_crates`",
                matched_text=repr(informational_rows),
            )
        ]

    blocking_crates = frozenset(blocking_rows)
    informational_crates = frozenset(informational_rows)
    violations = []
    for crate_name in sorted(EXISTING_DEMUX_REQUIRED_BLOCKING_CRATES):
        if crate_name in blocking_crates and crate_name not in informational_crates:
            continue
        violations.append(
            SourcePolicyViolation(
                path=relative_path,
                line_number=0,
                rule="S28G demux foundation crates обязаны оставаться blocking coverage owners",
                matched_text=f"crate `{crate_name}` не имеет exact blocking classification",
            )
        )
    return violations


def find_app_egui_custom_paint_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает ручные Painter-примитивы вне специализированного artwork crate."""

    paint_call = re.compile(
        r"\.(?:rect_filled|rect_stroke|circle_filled|circle_stroke|line_segment)\s*\("
    )
    violations: list[SourcePolicyViolation] = []
    source_root = repo_root / "crates/app-egui/src"
    if not source_root.exists():
        return violations
    for source_path in sorted(source_root.rglob("*.rs")):
        relative_path = source_path.relative_to(repo_root)
        for line_number, line in enumerate(read_text_lossy(source_path).splitlines(), start=1):
            if paint_call.search(line):
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_number,
                        rule="custom-рисовка app-egui должна проходить через ui-artwork-egui",
                        matched_text=line.strip(),
                    )
                )
    return violations


def find_public_video_backend_option_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает public config/UI options для удалённых video decode backend-ов."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_text_files(repo_root, PUBLIC_CONFIG_SCAN_ROOTS):
        text = read_text_lossy(repo_root / relative_path)
        for line_index, line in enumerate(text.splitlines(), start=1):
            stripped_line = line.strip()
            lowered_line = stripped_line.lower()

            if "ffmpeg_sw" in lowered_line or "ffmpeg-sw" in lowered_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule="ffmpeg_sw не должен появляться как public config/UI option",
                        matched_text=stripped_line,
                    )
                )

            if is_allowed_removed_vulkan_video_backend_reference(relative_path, stripped_line):
                continue

            if 'preferred_backend = "vulkan"' in stripped_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule='video.preferred_backend = "vulkan" не должен быть public config value',
                        matched_text=stripped_line,
                    )
                )

            if "VideoBackendPreference::Vulkan" in stripped_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule="VideoBackendPreference не должен возвращать Vulkan video backend variant",
                        matched_text=stripped_line,
                    )
                )

            if "settings.video.preferred_backend.vulkan" in stripped_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule="settings registry/UI не должен публиковать Vulkan video backend option",
                        matched_text=stripped_line,
                    )
                )

    return violations


def is_allowed_removed_vulkan_video_backend_reference(
    relative_path: Path,
    stripped_line: str,
) -> bool:
    """Оставляет только rejection diagnostics для старого удалённого значения."""

    if relative_path == Path("crates/config/src/store.rs"):
        return 'preferred_backend = "vulkan"' in stripped_line
    if relative_path != Path("crates/config/src/schema.rs"):
        return False
    return (
        'REMOVED_VULKAN_VIDEO_BACKEND_PREFERENCE: &str = "vulkan"' in stripped_line
        or 'video.preferred_backend = "vulkan" удал' in stripped_line
        or "REMOVED_VULKAN_VIDEO_BACKEND_PREFERENCE => Err" in stripped_line
    )


def find_direct_ffmpeg_type_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает raw FFmpeg identifiers за пределами `video-ffmpeg`."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_files_with_suffixes(
        repo_root,
        SOURCE_POLICY_SCAN_ROOTS,
        RUST_SOURCE_SUFFIXES,
    ):
        if path_is_under_any(relative_path, DIRECT_FFMPEG_TYPE_ALLOWED_ROOTS):
            continue
        violations.extend(
            find_regex_line_violations(
                repo_root,
                relative_path,
                DIRECT_FFMPEG_TYPE_PATTERNS,
            )
        )
    return violations


def find_cpu_rgb_conversion_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает FFmpeg/swscale-style CPU color conversion artifacts в source tree."""

    return find_regex_violations_in_roots(
        repo_root,
        SOURCE_POLICY_SCAN_ROOTS,
        RUST_SOURCE_SUFFIXES,
        CPU_RGB_CONVERSION_PATTERNS,
    )


def find_ffmpeg_hardware_decode_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает FFmpeg hardware decode API даже внутри `video-ffmpeg`."""

    return find_regex_violations_in_roots(
        repo_root,
        SOURCE_POLICY_SCAN_ROOTS,
        RUST_SOURCE_SUFFIXES,
        FFMPEG_HARDWARE_DECODE_PATTERNS,
    )


def find_direct_vaapi_display_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает raw VA display/session API за пределами `video-vaapi` owner-а."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_files_with_suffixes(
        repo_root,
        SOURCE_POLICY_SCAN_ROOTS,
        RUST_SOURCE_SUFFIXES,
    ):
        if path_is_under_any(relative_path, DIRECT_VAAPI_DISPLAY_ALLOWED_ROOTS):
            continue
        violations.extend(
            find_regex_line_violations(repo_root, relative_path, DIRECT_VAAPI_DISPLAY_PATTERNS)
        )
    return violations


def find_main_video_second_session_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Проверяет, что main-video scrub/seek route не создаёт второй backend/session."""

    return find_regex_violations_in_paths(
        repo_root,
        MAIN_VIDEO_REUSED_DECODER_SCAN_PATHS,
        MAIN_VIDEO_SECOND_SESSION_PATTERNS,
    )


def find_prepared_branch_promotion_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Проверяет, что branch promotion не становится app/render/backend API."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_files_with_suffixes(
        repo_root,
        SOURCE_POLICY_SCAN_ROOTS,
        RUST_SOURCE_SUFFIXES,
    ):
        if path_is_under_any(relative_path, PREPARED_BRANCH_PROMOTION_ALLOWED_ROOTS):
            continue
        violations.extend(
            find_regex_line_violations(repo_root, relative_path, PREPARED_BRANCH_PROMOTION_PATTERNS)
        )
    return violations


def find_required_source_anchor_violations(
    repo_root: Path,
    required_source_anchors: tuple[RequiredSourceAnchor, ...] = REQUIRED_SOURCE_ANCHORS,
) -> list[SourcePolicyViolation]:
    """Проверяет наличие focused тестов и boundary callsites из versioned policy."""

    violations: list[SourcePolicyViolation] = []
    for relative_path, required_anchors, rule in required_source_anchors:
        path = repo_root / relative_path
        if not path.is_file():
            violations.append(
                SourcePolicyViolation(
                    path=relative_path,
                    line_number=0,
                    rule=rule,
                    matched_text="required source file is missing",
                )
            )
            continue

        text = read_text_lossy(path)
        for required_anchor in required_anchors:
            if required_anchor not in text:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=0,
                        rule=rule,
                        matched_text=f"missing `{required_anchor}`",
                    )
                )
    return violations


def find_regex_violations_in_roots(
    repo_root: Path,
    relative_roots: tuple[str, ...],
    suffixes: frozenset[str],
    patterns: tuple[tuple[re.Pattern[str], str], ...],
) -> list[SourcePolicyViolation]:
    """Ищет regex guardrails в заданных roots и возвращает нарушения с line numbers."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_files_with_suffixes(repo_root, relative_roots, suffixes):
        violations.extend(find_regex_line_violations(repo_root, relative_path, patterns))
    return violations


def find_regex_violations_in_paths(
    repo_root: Path,
    relative_paths: tuple[Path, ...],
    patterns: tuple[tuple[re.Pattern[str], str], ...],
    *,
    suffixes: frozenset[str] = RUST_SOURCE_SUFFIXES,
) -> list[SourcePolicyViolation]:
    """Ищет regex guardrails в точечных файлах/директориях."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_paths_with_suffixes(repo_root, relative_paths, suffixes):
        violations.extend(find_regex_line_violations(repo_root, relative_path, patterns))
    return violations


def find_regex_line_violations(
    repo_root: Path,
    relative_path: Path,
    patterns: tuple[tuple[re.Pattern[str], str], ...],
) -> list[SourcePolicyViolation]:
    """Проверяет один файл набором regex policy rules."""

    violations: list[SourcePolicyViolation] = []
    text = read_text_lossy(repo_root / relative_path)
    for line_index, line in enumerate(text.splitlines(), start=1):
        stripped_line = line.strip()
        for pattern, rule in patterns:
            if pattern.search(stripped_line):
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule=rule,
                        matched_text=stripped_line,
                    )
                )
    return violations


def iter_text_files(repo_root: Path, relative_roots: tuple[str, ...]) -> list[Path]:
    """Возвращает текстовые файлы из ограниченных source roots."""

    return iter_files_with_suffixes(repo_root, relative_roots, TEXT_SOURCE_SUFFIXES)


def iter_files_with_suffixes(
    repo_root: Path,
    relative_roots: tuple[str, ...],
    suffixes: frozenset[str],
) -> list[Path]:
    """Возвращает файлы с нужными suffix-ами из ограниченных source roots."""

    text_files: list[Path] = []
    for relative_root in relative_roots:
        root = repo_root / relative_root
        if not root.exists():
            raise GuardrailError(f"source root `{relative_root}` отсутствует")
        for path in root.rglob("*"):
            if path.is_file() and path.suffix in suffixes:
                text_files.append(path.relative_to(repo_root))
    return sorted(text_files)


def iter_paths_with_suffixes(
    repo_root: Path,
    relative_paths: tuple[Path, ...],
    suffixes: frozenset[str],
) -> list[Path]:
    """Возвращает файлы из точечных path-ов, где path может быть файлом или директорией."""

    source_files: set[Path] = set()
    for relative_path in relative_paths:
        path = repo_root / relative_path
        if not path.exists():
            raise GuardrailError(f"source path `{relative_path}` отсутствует")
        if path.is_file():
            if path.suffix in suffixes:
                source_files.add(relative_path)
            continue
        if path.is_dir():
            for child_path in path.rglob("*"):
                if child_path.is_file() and child_path.suffix in suffixes:
                    source_files.add(child_path.relative_to(repo_root))
            continue
        raise GuardrailError(f"source path `{relative_path}` не файл и не директория")
    return sorted(source_files)


def path_is_under_any(relative_path: Path, allowed_roots: tuple[Path, ...]) -> bool:
    """Проверяет, находится ли относительный путь внутри одного из разрешённых roots."""

    return any(
        relative_path == allowed_root or allowed_root in relative_path.parents
        for allowed_root in allowed_roots
    )


def read_text_lossy(path: Path) -> str:
    """Читает UTF-8 source file; ошибки кодировки считаются нарушением guardrail input."""

    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise GuardrailError(f"`{path}` не является UTF-8 текстом: {error}") from error


def find_forbidden_dependencies(
    dependency_map: dict[str, frozenset[str]],
    owner_crates: frozenset[str],
    forbidden_dependencies: frozenset[str],
    rule: str,
) -> list[DependencyViolation]:
    """Возвращает прямые зависимости, запрещённые конкретным правилом."""

    violations: list[DependencyViolation] = []
    for owner in sorted(owner_crates):
        dependencies = dependency_map.get(owner, frozenset())
        for dependency in sorted(dependencies.intersection(forbidden_dependencies)):
            violations.append(DependencyViolation(owner=owner, dependency=dependency, rule=rule))
    return violations


def find_disallowed_dependencies(
    dependency_map: dict[str, frozenset[str]],
    owner_crates: frozenset[str],
    allowed_dependencies: frozenset[str],
    rule: str,
) -> list[DependencyViolation]:
    """Возвращает прямые зависимости, которых нет в allowlist роли."""

    violations: list[DependencyViolation] = []
    for owner in sorted(owner_crates):
        dependencies = dependency_map.get(owner, frozenset())
        for dependency in sorted(dependencies.difference(allowed_dependencies)):
            violations.append(DependencyViolation(owner=owner, dependency=dependency, rule=rule))
    return violations


def find_known_debt_edges(dependency_map: dict[str, frozenset[str]]) -> list[tuple[str, str, str]]:
    """Находит текущий temporary debt, который документируется как warning."""

    known_debt_edges: list[tuple[str, str, str]] = []
    for (owner, dependency), explanation in sorted(KNOWN_DEBT_EDGES.items()):
        if dependency in dependency_map.get(owner, frozenset()):
            known_debt_edges.append((owner, dependency, explanation))
    return known_debt_edges


def print_success(known_debt_edges: list[tuple[str, str, str]]) -> None:
    """Печатает успешный результат и текущий зафиксированный долг."""

    print("Refactor guardrails: OK")
    if not known_debt_edges:
        return

    print("Temporary debt, documented and allowed for now:")
    for owner, dependency, explanation in known_debt_edges:
        print(f"  warning: {owner} -> {dependency}: {explanation}")


def print_failures(
    missing_role_crates: list[str],
    reintroduced_workspace_crates: list[str],
    violations: list[DependencyViolation],
    source_policy_violations: list[SourcePolicyViolation],
) -> None:
    """Печатает все найденные ошибки за один запуск."""

    print("Refactor guardrails: FAILED", file=sys.stderr)
    if missing_role_crates:
        print("Missing required role crates:", file=sys.stderr)
        for crate_name in missing_role_crates:
            print(f"  - {crate_name}", file=sys.stderr)

    if reintroduced_workspace_crates:
        print("Removed workspace crates reintroduced:", file=sys.stderr)
        for crate_name in reintroduced_workspace_crates:
            print(f"  - {crate_name}", file=sys.stderr)

    if violations:
        print("Forbidden direct manifest dependencies:", file=sys.stderr)
        for violation in violations:
            print(
                f"  - {violation.owner} -> {violation.dependency}: {violation.rule}",
                file=sys.stderr,
            )

    if source_policy_violations:
        print("Forbidden source/config policy matches:", file=sys.stderr)
        for violation in source_policy_violations:
            line_suffix = f":{violation.line_number}" if violation.line_number else ""
            print(
                f"  - {violation.path}{line_suffix}: {violation.rule}: "
                f"{violation.matched_text}",
                file=sys.stderr,
            )


def run() -> int:
    """Запускает проверку и возвращает процессный exit code."""

    repo_root = repository_root()
    metadata = load_cargo_metadata(repo_root)
    packages = workspace_packages(metadata)
    workspace_dependencies = workspace_dependency_names(repo_root)

    dependency_policy_result = evaluate_dependency_graph_policies(
        packages,
        workspace_dependencies,
    )
    source_policy_violations = find_source_policy_violations(repo_root)
    if (
        dependency_policy_result.missing_role_crates
        or dependency_policy_result.reintroduced_workspace_crates
        or dependency_policy_result.dependency_violations
        or source_policy_violations
    ):
        print_failures(
            dependency_policy_result.missing_role_crates,
            dependency_policy_result.reintroduced_workspace_crates,
            dependency_policy_result.dependency_violations,
            source_policy_violations,
        )
        return 1

    dependency_map = direct_normal_dependencies(packages)
    print_success(find_known_debt_edges(dependency_map))
    return 0


def main() -> None:
    """Преобразует ожидаемые ошибки в понятный stderr и exit code."""

    try:
        exit_code = run()
    except GuardrailError as error:
        print(f"Refactor guardrails: ERROR: {error}", file=sys.stderr)
        exit_code = 2
    raise SystemExit(exit_code)


if __name__ == "__main__":
    main()
