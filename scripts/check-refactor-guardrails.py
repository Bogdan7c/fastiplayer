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


METADATA_COMMAND = (
    "cargo",
    "+1.99.0",
    "metadata",
    "--locked",
    "--no-deps",
    "--format-version",
    "1",
)

CONTRACT_CRATES = frozenset(
    {
        "atomic-file-store",
        "browser-bridge-protocol",
        "browser-bridge-session",
        "audio-core",
        "bounded-xml-reader",
        "dash-mpd-core",
        "smooth-streaming-manifest-core",
        "smooth-streaming-fmp4",
        "media-core",
        "natural-sort-key",
        "playlist-core",
        "playlist-io",
        "playlist-state",
        "subtitle-core",
        "subtitle-formats",
        "subtitle-pgs",
        "subtitle-raster",
        "subtitle-sidecar",
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
        "hls-playlist-core",
        "web-media-core",
        "web-media-playback-plan",
        "web-media-transport-api",
    }
)

REQUIRED_ROLE_CRATES = frozenset(
    {
        "ac3-decoder",
        "pushed-media-source",
        "browser-bridge-protocol",
        "browser-bridge-session",
        "ac3-elementary-demux",
        "ac3-syncframe",
        "animation-core",
        "app-egui",
        "atomic-file-store",
        "audio",
        "audio-core",
        "bounded-xml-reader",
        "dash-mpd-core",
        "smooth-streaming-manifest-core",
        "smooth-streaming-fmp4",
        "capability-core",
        "codec-core",
        "desktop-integration",
        "demux-api",
        "frame-server-core",
        "flv-demux",
        "hls-playlist-core",
        "media-prefetch",
        "media-source-open",
        "media-core",
        "natural-sort-key",
        "mpeg-ts-demux",
        "player-core",
        "playlist-core",
        "playlist-io",
        "playlist-discovery",
        "playlist-state",
        "render-core",
        "render-shell-api",
        "render-wgpu-shell",
        "render-wgpu-video",
        "fastiplayer-config",
        "fastiplayer-settings",
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
        "web-media-adaptive",
        "web-media-dash",
        "web-media-hls",
        "web-media-http",
        "web-media-ftp",
        "web-media-smooth",
        "web-media-playback-plan",
        "web-media-transport-api",
    }
)

# Crates из этого списка были удалены из workspace и не должны возвращаться
# как "reference" backend-ы без отдельного архитектурного решения.
REMOVED_WORKSPACE_CRATES = frozenset({"video-vulkan"})

VIDEO_FRAME_CONTRACT_ALLOWED_DEPENDENCIES = frozenset({"serde"})

# Контракт «приложение ↔ рендер» (issue #6, план user/gles2-renderer/): только нейтральные
# crate-ы плеера и egui ради типов мешей/текстур интерфейса. Allowlist автоматически запрещает
# wgpu/ash/glow/khronos-egl/winit/egui-wgpu, player-core, конкретные декодеры и рендеры.
# Сессия 02 плана `user/gles2-renderer/`: GPU-API и материализаторы кадров — внутри рендеров.
# Приложение держит `Box<dyn RenderShell>`; `render-wgpu-shell` остаётся только ради фабрики
# каталога рендеров, а `wgpu`/`render-wgpu-video` нужны лишь GPU-тестам (dev-dependencies).
APP_EGUI_FORBIDDEN_RENDERER_DEPENDENCIES = frozenset(
    {
        "ash",
        "glow",
        "khronos-egl",
        "render-wgpu-video",
        "wgpu",
        "wgpu-core",
        "wgpu-hal",
        "wgpu-types",
    }
)

RENDER_SHELL_API_ALLOWED_DEPENDENCIES = frozenset(
    {
        "egui",
        "render-core",
        "subtitle-raster",
        "video-backend-api",
        "video-core",
        "video-frame-contract",
        "video-present-core",
    }
)

# Переиспользуемый фундамент своего рендера GLES2 (issue #6, сессии 03–04 плана
# user/gles2-renderer/): EGL-контекст, возможности драйвера, показ, fence, художник egui, маска
# углов. Подключается к любому egui-приложению, поэтому normal-зависимости — только тонкие
# «провода» к драйверу, типы мешей/текстур egui (`epaint`, без библиотеки виджетов `egui`) и
# мелкие утилиты из allowlist-а.
GLES2_EGUI_ALLOWED_DEPENDENCIES = frozenset(
    {
        "bytemuck",
        "epaint",
        "glow",
        "khronos-egl",
        "log",
        "raw-window-handle",
        "thiserror",
        "wayland-sys",
    }
)
# Переиспользуемый видеослой GLES2 (сессия 05 плана user/gles2-renderer/): YUV-плоскости, кольцо
# текстур, цвет. Из workspace — только `gles2-egui` (контекст, кадр, fence, слой кадра), снаружи —
# `glow` и мелкие утилиты. Словарь кадра свой, не `video-core`.
GLES2_VIDEO_ALLOWED_DEPENDENCIES = frozenset(
    {
        "gles2-egui",
        "glow",
        "log",
        "thiserror",
    }
)
# Переиспользуемые GLES2-crate-ы не зависят ни от одного crate-а workspace, кроме своих `gles2-*`,
# даже в dev-dependencies (иначе тесты притянули бы плеер в чужое приложение).
REUSABLE_GLES2_CRATES = frozenset({"gles2-egui", "gles2-video"})
REUSABLE_GLES2_WORKSPACE_PREFIX = "gles2-"

# Адаптер плеера к GLES2-рендеру (`render-gles2-shell`, сессии 05–09): переводит дескрипторы
# `video-core` в словарь `gles2-video`, объявляет `RenderCapabilities`. Он — другая реализация
# того же шва, что wgpu-шелл, поэтому не видит wgpu и wgpu-рендер, а также плеер, приложение и
# конкретные декодеры (кадры приходят нейтральными дескрипторами). Dev-зависимость на декодер
# допустима только для функциональных тестов (как у wgpu-рендера).
RENDER_GLES2_SHELL_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "ash",
        "egui-wgpu",
        "player-core",
        "render-wgpu-shell",
        "render-wgpu-video",
        "video-ffmpeg",
        "video-vaapi",
        "wgpu",
        "wgpu-hal",
        "wgpu-types",
    }
)

# Будущие crate-ы своего рендера GLES2 (`gles2-egui`, `gles2-video`, `render-gles2-shell`,
# сессии 03–09 плана user/gles2-renderer/) перечислены в forbidden-наборах ниже: нейтральные
# контракты, плеер, декодеры и wgpu-рендер не должны зависеть от них ни сейчас, ни потом.

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
        # Pure in-memory перекодировка Windows-1251 для legacy .m3u/.cue (B1):
        # без filesystem/network/threads, граница parser-а не нарушается.
        "encoding_rs",
        "hls-playlist-core",
        "media-core",
        "playlist-core",
        "url",
    }
)

# Shared RFC 8216 owner выполняет только pure text/reference validation.
HLS_PLAYLIST_CORE_ALLOWED_DEPENDENCIES = frozenset(
    {"thiserror", "unicode-normalization", "url"}
)

# DASH schema owner зависит только от hardened XML boundary, exact UTC parser и typed errors.
DASH_MPD_CORE_ALLOWED_DEPENDENCIES = frozenset(
    {"bounded-xml-reader", "thiserror", "time"}
)

# Smooth Streaming schema owner зависит только от hardened XML boundary и typed errors.
SMOOTH_STREAMING_MANIFEST_CORE_ALLOWED_DEPENDENCIES = frozenset(
    {"bounded-xml-reader", "thiserror"}
)

# Smooth fMP4 adapter пересекает только sealed manifest model и единственного
# ISO-BMFF reconstruction owner-а; transport/demux/player edges запрещены.
SMOOTH_STREAMING_FMP4_ALLOWED_DEPENDENCIES = frozenset(
    {"smooth-streaming-manifest-core", "symphonia-format-isomp4", "thiserror"}
)

# Общий natural comparator остаётся std-only и не знает path/domain owners.
NATURAL_SORT_KEY_ALLOWED_DEPENDENCIES = frozenset()

# Atomic file store владеет только std filesystem durability protocol.
ATOMIC_FILE_STORE_ALLOWED_DEPENDENCIES = frozenset()

# Протокол моста в браузер — только типы и разбор: без I/O, D-Bus, сокетов и
# crate-ов плеера. Его же читают посредник (05) и браузерный режим (02–04).
BROWSER_BRIDGE_PROTOCOL_ALLOWED_DEPENDENCIES = frozenset(
    {"serde", "serde_json", "thiserror"}
)

# Серверная часть сеанса моста (сессия 02): протокол, сокеты WebSocket без tokio,
# случайные байты ОС для пропусков, ошибки и журнал. Без D-Bus, UI и crate-ов плеера.
BROWSER_BRIDGE_SESSION_ALLOWED_DEPENDENCIES = frozenset(
    {"browser-bridge-protocol", "getrandom", "thiserror", "tracing", "tungstenite"}
)

# Источник «сегменты извне» (сессия 03a моста): нейтрален к браузеру — знает только
# границы демуксеров и сами демуксеры, без протокола моста, сокетов, плеера и UI.
PUSHED_MEDIA_SOURCE_ALLOWED_DEPENDENCIES = frozenset(
    {
        "anyhow",
        "bytes",
        "demux-api",
        "media-core",
        "media-prefetch",
        "mpeg-ts-demux",
        "source-core",
        "symphonia-demux",
        "thiserror",
        "tracing",
    }
)

# Hardened XML boundary не должен получить filesystem/network/domain dependencies.
BOUNDED_XML_READER_ALLOWED_DEPENDENCIES = frozenset({"quick-xml", "thiserror"})

# Нейтральная модель субтитров знает только BCP 47 теги и typed errors: ни GPU, ни UI, ни форматов.
SUBTITLE_CORE_ALLOWED_DEPENDENCIES = frozenset({"oxilangtag", "thiserror"})

# Парсеры текстовых субтитров получают только модель, декодирование и typed errors.
SUBTITLE_FORMATS_ALLOWED_DEPENDENCIES = frozenset(
    {"subtitle-core", "encoding_rs", "chardetng", "thiserror", "bounded-xml-reader"}
)

# Декодер PGS (Blu-ray): только модель субтитров и typed errors; ни GPU, ни UI, ни контейнера.
SUBTITLE_PGS_ALLOWED_DEPENDENCIES = frozenset({"subtitle-core", "thiserror"})

# Поиск файлов субтитров рядом с видео: только модель и typed errors (std::fs — часть std).
SUBTITLE_SIDECAR_ALLOWED_DEPENDENCIES = frozenset({"subtitle-core", "thiserror"})

# Растеризатор субтитров: модель, разбор шрифтов/шейпинг/BiDi и typed errors; никакого GPU/UI/плеера.
SUBTITLE_RASTER_ALLOWED_DEPENDENCIES = frozenset(
    {
        "subtitle-core",
        "subtitle-pgs",
        "skrifa",
        "harfrust",
        "fontdb",
        "unicode-bidi",
        "thiserror",
        "tracing",
    }
)

# Neutral web-media values не должны напрямую знать process/service/network/app owners.
WEB_MEDIA_CORE_ALLOWED_DEPENDENCIES = frozenset()

# AC-3/E-AC-3 syncframe parser — std-only bitstream contract: его используют
# demuxer-ы (MPEG-TS, raw .ac3/.eac3) до декодера, поэтому decoder/container
# зависимости (сторонние декодеры, Symphonia, FFmpeg) в нём запрещены.
AC3_SYNCFRAME_ALLOWED_DEPENDENCIES = frozenset()

# Собственный clean-room декодер AC-3/E-AC-3 — std-only: весь разбор, таблицы
# и DSP по спецификациям A/52 / ETSI TS 102 366 живут внутри crate-а; чужие
# декодеры (Symphonia, FFmpeg и т. п.) и player/UI-зависимости запрещены.
AC3_DECODER_ALLOWED_DEPENDENCIES = frozenset()

# Декодер AC-3/E-AC-3 — деталь concrete `audio`: только его crate-private
# adapter `decoder::ac3` переводит PCM/позиции в neutral `AudioDecoder` boundary.
# Остальные crate-ы (demux, player-core, app) получают звук через `audio-core` и
# не должны знать конкретный декодер (dev-зависимости для тестов не ограничены).
AC3_DECODER_ALLOWED_DEPENDENTS = frozenset({"audio"})

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

# Concrete FTP provider переиспользует source-core FTP session и neutral transport
# API; HTTP client/prefetch/demux/service/player dependencies запрещены.
WEB_MEDIA_FTP_ALLOWED_DEPENDENCIES = frozenset(
    {"source-core", "thiserror", "web-media-transport-api"}
)

# Direct-media service только классифицирует locator по фактическим demux capabilities.
# Concrete transport, prefetch, demux runtime и app lifecycle принадлежат composition root.
SERVICE_DIRECT_MEDIA_ALLOWED_DEPENDENCIES = frozenset(
    {"demux-api", "source-core", "thiserror", "url", "web-media-transport-api"}
)

# Shared adaptive owner использует только neutral HTTP/policy/demux boundaries.
# Concrete HLS/DASH parser, reqwest, service, player, UI и cache edges запрещены.
WEB_MEDIA_ADAPTIVE_ALLOWED_DEPENDENCIES = frozenset(
    {
        "anyhow",
        # Bounded single-waker primitive прерывает active HTTP body без lock/polling.
        "atomic-waker",
        "bytes",
        "demux-api",
        "media-core",
        "source-core",
        "thiserror",
        "web-media-transport-api",
    }
)

# HLS runtime владеет concrete manifest policy и публикует provider-neutral
# catalog через web-media-core. Concrete container owners допустимы лишь в dev fixtures.
WEB_MEDIA_HLS_ALLOWED_DEPENDENCIES = frozenset(
    {
        "aes",
        "anyhow",
        "bytes",
        "cbc",
        "demux-api",
        "hls-playlist-core",
        # Neutral `log` facade публикует только typed secret-safe HLS evidence;
        # concrete logger/backend по-прежнему принадлежит composition root.
        "log",
        "media-core",
        # Чистый хэш для компактных opaque-ключей строк каталога (catalog/identity.rs);
        # не backend demux/transport.
        "sha2",
        "source-core",
        "thiserror",
        "web-media-adaptive",
        "web-media-core",
        "web-media-transport-api",
        "zeroize",
    }
)

# DASH runtime владеет manifest/serialized planning и публикует provider-neutral
# catalog через web-media-core, не перенося parser/runtime state в app.
WEB_MEDIA_DASH_ALLOWED_DEPENDENCIES = frozenset(
    {
        "anyhow",
        "bounded-xml-reader",
        "bytes",
        "dash-mpd-core",
        "demux-api",
        "media-core",
        "source-core",
        "thiserror",
        "web-media-adaptive",
        "web-media-core",
        "web-media-transport-api",
    }
)

# Smooth VOD runtime пересекает hardened XML/schema/F2/F1 owners, neutral
# adaptive/source/C3/transport contracts и neutral P4/P5 demux orchestration.
# Concrete demux backend, player, service и UI остаются у composition layers.
WEB_MEDIA_SMOOTH_ALLOWED_DEPENDENCIES = frozenset(
    {
        "anyhow",
        "bounded-xml-reader",
        "bytes",
        "demux-api",
        "media-core",
        "sha2",
        "smooth-streaming-fmp4",
        "smooth-streaming-manifest-core",
        "source-core",
        "symphonia-format-isomp4",
        "thiserror",
        "web-media-adaptive",
        "web-media-core",
        "web-media-transport-api",
    }
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
        "web-media-ftp",
    }
)

# Открытие источников (local/direct/web-media leaves), вынесенное из app-egui.
# Crate остаётся без UI/окна/GPU: зависимость от egui/winit/wgpu/render-* или
# обратно на app-egui вернула бы источник в composition root.
MEDIA_SOURCE_OPEN_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "egui",
        "egui-wgpu",
        "egui-winit",
        "render-core",
        "render-gles2-shell",
        "render-shell-api",
        "render-wgpu-shell",
        "render-wgpu-video",
        "ui-artwork-egui",
        "wgpu",
        "winit",
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
        "ac3-syncframe",
        "anyhow",
        "bytes",
        "codec-core",
        "demux-api",
        "media-core",
        "source-core",
        "thiserror",
        # Диагностика восстановимых повреждений (потеря синхронизации AC-3) —
        # тот же логгер, что у raw `ac3-elementary-demux`; не network/player/UI.
        "tracing",
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

# Сырые `.ac3`/`.eac3`: framing только через std-only `ac3-syncframe`, без
# декодера, Symphonia/FFmpeg, сети, player-а и UI — тот же уровень,
# что у остальных first-party container owner-ов.
AC3_ELEMENTARY_DEMUX_ALLOWED_DEPENDENCIES = frozenset(
    {
        "ac3-syncframe",
        "anyhow",
        "bytes",
        "demux-api",
        "media-core",
        "source-core",
        "thiserror",
        "tracing",
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
        # Наш crate сборки закреплённого FFmpeg: та же граница, что у FFmpeg-биндингов.
        "ffmpeg-vendor",
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
        "render-gles2-shell",
        "render-shell-api",
        "render-wgpu-shell",
        "render-wgpu-video",
        "fastiplayer-config",
        "fastiplayer-settings",
        "gles2-egui",
        "gles2-video",
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
        "gles2-egui",
        "gles2-video",
        "render-gles2-shell",
        "render-shell-api",
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
        "gles2-egui",
        "gles2-video",
        "render-gles2-shell",
        "render-shell-api",
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
        "gles2-egui",
        "gles2-video",
        "player-core",
        "render-core",
        "render-gles2-shell",
        "render-shell-api",
        "render-wgpu-shell",
        "render-wgpu-video",
        "wgpu",
        "wgpu-types",
    }
)

# Normal-зависимости render-wgpu-shell. GLES2-crate-ы здесь запрещены; dev-зависимости на
# `gles2-egui` (сессия 04), `gles2-video` и `render-gles2-shell` (сессия 05) — осознанное
# исключение (решение владельца 28 плана gles2-renderer): пиксельный паритет интерфейса и видео
# сравнивает настоящий wgpu-путь с GLES2 в тестах шелла, без копий.
RENDER_WGPU_SHELL_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "audio",
        "gles2-egui",
        "gles2-video",
        "player-core",
        "render-gles2-shell",
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
        "gles2-egui",
        "gles2-video",
        "player-core",
        "pollster",
        "render-gles2-shell",
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
    "crates/fastiplayer-settings",
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

# Exact-копия upstream ffmpeg-sys-next (патч vendored-режима): это сами FFmpeg-биндинги,
# поэтому политики «raw FFmpeg только в video-ffmpeg / без swscale / без hwaccel»
# относятся к нашему коду, который их использует, а не к сгенерированному API.
UPSTREAM_FFMPEG_BINDINGS_ROOT = Path("crates/ffmpeg-sys-next-patch")

DIRECT_FFMPEG_TYPE_ALLOWED_ROOTS = (Path("crates/video-ffmpeg"), UPSTREAM_FFMPEG_BINDINGS_ROOT)

DIRECT_FFMPEG_TYPE_PATTERNS = (
    (
        re.compile(
            r"\bAV(?:Frame|Packet|Codec|CodecContext|PixelFormat|Rational|Dictionary|BufferRef)\b"
        ),
        "raw FFmpeg/libav types should be inside the video ffmpeg",
    ),
    (
        re.compile(r"\bAVERROR\b"),
        "raw FFmpeg/libav error macros You have to stay inside video-ffmpeg",
    ),
    (
        re.compile(r"\bffmpeg_sys_next::|\bffmpeg_next::|\brsmpeg::"),
        "raw FFmpeg Rust bindings should only be used inside video-ffmpeg",
    ),
)

CPU_RGB_CONVERSION_PATTERNS = (
    (
        re.compile(r"\bsws_scale\b|\bsws_getContext\b|\bSwsContext\b"),
        "swscale CPU conversion forbidden playback/source tree",
    ),
    (
        re.compile(r"\blibswscale\b|\bav_image_convert\b|\bavpicture_"),
        "CPU RGB/YUV conversion helpers banned in playback/source tree",
    ),
)

FFMPEG_HARDWARE_DECODE_PATTERNS = (
    (
        re.compile(r"\bav_hwdevice_|\bav_hwframe_|\bAVHW(?:Device|Frames)"),
        "FFmpeg hardware decode/device API banned: native hardware path lives outside FFmpeg",
    ),
    (
        re.compile(r"\bhwaccel\b|\bhw_frames\b"),
        "FFmpeg hwaccel path prohibited: video-ffmpeg remains software-decode-only",
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
        "VA display/session opens only inside video-vaapi boundary owner",
    ),
    (
        re.compile(r"\bopen_drm_display\s*\("),
        "DRM VA display/session opens only inside video-vaapi boundary owner",
    ),
    (
        re.compile(r"\bVADisplay\b|\bvaGetDisplay\b|\bvaInitialize\b"),
        "raw VA display handle/API should not be beyond the video-vaapi boundary",
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
        "main-video SeekLanding/LiveScrub should reuse playback session/decoder, not start the second session/backend",
    ),
    (
        re.compile(
            r"\b(VaapiVideoBackendFactory|FfmpegSoftwareVideoBackendFactory)::new"
        ),
        "main-video scrub/prepared seek should not create a second decoder backend",
    ),
)

PREPARED_BRANCH_PROMOTION_ALLOWED_ROOTS = (
    Path("crates/frame-server-core"),
    Path("crates/player-core/src/session/prepared_seek.rs"),
)

PREPARED_BRANCH_PROMOTION_PATTERNS = (
    (
        re.compile(r"\bpromote_prepared_frame\s*\("),
        "prepared branch promotion only permitted frame-server-core boundary and S17 player-core owner-at",
    ),
)

# S18 закрепляет canonical top-level read boundary у каждого structural consumer-а.
PLAYLIST_TOPOLOGY_SOURCE_ANCHORS = (
    (
        Path("crates/playlist-state/src/dto/v2.rs"),
        ("iter_top_level_entries()",),
        "playlist-state v2 must retain canonical top-level entry order",
    ),
    (
        Path("crates/playlist-io/src/export/snapshot.rs"),
        ("iter_top_level_entries()",),
        "playlist export must take off canonical top-level entry snapshot",
    ),
    (
        Path("crates/app-egui/src/playlist_runtime/view.rs"),
        ("iter_top_level_entries()",),
        "Playlist UI read model must be built from canonical top-level entries",
    ),
    (
        Path("crates/app-egui/src/playlist_runtime/compound_view.rs"),
        ("iter_top_level_entries()",),
        "compound view must-be child rows from canonical top-level entries",
    ),
    (
        Path("crates/app-egui/src/playlist_runtime/external_projection.rs"),
        ("top_level_entry(entry_id)",),
        "external projection must permit compound context through canonical top-level entry",
    ),
)

# Structural consumers не должны снова подменять top-level order derived playable traversal-ом.
PLAYLIST_TOPOLOGY_DERIVED_READ_PATTERNS = (
    (
        re.compile(r"\biter_playable_(?:items|ids)\s*\("),
        "persistence/UI/external structural consumer shouldn't flatten canonical top-level entries",
    ),
    (
        re.compile(r"\bowned_playable_items_snapshot\s*\("),
        "persistence/UI/external structural consumer keep out derived playable snapshot how canonical state",
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
        "Playlist UI/external projection must not disclose secret-bearing identity",
    ),
)

# S27 фиксирует три production consumer-а единственной app-owned web-open composition.
PROGRESSIVE_WEB_SOURCE_ANCHORS = (
    (
        Path("crates/app-egui/src/startup_media.rs"),
        ("crate::web_media_open::prepare_yt_dlp_web_media(",),
        "CLI startup must use a single progressive web-open composition path.",
    ),
    (
        Path("crates/app-egui/src/media_open/preparation.rs"),
        ("crate::web_media_open::prepare_yt_dlp_web_media(",),
        "queue media-open must use a single progressive web-open composition path",
    ),
    (
        Path("crates/app-egui/src/state/settings_media_rebuild/prepare.rs"),
        ("cancel.prepare(MediaOpenSourceRequest::Web(",),
        "settings rebuild must use a single provider-neutral media-open preparation path",
    ),
    (
        Path("crates/service-ytdlp/src/candidate/tests.rs"),
        ("public_surface_and_manifest_have_no_legacy_webm_opener",),
        "service-ytdlp must retain focused proof of deletion of legacy WebM opener",
    ),
    (
        Path("scripts/progressive-web-smoke.sh"),
        ("--url", "redact_runtime_log", "MANUAL REVIEW REQUIRED"),
        "manual progressive runner must accept explicit URL and write redacted report only.",
    ),
    (
        Path("scripts/tests/progressive-web-smoke-self-test.sh"),
        ("<redacted-url>", "<redacted-secret-line>"),
        "manual progressive runner must focused redaction self-test",
    ),
)

# Старые service-owned WebM opener symbols не должны вернуться ни в Rust, ни в runtime scripts.
PROGRESSIVE_WEB_LEGACY_SCAN_PATHS = (
    Path("crates/app-egui/src"),
    # Web-open composition (`web_media_open`) вынесена в media-source-open (session-05).
    Path("crates/media-source-open/src"),
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
        "legacy service-owned WebM opener/runtime scenario banned after S27",
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
    # Доменная часть stream model и catalog вынесена в media-source-open (session-02).
    Path("crates/media-source-open/src/web_media_stream_model.rs"),
    Path("crates/media-source-open/src/web_media_stream_model"),
    Path("crates/media-source-open/src/web_media_catalog.rs"),
    Path("crates/media-source-open/src/web_media_catalog"),
)

PROGRESSIVE_WEB_TRANSIENT_SECRET_PATTERNS = (
    (
        re.compile(
            r"\b(?:HttpRequestTarget|ScopedHttpCookieJar|SecretRequestContext|"
            r"TransportOpenRequest|YtDlpRequestMaterial)\b"
        ),
        "durable state/config/URL UI should not depend on transient transport secrets",
    ),
)

# S28G включается только в repository, где существует concrete Symphonia factory.
EXISTING_DEMUX_BOUNDARY_MARKER = Path("crates/symphonia-demux/src/factory.rs")

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
        "S28G factory descriptor must retain exact S28A/B/C fixture inventory",
    ),
    (
        Path("crates/symphonia-demux/src/factory/tests.rs"),
        ("descriptor_lists_exact_s28_foundation_fixture_ids",),
        "S28G fixture inventory must be focused exact regression test",
    ),
    (
        Path("crates/symphonia-demux/src/factory/tests/matroska.rs"),
        ("exact_matroska_doctype_opens_local_and_ordered_inputs",),
        "Matroska registration can not be proved only by WebM DocType fixture",
    ),
    (
        Path("crates/symphonia-demux/src/matroska_metadata.rs"),
        ("cluster_payload_is_opaque_to_bounded_metadata_and_cue_indexer",),
        "bounded Matroska metadata/cue indexer must-be opaque Cluster proof",
    ),
    (
        Path("docs/existing-demux-s28g.md"),
        (
            "# S28G: existing-demux hardening gate",
            "symphonia/generated-fmp4-s28a",
            "symphonia/generated-matroska-ordered-s28b",
            "Known coverage limitation",
        ),
        "S28G aggregate evidence artifact must be complete and honest",
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
        "Matroska packet element IDs are exactly symphonia-format-mkv patch",
    ),
    (
        re.compile(
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+\w*"
            r"(?:cluster|simple_block|block_group|matroska_block|lacing|packet_payload)"
            r"\w*\s*\(",
            re.IGNORECASE,
        ),
        "Matroska Cluster/Block/lacing parsing belongs to exact symphonia-format-mkv patch",
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
        raise GuardrailError(f"cargo metadata returned invalid JSON: {error}") from error

    if not isinstance(metadata, dict):
        raise GuardrailError("The cargo metadata must return the top-level JSON object.")

    if metadata.get("version") != 1:
        raise GuardrailError("cargo metadata returns format-version other than expected 1")

    return metadata


def format_failed_command(completed_process: subprocess.CompletedProcess[str]) -> str:
    """Формирует диагностическое сообщение без потери stdout/stderr Cargo."""

    command_text = " ".join(METADATA_COMMAND)
    stdout_text = completed_process.stdout.strip()
    stderr_text = completed_process.stderr.strip()
    details = [f"command `{command_text}` exited with code {completed_process.returncode}"]
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
        raise GuardrailError("cargo metadata lacking packages")
    if not isinstance(workspace_members, list):
        raise GuardrailError("cargo metadata does not contain an array of workspace_members")

    packages_by_id = collect_packages_by_id(packages)
    package_names: dict[str, dict[str, Any]] = {}
    for package_id in workspace_members:
        if not isinstance(package_id, str):
            raise GuardrailError("workspace_members must contain a string package id")
        package = packages_by_id.get(package_id)
        if package is None:
            raise GuardrailError(f"workspace member `{package_id}` is missing from packages")
        package_name = read_string_field(package, "name", f"package `{package_id}`")
        if package_name in package_names:
            raise GuardrailError(f"workspace contains duplicate package name `{package_name}`")
        package_names[package_name] = package

    return package_names


def collect_packages_by_id(packages: list[Any]) -> dict[str, dict[str, Any]]:
    """Индексирует packages по Cargo package id."""

    packages_by_id: dict[str, dict[str, Any]] = {}
    for package in packages:
        if not isinstance(package, dict):
            raise GuardrailError("element packages must have JSON object")
        package_id = read_string_field(package, "id", "package")
        if package_id in packages_by_id:
            raise GuardrailError(f"cargo metadata contains duplicate package id `{package_id}`")
        packages_by_id[package_id] = package
    return packages_by_id


def read_string_field(source: dict[str, Any], field_name: str, context: str) -> str:
    """Читает обязательное строковое поле из JSON object."""

    value = source.get(field_name)
    if not isinstance(value, str):
        raise GuardrailError(f"{context} must contain string field `{field_name}`")
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
            raise GuardrailError(f"package `{package_name}` does not contain a dependencies array")

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
        raise GuardrailError(f"`{manifest_path}` contains invalid TOML: {error}") from error
    except OSError as error:
        raise GuardrailError(f"`{manifest_path}` could not be read: {error}") from error

    workspace = manifest.get("workspace", {})
    if not isinstance(workspace, dict):
        raise GuardrailError("root Cargo.toml `[workspace]' should be a TOML table")

    dependencies = workspace.get("dependencies", {})
    if not isinstance(dependencies, dict):
        raise GuardrailError("root Cargo.toml '[workspace.dependencies]' should be a TOML table")

    return frozenset(dependencies)


def is_normal_dependency(dependency: Any, package_name: str) -> bool:
    """Отличает normal dependency от dev/build dependency."""

    if not isinstance(dependency, dict):
        raise GuardrailError(f"dependency package `{package_name}` must be a JSON object")
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
                rule="FFmpeg/libav crates should not be common workspace dependencies",
            )
        )
    for dependency in sorted(
        workspace_dependencies.intersection(TEMPO_WORKSPACE_FORBIDDEN_DEPENDENCIES)
    ):
        violations.append(
            DependencyViolation(
                owner="workspace.dependencies",
                dependency=dependency,
                rule="tempo backend crates should not be common workspace dependencies before concrete adapter boundary",
            )
        )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"video-frame-contract"}),
            VIDEO_FRAME_CONTRACT_ALLOWED_DEPENDENCIES,
            "video-frame-contract remains leaf contract crate It depends only on serde",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"render-shell-api"}),
            RENDER_SHELL_API_ALLOWED_DEPENDENCIES,
            "render-shell-api is the neutral app/renderer contract without GPU/window/player/backend deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"gles2-egui"}),
            GLES2_EGUI_ALLOWED_DEPENDENCIES,
            "gles2-egui is a reusable GLES2 renderer: only glow/khronos-egl/wayland-sys, epaint and small utilities",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"gles2-video"}),
            GLES2_VIDEO_ALLOWED_DEPENDENCIES,
            "gles2-video is a reusable GLES2 video layer: only gles2-egui, glow and small utilities",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"render-gles2-shell"}),
            RENDER_GLES2_SHELL_FORBIDDEN_DEPENDENCIES,
            "render-gles2-shell is the GLES2 side of the render seam: no wgpu, player, app or concrete decoders",
        )
    )
    violations.extend(find_reusable_gles2_workspace_dependencies(all_dependency_map))
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"app-egui"}),
            APP_EGUI_FORBIDDEN_RENDERER_DEPENDENCIES,
            "app-egui sees renderers only through render-shell-api; GPU APIs and frame materializers stay inside renderer crates",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"video-present-core"}),
            VIDEO_PRESENT_CORE_ALLOWED_DEPENDENCIES,
            "video-present-core normal-deps remain neutral present boundary without player/render/backend/UI deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"frame-server-core"}),
            FRAME_SERVER_CORE_ALLOWED_DEPENDENCIES,
            "Frame-server-core is a neutral frame-server boundary without player/app/render/backend/service deps.",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"natural-sort-key"}),
            NATURAL_SORT_KEY_ALLOWED_DEPENDENCIES,
            "natural-sort-key remains std-only neutral comparator",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"atomic-file-store"}),
            ATOMIC_FILE_STORE_ALLOWED_DEPENDENCIES,
            "atomic-file-store remains std-only neutral durability boundary",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"browser-bridge-protocol"}),
            BROWSER_BRIDGE_PROTOCOL_ALLOWED_DEPENDENCIES,
            "browser-bridge-protocol is a neutral types-only bridge protocol without I/O, transport or player deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"browser-bridge-session"}),
            BROWSER_BRIDGE_SESSION_ALLOWED_DEPENDENCIES,
            "browser-bridge-session is a neutral bridge session server without UI, D-Bus, tokio or player deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"pushed-media-source"}),
            PUSHED_MEDIA_SOURCE_ALLOWED_DEPENDENCIES,
            "pushed-media-source is a browser-neutral pushed segment source over existing demuxers",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"subtitle-core"}),
            SUBTITLE_CORE_ALLOWED_DEPENDENCIES,
            "subtitle-core remains a neutral model without GPU/UI/format dependencies",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"subtitle-formats"}),
            SUBTITLE_FORMATS_ALLOWED_DEPENDENCIES,
            "subtitle-formats depends only on subtitle-core and text decoding",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"subtitle-pgs"}),
            SUBTITLE_PGS_ALLOWED_DEPENDENCIES,
            "subtitle-pgs depends only on subtitle-core and typed errors",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"subtitle-raster"}),
            SUBTITLE_RASTER_ALLOWED_DEPENDENCIES,
            "subtitle-raster depends only on subtitle-core, font/shaping libraries and diagnostics",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"subtitle-sidecar"}),
            SUBTITLE_SIDECAR_ALLOWED_DEPENDENCIES,
            "subtitle-sidecar depends only on subtitle-core and typed errors",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"bounded-xml-reader"}),
            BOUNDED_XML_READER_ALLOWED_DEPENDENCIES,
            "bounded-xml-reader does not receive filesystem/network/domain dependencies",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-core"}),
            WEB_MEDIA_CORE_ALLOWED_DEPENDENCIES,
            "web-media-core remains std-only neutral value contract",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"ac3-syncframe"}),
            AC3_SYNCFRAME_ALLOWED_DEPENDENCIES,
            "ac3-syncframe remains std-only AC-3/E-AC-3 header parser without decoder/container deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"ac3-decoder"}),
            AC3_DECODER_ALLOWED_DEPENDENCIES,
            "ac3-decoder remains std-only clean-room AC-3/E-AC-3 decoder without foreign decoder deps",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset(dependency_map).difference(AC3_DECODER_ALLOWED_DEPENDENTS),
            frozenset({"ac3-decoder"}),
            "only concrete audio adapter depends on ac3-decoder; others use audio-core boundary",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-playback-plan"}),
            WEB_MEDIA_PLAYBACK_PLAN_ALLOWED_DEPENDENCIES,
            "web-media-playback-plan remains pure neutral capability planner",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-transport-api"}),
            WEB_MEDIA_TRANSPORT_API_ALLOWED_DEPENDENCIES,
            "web-media-transport-api remains neutral provider/secret/network boundary",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-http"}),
            WEB_MEDIA_HTTP_ALLOWED_DEPENDENCIES,
            "web-media-http reuses neutral transport API, source-core and media-prefetch",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-ftp"}),
            WEB_MEDIA_FTP_ALLOWED_DEPENDENCIES,
            "web-media-ftp reuses neutral transport API and source-core",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-adaptive"}),
            WEB_MEDIA_ADAPTIVE_ALLOWED_DEPENDENCIES,
            "web-media-adaptive remains shared manifest/segment lifecycle without concrete parser/player/UI/reqwest",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"hls-playlist-core"}),
            HLS_PLAYLIST_CORE_ALLOWED_DEPENDENCIES,
            "hls-playlist-core remains pure RFC parser/model without runtime owners.",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"dash-mpd-core"}),
            DASH_MPD_CORE_ALLOWED_DEPENDENCIES,
            "dash-mpd-core remains pure MPD schema/timing model without HTTP/demux/app/yt-dlp",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"smooth-streaming-manifest-core"}),
            SMOOTH_STREAMING_MANIFEST_CORE_ALLOWED_DEPENDENCIES,
            "smooth-streaming-manifest-core remains pure MS-SSTR schema/timing model without runtime owners",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"smooth-streaming-fmp4"}),
            SMOOTH_STREAMING_FMP4_ALLOWED_DEPENDENCIES,
            "smooth-streaming-fmp4 remains pure manifest-to-ISO-BMFF adapter without transport/demux/player owners",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-hls"}),
            WEB_MEDIA_HLS_ALLOWED_DEPENDENCIES,
            "web-media-hls limited to HLS policy and neutral transport/demux contracts without concrete demux backends",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-dash"}),
            WEB_MEDIA_DASH_ALLOWED_DEPENDENCIES,
            "web-media-dash limited static DASH policy and neutral transport/demux contracts",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"web-media-smooth"}),
            WEB_MEDIA_SMOOTH_ALLOWED_DEPENDENCIES,
            "web-media-smooth limited Smooth VOD preparation and neutral transport/C3/demux contracts",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"service-ytdlp"}),
            SERVICE_YTDLP_FORBIDDEN_DEPENDENCIES,
            "service-ytdlp doesn't have a concrete HTTP/cache/demux/player playback stack",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"service-direct-media"}),
            SERVICE_DIRECT_MEDIA_ALLOWED_DEPENDENCIES,
            "service-direct-media remains classification/locator boundary without transport/demux runtime owners",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"demux-api"}),
            DEMUX_API_ALLOWED_DEPENDENCIES,
            "demux-api remains neutral registry/composition boundary without concrete backend/player/service/UI deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"mpeg-ts-demux"}),
            MPEG_TS_DEMUX_ALLOWED_DEPENDENCIES,
            "mpeg-ts-demux remains a reusable container owner without HLS/network/player/UI/FFmpeg deps.",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"ac3-elementary-demux"}),
            AC3_ELEMENTARY_DEMUX_ALLOWED_DEPENDENCIES,
            "ac3-elementary-demux remains raw AC-3 container owner without decoder/network/player/UI deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"flv-demux"}),
            FLV_DEMUX_ALLOWED_DEPENDENCIES,
            "flv-demux remains reusable FLV/F4F owner without HDS/RTMP network/player/UI/FFmpeg deps",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"symphonia-demux"}),
            SYMPHONIA_DEMUX_FORBIDDEN_ALTERNATIVE_PARSER_DEPENDENCIES,
            "symphonia-demux reuses exact Symphonia format patches and does not add a second container parser",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"playlist-core"}),
            PLAYLIST_CORE_ALLOWED_DEPENDENCIES,
            "playlist-core depends only neutral metadata/natural-key contracts and rand",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"playlist-io"}),
            PLAYLIST_IO_ALLOWED_DEPENDENCIES,
            "playlist-io is neutral parser boundary without filesystem/network/app/player/service deps",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"playlist-discovery"}),
            PLAYLIST_DISCOVERY_ALLOWED_DEPENDENCIES,
            "playlist-discovery remains UI/player/config-neutral local probe owner",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"playlist-state"}),
            PLAYLIST_STATE_ALLOWED_DEPENDENCIES,
            "playlist-state remain separate JSON/I/O owner without app/player/UI/service dependencies",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            all_dependency_map,
            frozenset(all_dependency_map).difference({"video-ffmpeg"}),
            FFMPEG_FORBIDDEN_DEPENDENCIES,
            "FFmpeg/libav crates are only allowed inside video-ffmpeg",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            all_dependency_map,
            TEMPO_NEUTRAL_CRATES,
            TEMPO_NEUTRAL_FORBIDDEN_DEPENDENCIES,
            "audio-core/player-core sees only neutral tempo boundary, without direct concrete tempo dependency",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            CONTRACT_CRATES,
            CONTRACT_FORBIDDEN_DEPENDENCIES,
            "contract crates are not dependent on shell/backend/player/UI crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            LOW_LEVEL_CRATES,
            LOW_LEVEL_FORBIDDEN_DEPENDENCIES,
            "media/codec/audio/demux layer is independent of GPU/video backend crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"player-core"}),
            PLAYER_CORE_FORBIDDEN_DEPENDENCIES,
            "player-core adds direct dependency on shell/service/demux/video backend/GPU crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            VIDEO_BACKEND_CRATES,
            VIDEO_BACKEND_FORBIDDEN_DEPENDENCIES,
            "concrete video backend crates use video-backend-api do not own renderer/GPU import crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"render-wgpu-shell"}),
            RENDER_WGPU_SHELL_FORBIDDEN_DEPENDENCIES,
            "render-wgpu-shell regardless demux/source/audio/player/service/concrete video backend crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"render-wgpu-video"}),
            RENDER_WGPU_VIDEO_FORBIDDEN_DEPENDENCIES,
            "render-wgpu-video does not depend on shell/UI/app/player/service/concrete video backend crates",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            MEDIA_PREFETCH_CRATES,
            MEDIA_PREFETCH_ALLOWED_DEPENDENCIES,
            "media-prefetch depends only source-core plus tracing/thiserror",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"media-source-open"}),
            MEDIA_SOURCE_OPEN_FORBIDDEN_DEPENDENCIES,
            "media-source-open does not depend on UI/window/GPU/render crates or app-egui",
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
    violations.extend(find_app_egui_concrete_renderer_violations(repo_root))
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
    """Закрепляет S28G parser ownership и evidence."""

    # Минимальные unit-test repositories без Symphonia factory не собирают S28G tree.
    if not (repo_root / EXISTING_DEMUX_BOUNDARY_MARKER).is_file():
        return []

    violations = find_required_source_anchor_violations(
        repo_root,
        EXISTING_DEMUX_SOURCE_ANCHORS,
    )
    violations.extend(find_existing_demux_packet_parser_violations(repo_root))
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
                        rule="custom-drawing app-egui must pass through ui-artwork-egui",
                        matched_text=line.strip(),
                    )
                )
    return violations


# Сессия 02 плана `user/gles2-renderer/`: app-egui видит рендер только как
# `Box<dyn render_shell_api::RenderShell>`. Конкретный wgpu-рендер знает лишь каталог рендеров
# (`render_backend/`, фабрика кандидата). wgpu-типы и материализаторы остаются в тестах,
# которые рисуют напрямую через `WgpuVideoRenderer` / `SubtitleOverlayRenderer`.
APP_EGUI_CONCRETE_RENDERER_SOURCE_ROOT = Path("crates/app-egui/src")
APP_EGUI_RENDERER_CATALOG_ROOTS = (Path("crates/app-egui/src/render_backend"),)
APP_EGUI_GPU_TEST_ROOTS = (
    Path("crates/app-egui/src/state/subtitles/tests"),
    Path("crates/app-egui/src/web_media_open/content_probe_tests"),
    Path("crates/app-egui/src/media_open/web/tests"),
)
APP_EGUI_CONCRETE_RENDERER_PATTERN = re.compile(r"\b(?:render_wgpu_shell|render_gles2_shell)\b")
APP_EGUI_GPU_API_PATTERN = re.compile(r"\b(?:render_wgpu_video|wgpu)::")


def find_app_egui_concrete_renderer_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Не даёт production app-egui снова увидеть конкретный рендер или GPU-API.

    `render_wgpu_shell` и `render_gles2_shell` разрешены только каталогу рендеров и GPU-тестам; `wgpu::` и
    `render_wgpu_video::` — только GPU-тестам (в production они и не собрались бы: это
    dev-dependencies). Строки-комментарии не проверяются.
    """

    violations: list[SourcePolicyViolation] = []
    source_root = repo_root / APP_EGUI_CONCRETE_RENDERER_SOURCE_ROOT
    if not source_root.exists():
        return violations
    for source_path in sorted(source_root.rglob("*.rs")):
        relative_path = source_path.relative_to(repo_root)
        if path_is_under_any(relative_path, APP_EGUI_GPU_TEST_ROOTS):
            continue
        concrete_renderer_allowed = path_is_under_any(
            relative_path, APP_EGUI_RENDERER_CATALOG_ROOTS
        )
        for line_number, line in enumerate(read_text_lossy(source_path).splitlines(), start=1):
            stripped_line = line.strip()
            if stripped_line.startswith("//"):
                continue
            if APP_EGUI_GPU_API_PATTERN.search(line):
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_number,
                        rule="app-egui production code must not use wgpu or render-wgpu-video types",
                        matched_text=stripped_line,
                    )
                )
            elif not concrete_renderer_allowed and APP_EGUI_CONCRETE_RENDERER_PATTERN.search(line):
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_number,
                        rule="only the app-egui renderer catalog (render_backend) may name render_wgpu_shell or render_gles2_shell",
                        matched_text=stripped_line,
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
                        rule="ffmpeg_sw does not appear as a public config/UI option",
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
                        rule='video.preferred_backend = "vulkan" shall not be public config value',
                        matched_text=stripped_line,
                    )
                )

            if "VideoBackendPreference::Vulkan" in stripped_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule="VideoBackendPreference should not return Vulkan video backend variant",
                        matched_text=stripped_line,
                    )
                )

            if "settings.video.preferred_backend.vulkan" in stripped_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule="settings registry/UI should not publish Vulkan video backend option",
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
        or 'video.preferred_backend = "vulkan" has gone' in stripped_line
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

    return [
        violation
        for violation in find_regex_violations_in_roots(
            repo_root,
            SOURCE_POLICY_SCAN_ROOTS,
            RUST_SOURCE_SUFFIXES,
            CPU_RGB_CONVERSION_PATTERNS,
        )
        if not path_is_under_any(violation.path, (UPSTREAM_FFMPEG_BINDINGS_ROOT,))
    ]


def find_ffmpeg_hardware_decode_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает FFmpeg hardware decode API даже внутри `video-ffmpeg`."""

    return [
        violation
        for violation in find_regex_violations_in_roots(
            repo_root,
            SOURCE_POLICY_SCAN_ROOTS,
            RUST_SOURCE_SUFFIXES,
            FFMPEG_HARDWARE_DECODE_PATTERNS,
        )
        if not path_is_under_any(violation.path, (UPSTREAM_FFMPEG_BINDINGS_ROOT,))
    ]


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
            raise GuardrailError(f"source root `{relative_root}` is missing")
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
            raise GuardrailError(f"source path `{relative_path}` is missing")
        if path.is_file():
            if path.suffix in suffixes:
                source_files.add(relative_path)
            continue
        if path.is_dir():
            for child_path in path.rglob("*"):
                if child_path.is_file() and child_path.suffix in suffixes:
                    source_files.add(child_path.relative_to(repo_root))
            continue
        raise GuardrailError(f"source path `{relative_path}` is neither a file nor a directory")
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
        raise GuardrailError(f"`{path}` is not UTF-8 text: {error}") from error


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


def find_reusable_gles2_workspace_dependencies(
    all_dependency_map: dict[str, frozenset[str]],
) -> list[DependencyViolation]:
    """Переиспользуемые GLES2-crate-ы видят из workspace только другие `gles2-*` (любой вид зависимости)."""

    workspace_crates = frozenset(all_dependency_map)
    violations: list[DependencyViolation] = []
    for owner in sorted(REUSABLE_GLES2_CRATES.intersection(workspace_crates)):
        for dependency in sorted(all_dependency_map[owner].intersection(workspace_crates)):
            if dependency.startswith(REUSABLE_GLES2_WORKSPACE_PREFIX):
                continue
            violations.append(
                DependencyViolation(
                    owner=owner,
                    dependency=dependency,
                    rule="reusable GLES2 crates depend on no workspace crate except gles2-* (dev included)",
                )
            )
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
