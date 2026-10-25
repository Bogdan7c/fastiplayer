#!/usr/bin/env python3
"""Regression tests для dependency policy из check-refactor-guardrails.py."""

from __future__ import annotations

import importlib.util
import io
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from types import ModuleType


GUARDRAIL_PATH = Path(__file__).resolve().parents[1] / "check-refactor-guardrails.py"
EXPECTED_NEUTRAL_TEMPO_OWNERS = frozenset({"audio-core", "player-core"})
EXPECTED_CONCRETE_TEMPO_DEPENDENCIES = frozenset(
    {
        "audio-signalsmith",
        "audio-timestretch",
        "signalsmith-stretch",
        "timestretch",
    }
)


def load_guardrail_module() -> ModuleType:
    """Загружает скрипт с дефисами в имени как обычный Python module."""

    module_spec = importlib.util.spec_from_file_location(
        "check_refactor_guardrails",
        GUARDRAIL_PATH,
    )
    if module_spec is None or module_spec.loader is None:
        raise RuntimeError(f"failed to create an import spec for `{GUARDRAIL_PATH}`")

    guardrail_module = importlib.util.module_from_spec(module_spec)
    # `dataclasses` ищет module namespace через `sys.modules` во время декорирования.
    sys.modules[module_spec.name] = guardrail_module
    module_spec.loader.exec_module(guardrail_module)
    return guardrail_module


GUARDRAIL = load_guardrail_module()


def package_with_dependencies(
    package_name: str,
    dependencies: tuple[tuple[str, str | None], ...],
) -> dict[str, object]:
    """Строит минимальный cargo-metadata package для focused policy test."""

    return {
        "name": package_name,
        "dependencies": [
            {"name": dependency_name, "kind": dependency_kind}
            for dependency_name, dependency_kind in dependencies
        ],
    }


def complete_workspace_packages() -> dict[str, dict[str, object]]:
    """Строит минимальный полный workspace graph для pass-сценариев."""

    return {
        crate_name: package_with_dependencies(crate_name, ())
        for crate_name in GUARDRAIL.REQUIRED_ROLE_CRATES
    }


class TemporaryPolicyRepository:
    """Создаёт герметичный source tree, достаточный для всех source policies."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.write("Cargo.toml", "[workspace]\nmembers = []\n[workspace.dependencies]\n")
        for relative_root in GUARDRAIL.PUBLIC_CONFIG_SCAN_ROOTS:
            (root / relative_root).mkdir(parents=True, exist_ok=True)
        for relative_path in GUARDRAIL.MAIN_VIDEO_REUSED_DECODER_SCAN_PATHS:
            self.write(relative_path, "// playback decoder reuse remains explicit\n")
        self.write("crates/app-egui/src/lib.rs", "// composition root\n")
        self.write("crates/video-ffmpeg/src/lib.rs", "pub struct AVFrame;\n")
        self.write("crates/video-vaapi/src/lib.rs", "fn open_owned_display() {}\n")

    def write(self, relative_path: str | Path, text: str) -> Path:
        """Пишет один fixture-файл и возвращает его абсолютный путь."""

        fixture_path = self.root / relative_path
        fixture_path.parent.mkdir(parents=True, exist_ok=True)
        fixture_path.write_text(text, encoding="utf-8")
        return fixture_path


class TempoDependencyGuardrailTests(unittest.TestCase):
    """Закрепляет нейтральную tempo boundary без запрета composition graph."""

    def test_neutral_crates_reject_every_concrete_tempo_dependency_kind(self) -> None:
        """Normal/dev/build edges одинаково запрещены для обоих neutral owners."""

        self.assertEqual(EXPECTED_NEUTRAL_TEMPO_OWNERS, GUARDRAIL.TEMPO_NEUTRAL_CRATES)
        self.assertEqual(
            EXPECTED_CONCRETE_TEMPO_DEPENDENCIES,
            GUARDRAIL.TEMPO_NEUTRAL_FORBIDDEN_DEPENDENCIES,
        )


class AppEguiConcreteRendererGuardrailTests(unittest.TestCase):
    """Production app-egui видит рендер только через контракт (сессия 02 gles2-renderer)."""

    def test_catalog_may_name_the_concrete_wgpu_renderer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repository = TemporaryPolicyRepository(Path(temporary_directory))
            repository.write(
                "crates/app-egui/src/render_backend/wgpu_vulkan.rs",
                "use render_wgpu_shell::Renderer;\n",
            )
            repository.write(
                "crates/app-egui/src/frame_prepare.rs",
                "// Раньше здесь был render_wgpu_shell::Renderer.\nuse render_shell_api::RenderShell;\n",
            )
            self.assertEqual(
                [],
                GUARDRAIL.find_app_egui_concrete_renderer_violations(repository.root),
            )

    def test_production_code_outside_catalog_cannot_name_concrete_renderer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repository = TemporaryPolicyRepository(Path(temporary_directory))
            repository.write(
                "crates/app-egui/src/state.rs",
                "fn draw(renderer: &mut render_wgpu_shell::Renderer) {}\n",
            )
            violations = GUARDRAIL.find_app_egui_concrete_renderer_violations(repository.root)
            self.assertEqual(1, len(violations))
            self.assertIn("render_backend", violations[0].rule)

    def test_gles2_renderer_follows_the_same_catalog_only_rule(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repository = TemporaryPolicyRepository(Path(temporary_directory))
            repository.write(
                "crates/app-egui/src/render_backend/gles2.rs",
                "use render_gles2_shell::Gles2Renderer;\n",
            )
            repository.write(
                "crates/app-egui/src/state.rs",
                "fn draw(renderer: &mut render_gles2_shell::Gles2Renderer) {}\n",
            )
            violations = GUARDRAIL.find_app_egui_concrete_renderer_violations(repository.root)
            self.assertEqual(
                [(Path("crates/app-egui/src/state.rs"), 1)],
                [(violation.path, violation.line_number) for violation in violations],
            )
            self.assertIn("render_gles2_shell", violations[0].rule)

    def test_gpu_types_are_rejected_even_in_the_catalog(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repository = TemporaryPolicyRepository(Path(temporary_directory))
            repository.write(
                "crates/app-egui/src/render_backend/mod.rs",
                "fn queue(queue: &wgpu::Queue) {}\n",
            )
            repository.write(
                "crates/app-egui/src/state/video_backend.rs",
                "use render_wgpu_video::DmaBufWgpuFrameMaterializer;\nfn x(_: render_wgpu_video::DmaBufImportStats) {}\n",
            )
            violations = GUARDRAIL.find_app_egui_concrete_renderer_violations(repository.root)
            # Каждая строка с GPU-типом — отдельное нарушение (в video_backend.rs их две).
            self.assertEqual(
                [
                    (Path("crates/app-egui/src/render_backend/mod.rs"), 1),
                    (Path("crates/app-egui/src/state/video_backend.rs"), 1),
                    (Path("crates/app-egui/src/state/video_backend.rs"), 2),
                ],
                [(violation.path, violation.line_number) for violation in violations],
            )
            self.assertTrue(all("wgpu" in violation.rule for violation in violations))

    def test_gpu_tests_may_draw_through_wgpu_directly(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repository = TemporaryPolicyRepository(Path(temporary_directory))
            repository.write(
                "crates/app-egui/src/state/subtitles/tests/gpu_chain.rs",
                "use render_wgpu_shell::SubtitleOverlayRenderer;\nconst F: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;\n",
            )
            self.assertEqual(
                [],
                GUARDRAIL.find_app_egui_concrete_renderer_violations(repository.root),
            )


class ArtworkBoundaryGuardrailTests(unittest.TestCase):
    """Закрепляет визуальную boundary между app-egui и ui-artwork-egui."""

    def test_facade_call_is_allowed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repository = TemporaryPolicyRepository(Path(temporary_directory))
            repository.write(
                "crates/app-egui/src/ui.rs",
                "ArtworkPainter::new(ui.painter()).video_dim_overlay(rect, color);\n",
            )
            self.assertEqual(
                [],
                GUARDRAIL.find_app_egui_custom_paint_violations(repository.root),
            )

    def test_direct_painter_primitive_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repository = TemporaryPolicyRepository(Path(temporary_directory))
            repository.write(
                "crates/app-egui/src/ui.rs",
                "ui.painter().rect_filled(rect, 0.0, color);\n",
            )
            violations = GUARDRAIL.find_app_egui_custom_paint_violations(repository.root)
            self.assertEqual(1, len(violations))
            self.assertIn("ui-artwork-egui", violations[0].rule)
        dependency_kinds: tuple[tuple[str, str | None], ...] = (
            ("normal", None),
            ("dev", "dev"),
            ("build", "build"),
        )

        for owner in sorted(EXPECTED_NEUTRAL_TEMPO_OWNERS):
            for dependency_name in sorted(EXPECTED_CONCRETE_TEMPO_DEPENDENCIES):
                for kind_label, dependency_kind in dependency_kinds:
                    with self.subTest(
                        owner=owner,
                        dependency=dependency_name,
                        kind=kind_label,
                    ):
                        packages = {
                            owner: package_with_dependencies(
                                owner,
                                ((dependency_name, dependency_kind),),
                            )
                        }
                        normal_dependencies = GUARDRAIL.direct_normal_dependencies(packages)
                        all_dependencies = GUARDRAIL.direct_all_manifest_dependencies(packages)
                        violations = GUARDRAIL.find_dependency_violations(
                            normal_dependencies,
                            all_dependencies,
                            frozenset(),
                        )

                        self.assertTrue(
                            any(
                                violation.owner == owner
                                and violation.dependency == dependency_name
                                for violation in violations
                            ),
                            msg=(
                                f"expected prohibition of {kind_label} dependency "
                                f"{owner} -> {dependency_name}"
                            ),
                        )

    def test_composition_root_and_concrete_adapter_edges_remain_allowed(self) -> None:
        """Policy не превращается в workspace-global запрет runtime composition."""

        dependency_map = {
            "app-egui": frozenset({"audio-signalsmith"}),
            "audio-signalsmith": frozenset({"audio-core", "signalsmith-stretch"}),
            "audio-timestretch": frozenset({"audio-core", "timestretch"}),
        }

        violations = GUARDRAIL.find_dependency_violations(
            dependency_map,
            dependency_map,
            frozenset({"audio-signalsmith"}),
        )

        self.assertEqual([], violations)


class DependencyGraphPolicyTests(unittest.TestCase):
    """Доказывает pass/fail поведение manifest/dependency-graph policies."""

    def test_smooth_manifest_core_accepts_only_hardened_xml_and_typed_errors(self) -> None:
        """D1 crate не получает скрытый URL/network/media/runtime dependency edge."""

        allowed_packages = {
            "smooth-streaming-manifest-core": package_with_dependencies(
                "smooth-streaming-manifest-core",
                (("bounded-xml-reader", None), ("thiserror", None)),
            )
        }
        allowed_violations = GUARDRAIL.find_dependency_violations(
            GUARDRAIL.direct_normal_dependencies(allowed_packages),
            GUARDRAIL.direct_all_manifest_dependencies(allowed_packages),
            frozenset({"bounded-xml-reader", "thiserror"}),
        )
        self.assertFalse(
            any(
                violation.owner == "smooth-streaming-manifest-core"
                for violation in allowed_violations
            )
        )

        forbidden_packages = {
            "smooth-streaming-manifest-core": package_with_dependencies(
                "smooth-streaming-manifest-core",
                (("url", None),),
            )
        }
        forbidden_violations = GUARDRAIL.find_dependency_violations(
            GUARDRAIL.direct_normal_dependencies(forbidden_packages),
            GUARDRAIL.direct_all_manifest_dependencies(forbidden_packages),
            frozenset({"url"}),
        )
        self.assertTrue(
            any(
                violation.owner == "smooth-streaming-manifest-core"
                and violation.dependency == "url"
                for violation in forbidden_violations
            )
        )

    def test_smooth_fmp4_accepts_only_manifest_patch_and_typed_errors(self) -> None:
        """F2 adapter не получает скрытый transport/demux/player dependency edge."""

        allowed_packages = {
            "smooth-streaming-fmp4": package_with_dependencies(
                "smooth-streaming-fmp4",
                (
                    ("smooth-streaming-manifest-core", None),
                    ("symphonia-format-isomp4", None),
                    ("thiserror", None),
                ),
            )
        }
        allowed_violations = GUARDRAIL.find_dependency_violations(
            GUARDRAIL.direct_normal_dependencies(allowed_packages),
            GUARDRAIL.direct_all_manifest_dependencies(allowed_packages),
            frozenset(
                {
                    "smooth-streaming-manifest-core",
                    "symphonia-format-isomp4",
                    "thiserror",
                }
            ),
        )
        self.assertFalse(
            any(
                violation.owner == "smooth-streaming-fmp4"
                for violation in allowed_violations
            )
        )

        forbidden_packages = {
            "smooth-streaming-fmp4": package_with_dependencies(
                "smooth-streaming-fmp4",
                (("web-media-http", None),),
            )
        }
        forbidden_violations = GUARDRAIL.find_dependency_violations(
            GUARDRAIL.direct_normal_dependencies(forbidden_packages),
            GUARDRAIL.direct_all_manifest_dependencies(forbidden_packages),
            frozenset({"web-media-http"}),
        )
        self.assertTrue(
            any(
                violation.owner == "smooth-streaming-fmp4"
                and violation.dependency == "web-media-http"
                for violation in forbidden_violations
            )
        )

    def test_hls_runtime_accepts_concrete_demux_only_as_dev_composition(self) -> None:
        """Production HLS graph neutral, а hermetic fixtures могут собрать concrete registry."""

        for dependency_name in ("mpeg-ts-demux", "symphonia-demux"):
            with self.subTest(dependency=dependency_name, kind="normal"):
                packages = {
                    "web-media-hls": package_with_dependencies(
                        "web-media-hls",
                        ((dependency_name, None),),
                    )
                }
                violations = GUARDRAIL.find_dependency_violations(
                    GUARDRAIL.direct_normal_dependencies(packages),
                    GUARDRAIL.direct_all_manifest_dependencies(packages),
                    frozenset(),
                )
                self.assertTrue(
                    any(
                        violation.owner == "web-media-hls"
                        and violation.dependency == dependency_name
                        for violation in violations
                    )
                )

            with self.subTest(dependency=dependency_name, kind="dev"):
                packages = {
                    "web-media-hls": package_with_dependencies(
                        "web-media-hls",
                        ((dependency_name, "dev"),),
                    )
                }
                violations = GUARDRAIL.find_dependency_violations(
                    GUARDRAIL.direct_normal_dependencies(packages),
                    GUARDRAIL.direct_all_manifest_dependencies(packages),
                    frozenset(),
                )
                self.assertFalse(
                    any(
                        violation.owner == "web-media-hls"
                        and violation.dependency == dependency_name
                        for violation in violations
                    )
                )

    def test_hls_and_dash_allow_only_neutral_catalog_dependency(self) -> None:
        """Provider catalog edge идёт в neutral core, но не в service/app owners."""

        for owner in ("web-media-hls", "web-media-dash"):
            with self.subTest(owner=owner, dependency="web-media-core"):
                packages = {
                    owner: package_with_dependencies(
                        owner,
                        (("web-media-core", None),),
                    )
                }
                violations = GUARDRAIL.find_dependency_violations(
                    GUARDRAIL.direct_normal_dependencies(packages),
                    GUARDRAIL.direct_all_manifest_dependencies(packages),
                    frozenset(),
                )
                self.assertFalse(
                    any(
                        violation.owner == owner
                        and violation.dependency == "web-media-core"
                        for violation in violations
                    )
                )

            with self.subTest(owner=owner, dependency="service-ytdlp"):
                packages = {
                    owner: package_with_dependencies(
                        owner,
                        (("service-ytdlp", None),),
                    )
                }
                violations = GUARDRAIL.find_dependency_violations(
                    GUARDRAIL.direct_normal_dependencies(packages),
                    GUARDRAIL.direct_all_manifest_dependencies(packages),
                    frozenset(),
                )
                self.assertTrue(
                    any(
                        violation.owner == owner
                        and violation.dependency == "service-ytdlp"
                        for violation in violations
                    )
                )

    def test_forbidden_direction_allows_forward_edge_and_rejects_reverse_edge(self) -> None:
        """Neutral backend API direction разрешена, backend -> player-core запрещена."""

        packages = complete_workspace_packages()
        packages["player-core"] = package_with_dependencies(
            "player-core", (("video-backend-api", None),)
        )
        packages["video-vaapi"] = package_with_dependencies(
            "video-vaapi", (("video-backend-api", None),)
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertEqual([], passing_result.dependency_violations)

        packages["video-vaapi"] = package_with_dependencies(
            "video-vaapi", (("player-core", None),)
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "video-vaapi"
                and violation.dependency == "player-core"
                for violation in failing_result.dependency_violations
            )
        )

    def test_playlist_core_allows_only_media_core_and_rand_dependencies(self) -> None:
        """Playlist domain получает только metadata и production RNG boundary."""

        packages = complete_workspace_packages()
        packages["playlist-core"] = package_with_dependencies(
            "playlist-core",
            (("media-core", None), ("natural-sort-key", None), ("rand", None)),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "playlist-core"
                for violation in passing_result.dependency_violations
            )
        )

        packages["playlist-core"] = package_with_dependencies(
            "playlist-core",
            (
                ("media-core", None),
                ("natural-sort-key", None),
                ("rand", None),
                ("serde", None),
            ),
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "playlist-core"
                and violation.dependency == "serde"
                for violation in failing_result.dependency_violations
            )
        )

    def test_playlist_io_allows_only_neutral_parser_dependencies(self) -> None:
        """Playlist parser не получает hidden I/O/app/service dependency."""

        packages = complete_workspace_packages()
        packages["playlist-io"] = package_with_dependencies(
            "playlist-io",
            (
                ("hls-playlist-core", None),
                ("media-core", None),
                ("playlist-core", None),
                ("url", None),
            ),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "playlist-io"
                for violation in passing_result.dependency_violations
            )
        )

        packages["playlist-io"] = package_with_dependencies(
            "playlist-io",
            (
                ("hls-playlist-core", None),
                ("media-core", None),
                ("playlist-core", None),
                ("url", None),
                ("service-ytdlp", None),
            ),
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "playlist-io"
                and violation.dependency == "service-ytdlp"
                for violation in failing_result.dependency_violations
            )
        )

    def test_web_media_core_rejects_every_normal_dependency(self) -> None:
        """Normalized web-media values остаются std-only и service-neutral."""

        packages = complete_workspace_packages()
        packages["web-media-core"] = package_with_dependencies("web-media-core", ())
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "web-media-core"
                for violation in passing_result.dependency_violations
            )
        )

        packages["web-media-core"] = package_with_dependencies(
            "web-media-core", (("service-ytdlp", None),)
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "web-media-core"
                and violation.dependency == "service-ytdlp"
                for violation in failing_result.dependency_violations
            )
        )

    def test_web_media_transport_api_keeps_neutral_boundary(self) -> None:
        """Transport API видит identities/source primitives, но не service/demux/player."""

        packages = complete_workspace_packages()
        packages["web-media-transport-api"] = package_with_dependencies(
            "web-media-transport-api",
            (
                ("source-core", None),
                ("thiserror", None),
                ("web-media-core", None),
            ),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "web-media-transport-api"
                for violation in passing_result.dependency_violations
            )
        )

        packages["web-media-transport-api"] = package_with_dependencies(
            "web-media-transport-api",
            (
                ("source-core", None),
                ("thiserror", None),
                ("web-media-core", None),
                ("demux-api", None),
                ("player-core", None),
                ("service-ytdlp", None),
            ),
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertEqual(
            {
                violation.dependency
                for violation in failing_result.dependency_violations
                if violation.owner == "web-media-transport-api"
            },
            {"demux-api", "player-core", "service-ytdlp"},
        )

    def test_web_media_http_reuses_source_and_prefetch_owners(self) -> None:
        """Concrete HTTP provider не создаёт второй client/cache/service owner."""

        packages = complete_workspace_packages()
        packages["web-media-http"] = package_with_dependencies(
            "web-media-http",
            (
                ("media-prefetch", None),
                ("source-core", None),
                ("web-media-transport-api", None),
            ),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "web-media-http"
                for violation in passing_result.dependency_violations
            )
        )

        packages["web-media-http"] = package_with_dependencies(
            "web-media-http",
            (
                ("media-prefetch", None),
                ("source-core", None),
                ("web-media-transport-api", None),
                ("reqwest", None),
                ("service-direct-media", None),
                ("demux-api", None),
            ),
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertEqual(
            {
                violation.dependency
                for violation in failing_result.dependency_violations
                if violation.owner == "web-media-http"
            },
            {"demux-api", "reqwest", "service-direct-media"},
        )

    def test_media_source_open_stays_below_ui_and_render(self) -> None:
        """Открытие источников не тянет UI/окно/GPU и не зависит обратно от app-egui."""

        packages = complete_workspace_packages()
        packages["app-egui"] = package_with_dependencies(
            "app-egui", (("media-source-open", None),)
        )
        packages["media-source-open"] = package_with_dependencies(
            "media-source-open",
            (
                ("demux-api", None),
                ("player-core", None),
                ("service-ytdlp", None),
                ("source-core", None),
                ("web-media-core", None),
            ),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "media-source-open"
                for violation in passing_result.dependency_violations
            )
        )

        packages["media-source-open"] = package_with_dependencies(
            "media-source-open",
            (
                ("source-core", None),
                ("app-egui", None),
                ("egui", None),
                ("render-wgpu-video", None),
                ("wgpu", None),
                ("winit", None),
            ),
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertEqual(
            {"app-egui", "egui", "render-wgpu-video", "wgpu", "winit"},
            {
                violation.dependency
                for violation in failing_result.dependency_violations
                if violation.owner == "media-source-open"
            },
        )

    def test_service_ytdlp_stops_before_concrete_playback_owners(self) -> None:
        """Extractor service не возвращает удалённый HTTP/WebM/player ownership."""

        packages = complete_workspace_packages()
        packages["service-ytdlp"] = package_with_dependencies(
            "service-ytdlp",
            (
                ("source-core", None),
                ("web-media-core", None),
                ("web-media-playback-plan", None),
                ("web-media-transport-api", None),
            ),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "service-ytdlp"
                for violation in passing_result.dependency_violations
            )
        )

        packages["service-ytdlp"] = package_with_dependencies(
            "service-ytdlp",
            (
                ("source-core", None),
                ("web-media-core", None),
                ("media-prefetch", None),
                ("reqwest", None),
                ("symphonia-demux", None),
                ("web-media-http", None),
            ),
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertEqual(
            {
                violation.dependency
                for violation in failing_result.dependency_violations
                if violation.owner == "service-ytdlp"
            },
            {"media-prefetch", "reqwest", "symphonia-demux", "web-media-http"},
        )

    def test_symphonia_demux_rejects_second_existing_container_parser(self) -> None:
        """Existing format rows остаются у exact Symphonia patches без fallback parser-а."""

        packages = complete_workspace_packages()
        packages["symphonia-demux"] = package_with_dependencies(
            "symphonia-demux",
            (("demux-api", None), ("media-core", None), ("symphonia", None)),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "symphonia-demux"
                for violation in passing_result.dependency_violations
            )
        )

        packages["symphonia-demux"] = package_with_dependencies(
            "symphonia-demux",
            tuple(
                (dependency_name, None)
                for dependency_name in sorted(
                    GUARDRAIL.SYMPHONIA_DEMUX_FORBIDDEN_ALTERNATIVE_PARSER_DEPENDENCIES
                )
            ),
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertEqual(
            GUARDRAIL.SYMPHONIA_DEMUX_FORBIDDEN_ALTERNATIVE_PARSER_DEPENDENCIES,
            {
                violation.dependency
                for violation in failing_result.dependency_violations
                if violation.owner == "symphonia-demux"
            },
        )

    def test_natural_sort_key_rejects_every_normal_dependency(self) -> None:
        """Общий prepared comparator остаётся строго std-only."""

        packages = complete_workspace_packages()
        packages["natural-sort-key"] = package_with_dependencies(
            "natural-sort-key", ()
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "natural-sort-key"
                for violation in passing_result.dependency_violations
            )
        )

        packages["natural-sort-key"] = package_with_dependencies(
            "natural-sort-key", (("unicode-normalization", None),)
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "natural-sort-key"
                and violation.dependency == "unicode-normalization"
                for violation in failing_result.dependency_violations
            )
        )

    def test_subtitle_core_rejects_gpu_dependency(self) -> None:
        """Нейтральная модель субтитров не может зависеть от wgpu."""

        packages = complete_workspace_packages()
        packages["subtitle-core"] = package_with_dependencies(
            "subtitle-core", (("oxilangtag", None), ("thiserror", None))
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "subtitle-core"
                for violation in passing_result.dependency_violations
            )
        )

        packages["subtitle-core"] = package_with_dependencies(
            "subtitle-core", (("oxilangtag", None), ("wgpu", None))
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "subtitle-core" and violation.dependency == "wgpu"
                for violation in failing_result.dependency_violations
            )
        )

    def test_render_shell_api_keeps_neutral_renderer_contract(self) -> None:
        """Контракт «приложение ↔ рендер» принимает только нейтральные crate-ы и egui."""

        packages = complete_workspace_packages()
        packages["render-shell-api"] = package_with_dependencies(
            "render-shell-api",
            tuple(
                (dependency, None)
                for dependency in sorted(GUARDRAIL.RENDER_SHELL_API_ALLOWED_DEPENDENCIES)
            )
            + (("anyhow", "dev"), ("codec-core", "dev")),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "render-shell-api"
                for violation in passing_result.dependency_violations
            )
        )

        for forbidden in (
            "wgpu",
            "ash",
            "glow",
            "khronos-egl",
            "winit",
            "egui-wgpu",
            "player-core",
            "video-vaapi",
            "render-wgpu-shell",
            "render-gles2-shell",
        ):
            packages["render-shell-api"] = package_with_dependencies(
                "render-shell-api", (("render-core", None), (forbidden, None))
            )
            failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
                packages, frozenset()
            )
            self.assertTrue(
                any(
                    violation.owner == "render-shell-api"
                    and violation.dependency == forbidden
                    for violation in failing_result.dependency_violations
                ),
                forbidden,
            )

    def test_gles2_egui_stays_reusable_without_player_crates(self) -> None:
        """Фундамент GLES2 берёт только провода к драйверу и не видит crate-ов плеера."""

        packages = complete_workspace_packages()
        packages["gles2-egui"] = package_with_dependencies(
            "gles2-egui",
            tuple(
                (dependency, None)
                for dependency in sorted(GUARDRAIL.GLES2_EGUI_ALLOWED_DEPENDENCIES)
            )
            + (("winit", "dev"),),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "gles2-egui"
                for violation in passing_result.dependency_violations
            )
        )

        # Normal-зависимость вне allowlist-а и crate-ы плеера — нарушение.
        # `egui` целиком (виджеты) тоже: художнику нужны только типы `epaint`.
        for forbidden in (
            "wgpu",
            "egui",
            "egui-wgpu",
            "render-shell-api",
            "player-core",
            "video-core",
        ):
            packages["gles2-egui"] = package_with_dependencies(
                "gles2-egui", (("glow", None), (forbidden, None))
            )
            failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
                packages, frozenset()
            )
            self.assertTrue(
                any(
                    violation.owner == "gles2-egui" and violation.dependency == forbidden
                    for violation in failing_result.dependency_violations
                ),
                forbidden,
            )

        # Даже dev-зависимость на crate плеера запрещена: тесты не тащат плеер в чужое приложение.
        packages["gles2-egui"] = package_with_dependencies(
            "gles2-egui", (("glow", None), ("render-core", "dev"))
        )
        dev_result = GUARDRAIL.evaluate_dependency_graph_policies(packages, frozenset())
        self.assertTrue(
            any(
                violation.owner == "gles2-egui" and violation.dependency == "render-core"
                for violation in dev_result.dependency_violations
            )
        )

        # Свои будущие gles2-* crate-ы разрешены правилом workspace (не allowlist-ом normal deps).
        packages["gles2-video"] = package_with_dependencies("gles2-video", ())
        packages["gles2-egui"] = package_with_dependencies(
            "gles2-egui", (("glow", None), ("gles2-video", "dev"))
        )
        sibling_result = GUARDRAIL.evaluate_dependency_graph_policies(packages, frozenset())
        self.assertFalse(
            any(
                violation.owner == "gles2-egui"
                for violation in sibling_result.dependency_violations
            )
        )

    def test_gles2_video_stays_reusable_on_top_of_gles2_egui_only(self) -> None:
        """Видеослой GLES2 видит из workspace только `gles2-egui`, словарь кадра — свой."""

        packages = complete_workspace_packages()
        packages["gles2-video"] = package_with_dependencies(
            "gles2-video",
            tuple(
                (dependency, None)
                for dependency in sorted(GUARDRAIL.GLES2_VIDEO_ALLOWED_DEPENDENCIES)
            )
            + (("egui", "dev"),),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "gles2-video"
                for violation in passing_result.dependency_violations
            )
        )

        for forbidden in ("video-core", "render-core", "wgpu", "khronos-egl", "player-core"):
            packages["gles2-video"] = package_with_dependencies(
                "gles2-video", (("gles2-egui", None), (forbidden, None))
            )
            failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
                packages, frozenset()
            )
            self.assertTrue(
                any(
                    violation.owner == "gles2-video" and violation.dependency == forbidden
                    for violation in failing_result.dependency_violations
                ),
                forbidden,
            )

        # Тесты видеослоя тоже не тащат плеер: dev-зависимость на crate плеера — нарушение.
        packages["gles2-video"] = package_with_dependencies(
            "gles2-video", (("gles2-egui", None), ("video-core", "dev"))
        )
        dev_result = GUARDRAIL.evaluate_dependency_graph_policies(packages, frozenset())
        self.assertTrue(
            any(
                violation.owner == "gles2-video" and violation.dependency == "video-core"
                for violation in dev_result.dependency_violations
            )
        )

    def test_render_gles2_shell_stays_on_its_side_of_the_render_seam(self) -> None:
        """Адаптер GLES2 берёт нейтральные контракты и GLES2-crate-ы, но не wgpu, плеер и декодеры."""

        packages = complete_workspace_packages()
        packages["render-gles2-shell"] = package_with_dependencies(
            "render-gles2-shell",
            (
                ("gles2-egui", None),
                ("gles2-video", None),
                ("render-core", None),
                ("video-core", None),
                ("video-ffmpeg", "dev"),
            ),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "render-gles2-shell"
                for violation in passing_result.dependency_violations
            )
        )
        for forbidden in sorted(GUARDRAIL.RENDER_GLES2_SHELL_FORBIDDEN_DEPENDENCIES):
            packages["render-gles2-shell"] = package_with_dependencies(
                "render-gles2-shell", (("gles2-video", None), (forbidden, None))
            )
            failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
                packages, frozenset()
            )
            self.assertTrue(
                any(
                    violation.owner == "render-gles2-shell"
                    and violation.dependency == forbidden
                    for violation in failing_result.dependency_violations
                ),
                forbidden,
            )

    def test_neutral_and_wgpu_crates_reject_future_gles2_renderer(self) -> None:
        """Нейтральные контракты, плеер, декодеры и wgpu-рендер не зависят от GLES2-crate-ов."""

        for owner in (
            "video-core",
            "render-core",
            "player-core",
            "video-vaapi",
            "render-wgpu-shell",
            "render-wgpu-video",
        ):
            for forbidden in ("render-gles2-shell", "gles2-egui", "gles2-video"):
                packages = complete_workspace_packages()
                packages[owner] = package_with_dependencies(owner, ((forbidden, None),))
                result = GUARDRAIL.evaluate_dependency_graph_policies(
                    packages, frozenset()
                )
                self.assertTrue(
                    any(
                        violation.owner == owner and violation.dependency == forbidden
                        for violation in result.dependency_violations
                    ),
                    f"{owner} -> {forbidden}",
                )

    def test_app_egui_keeps_gpu_apis_out_of_normal_dependencies(self) -> None:
        """app-egui держит `Box<dyn RenderShell>`: GPU-API только в dev-dependencies тестов."""

        packages = complete_workspace_packages()
        packages["app-egui"] = package_with_dependencies(
            "app-egui",
            (
                ("render-shell-api", None),
                ("render-wgpu-shell", None),
                ("wgpu", "dev"),
                ("render-wgpu-video", "dev"),
            ),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(packages, frozenset())
        self.assertFalse(
            any(
                violation.owner == "app-egui"
                and violation.dependency in GUARDRAIL.APP_EGUI_FORBIDDEN_RENDERER_DEPENDENCIES
                for violation in passing_result.dependency_violations
            )
        )

        for forbidden in ("wgpu", "render-wgpu-video", "ash", "glow", "khronos-egl"):
            packages["app-egui"] = package_with_dependencies(
                "app-egui", (("render-shell-api", None), (forbidden, None))
            )
            failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
                packages, frozenset()
            )
            self.assertTrue(
                any(
                    violation.owner == "app-egui" and violation.dependency == forbidden
                    for violation in failing_result.dependency_violations
                ),
                forbidden,
            )

    def test_player_and_backends_reject_render_shell_contract(self) -> None:
        """Плеер и декодеры не знают о рендере: контракт рендера им запрещён."""

        for owner in ("player-core", "video-vaapi", "video-ffmpeg", "video-core"):
            packages = complete_workspace_packages()
            packages[owner] = package_with_dependencies(
                owner, (("render-shell-api", None),)
            )
            result = GUARDRAIL.evaluate_dependency_graph_policies(packages, frozenset())
            self.assertTrue(
                any(
                    violation.owner == owner
                    and violation.dependency == "render-shell-api"
                    for violation in result.dependency_violations
                ),
                owner,
            )

    def test_subtitle_sidecar_rejects_gpu_dependency(self) -> None:
        """Поиск файлов субтитров не может зависеть от wgpu, но может от модели и thiserror."""

        packages = complete_workspace_packages()
        packages["subtitle-sidecar"] = package_with_dependencies(
            "subtitle-sidecar", (("subtitle-core", None), ("thiserror", None))
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "subtitle-sidecar"
                for violation in passing_result.dependency_violations
            )
        )

        packages["subtitle-sidecar"] = package_with_dependencies(
            "subtitle-sidecar", (("subtitle-core", None), ("wgpu", None))
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "subtitle-sidecar"
                and violation.dependency == "wgpu"
                for violation in failing_result.dependency_violations
            )
        )

    def test_subtitle_raster_rejects_gpu_dependency(self) -> None:
        """Растеризатор субтитров не может зависеть от wgpu, но может от шрифтовых библиотек."""

        packages = complete_workspace_packages()
        packages["subtitle-raster"] = package_with_dependencies(
            "subtitle-raster",
            (("subtitle-core", None), ("skrifa", None), ("fontdb", None)),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "subtitle-raster"
                for violation in passing_result.dependency_violations
            )
        )

        packages["subtitle-raster"] = package_with_dependencies(
            "subtitle-raster", (("subtitle-core", None), ("wgpu", None))
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "subtitle-raster" and violation.dependency == "wgpu"
                for violation in failing_result.dependency_violations
            )
        )

    def test_subtitle_pgs_rejects_gpu_dependency(self) -> None:
        """Декодер PGS не может зависеть от wgpu, но может от модели и thiserror."""

        packages = complete_workspace_packages()
        packages["subtitle-pgs"] = package_with_dependencies(
            "subtitle-pgs", (("subtitle-core", None), ("thiserror", None))
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "subtitle-pgs"
                for violation in passing_result.dependency_violations
            )
        )

        packages["subtitle-pgs"] = package_with_dependencies(
            "subtitle-pgs", (("subtitle-core", None), ("wgpu", None))
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "subtitle-pgs" and violation.dependency == "wgpu"
                for violation in failing_result.dependency_violations
            )
        )

    def test_playlist_discovery_rejects_player_and_ui_dependencies(self) -> None:
        """Local probe owner видит только neutral metadata/source/demux boundaries."""

        packages = complete_workspace_packages()
        packages["playlist-discovery"] = package_with_dependencies(
            "playlist-discovery",
            (
                ("media-core", None),
                ("natural-sort-key", None),
                ("source-core", None),
                ("symphonia-demux", None),
                ("thiserror", None),
            ),
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertFalse(
            any(
                violation.owner == "playlist-discovery"
                for violation in passing_result.dependency_violations
            )
        )

        packages["playlist-discovery"] = package_with_dependencies(
            "playlist-discovery",
            (("media-core", None), ("source-core", None), ("player-core", None)),
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertTrue(
            any(
                violation.owner == "playlist-discovery"
                and violation.dependency == "player-core"
                for violation in failing_result.dependency_violations
            )
        )

    def test_required_role_crates_report_only_missing_role(self) -> None:
        """Полный fixture проходит, а удалённая роль называется в результате."""

        packages = complete_workspace_packages()
        self.assertEqual([], GUARDRAIL.find_missing_role_crates(packages))
        del packages["render-core"]
        self.assertEqual(["render-core"], GUARDRAIL.find_missing_role_crates(packages))

    def test_removed_workspace_crate_is_rejected_when_reintroduced(self) -> None:
        """Обычный workspace проходит, но video-vulkan снова вводить нельзя."""

        packages = complete_workspace_packages()
        self.assertEqual([], GUARDRAIL.find_reintroduced_workspace_crates(packages))
        packages["video-vulkan"] = package_with_dependencies("video-vulkan", ())
        self.assertEqual(
            ["video-vulkan"], GUARDRAIL.find_reintroduced_workspace_crates(packages)
        )

    def test_ffmpeg_isolation_allows_owner_and_rejects_other_crate_and_workspace(self) -> None:
        """FFmpeg dependency допустима только внутри video-ffmpeg manifest."""

        packages = complete_workspace_packages()
        packages["video-ffmpeg"] = package_with_dependencies(
            "video-ffmpeg", (("ffmpeg-sys-next", None),)
        )
        passing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset()
        )
        self.assertEqual([], passing_result.dependency_violations)

        packages["app-egui"] = package_with_dependencies(
            "app-egui", (("ffmpeg-sys-next", "build"),)
        )
        failing_result = GUARDRAIL.evaluate_dependency_graph_policies(
            packages, frozenset({"ffmpeg-next"})
        )
        observed_edges = {
            (violation.owner, violation.dependency)
            for violation in failing_result.dependency_violations
        }
        self.assertIn(("app-egui", "ffmpeg-sys-next"), observed_edges)
        self.assertIn(("workspace.dependencies", "ffmpeg-next"), observed_edges)

    def test_workspace_dependency_manifest_fixture_is_parsed(self) -> None:
        """Temporary Cargo.toml проверяет реальный TOML seam, а не только set fixture."""

        with tempfile.TemporaryDirectory() as temporary_directory:
            repository = TemporaryPolicyRepository(Path(temporary_directory))
            repository.write(
                "Cargo.toml",
                "[workspace]\nmembers = []\n[workspace.dependencies]\nffmpeg-next = \"7\"\n",
            )
            self.assertEqual(
                frozenset({"ffmpeg-next"}),
                GUARDRAIL.workspace_dependency_names(repository.root),
            )


class SourceTextPolicyTests(unittest.TestCase):
    """Запускает source policies на временных repositories без production tree."""

    def setUp(self) -> None:
        """Создаёт новый независимый repository fixture для каждого теста."""

        self.temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary_directory.cleanup)
        self.repository = TemporaryPolicyRepository(Path(self.temporary_directory.name))

    def assert_single_policy_failure(
        self,
        relative_path: str | Path,
        source_text: str,
        expected_rule_fragment: str,
    ) -> None:
        """Проверяет pass fixture, затем одно actionable нарушение в заданном файле."""

        self.assertEqual([], GUARDRAIL.find_source_policy_violations(self.repository.root))
        self.repository.write(relative_path, source_text)
        violations = GUARDRAIL.find_source_policy_violations(self.repository.root)
        matching_violations = [
            violation
            for violation in violations
            if violation.path == Path(relative_path)
            and expected_rule_fragment in violation.rule
        ]
        self.assertTrue(matching_violations, msg=f"violations: {violations!r}")
        self.assertGreater(matching_violations[0].line_number, 0)

    def test_public_backend_options_pass_and_fail(self) -> None:
        """Public auto/hardware/software проходит, отдельный ffmpeg_sw падает."""

        self.repository.write(
            "crates/config/src/options.toml",
            'preferred_backend = "software"\n',
        )
        self.assert_single_policy_failure(
            "crates/config/src/options.toml",
            'preferred_backend = "ffmpeg_sw"\n',
            "public config/UI option",
        )

    def test_cpu_rgb_conversion_pass_and_fail(self) -> None:
        """GPU conversion wording проходит, прямой swscale call падает."""

        self.assert_single_policy_failure(
            "crates/render-wgpu-video/src/upload.rs",
            "let converted = sws_scale(context);\n",
            "swscale CPU conversion",
        )

    def test_direct_va_display_pass_and_fail(self) -> None:
        """VA owner может открыть display, внешний crate — нет."""

        owner_violations = GUARDRAIL.find_direct_vaapi_display_violations(
            self.repository.root
        )
        self.assertEqual([], owner_violations)
        self.assert_single_policy_failure(
            "crates/app-egui/src/va_probe.rs",
            "let display: VADisplay = vaGetDisplay(native);\n",
            "video-vaapi boundary",
        )

    def test_second_main_video_session_pass_and_fail(self) -> None:
        """Decoder reuse проходит, создание второго PlayerSession падает."""

        self.assert_single_policy_failure(
            "crates/player-core/src/session/scrub_driver.rs",
            "let preview = PlayerSession::new(factory);\n",
            "reuse playback session/decoder",
        )

    def test_required_source_anchors_pass_and_fail(self) -> None:
        """Инъекция anchors проверяет и наличие, и точную диагностику отсутствия."""

        anchor_path = Path("crates/player-core/src/session/anchor.rs")
        rule = "player-core must preserve the required decoder reuse anchor"
        anchors = ((anchor_path, ("reuse_main_decoder",), rule),)
        self.repository.write(anchor_path, "fn reuse_main_decoder() {}\n")
        self.assertEqual(
            [],
            GUARDRAIL.find_source_policy_violations(
                self.repository.root,
                required_source_anchors=anchors,
            ),
        )

        self.repository.write(anchor_path, "fn unrelated() {}\n")
        violations = GUARDRAIL.find_source_policy_violations(
            self.repository.root,
            required_source_anchors=anchors,
        )
        self.assertEqual(1, len(violations))
        self.assertEqual(anchor_path, violations[0].path)
        self.assertEqual(rule, violations[0].rule)
        self.assertIn("reuse_main_decoder", violations[0].matched_text)

    def test_existing_demux_parser_policy_ignores_words_but_rejects_parser_code(
        self,
    ) -> None:
        """S28G проверяет production declarations, а не комментарии или test corpus."""

        scanner_path = "crates/symphonia-demux/src/matroska_metadata.rs"
        self.repository.write(
            scanner_path,
            "// Cluster, Block и lacing описывают запрещённую границу.\n"
            "fn parse_tracks() {}\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    const ID_SIMPLE_BLOCK: u64 = 0xA3;\n"
            "    fn parse_lacing_fixture() {}\n"
            "}\n",
        )
        self.assertEqual(
            [],
            GUARDRAIL.find_existing_demux_packet_parser_violations(
                self.repository.root
            ),
        )

        self.repository.write(
            scanner_path,
            "const ID_SIMPLE_BLOCK: u64 = 0xA3;\n"
            "fn parse_matroska_block_payload() {}\n",
        )
        violations = GUARDRAIL.find_existing_demux_packet_parser_violations(
            self.repository.root
        )
        self.assertEqual(2, len(violations))
        self.assertTrue(all(violation.line_number > 0 for violation in violations))
        self.assertTrue(
            all("symphonia-format-mkv patch" in violation.rule for violation in violations)
        )

    def test_existing_demux_boundary_requires_fixture_and_document_anchors(
        self,
    ) -> None:
        """S28G aggregate guardrail связывает descriptor, tests и human artifact."""

        for relative_path, anchors, _ in GUARDRAIL.EXISTING_DEMUX_SOURCE_ANCHORS:
            self.repository.write(relative_path, "\n".join(anchors) + "\n")
        self.assertEqual(
            [],
            GUARDRAIL.find_existing_demux_boundary_violations(self.repository.root),
        )

        factory_path = Path("crates/symphonia-demux/src/factory.rs")
        self.repository.write(factory_path, "symphonia/generated-webm-s28b\n")
        violations = GUARDRAIL.find_existing_demux_boundary_violations(
            self.repository.root
        )
        self.assertTrue(
            any(
                violation.path == factory_path
                and "fixture inventory" in violation.rule
                for violation in violations
            )
        )

    def test_playlist_topology_boundaries_pass_and_reject_flattening_or_secret_access(
        self,
    ) -> None:
        """S18 принимает canonical consumers и ловит оба запрещённых shortcut-а."""

        # Presence marker включает S18 policy только для полного playlist tree.
        self.repository.write(
            "crates/playlist-core/src/queue/read.rs",
            "pub fn iter_top_level_entries() {}\n",
        )
        # Каждый structural consumer получает свой intent-named canonical anchor.
        for relative_path, anchors, _ in GUARDRAIL.PLAYLIST_TOPOLOGY_SOURCE_ANCHORS:
            self.repository.write(relative_path, "\n".join(anchors) + "\n")
        # Directory roots secret-аудита получают безопасные presentation fixtures.
        self.repository.write(
            "crates/app-egui/src/ui/playlist/mod.rs",
            "fn render_redacted_playlist() {}\n",
        )
        self.repository.write(
            "crates/desktop-integration/src/lib.rs",
            "pub struct RedactedDesktopSnapshot;\n",
        )
        self.assertEqual(
            [],
            GUARDRAIL.find_playlist_topology_boundary_violations(self.repository.root),
        )

        # Derived traversal не может заменить top-level order в persistence owner-е.
        state_path = "crates/playlist-state/src/dto/v2.rs"
        self.repository.write(
            state_path,
            "iter_top_level_entries()\nqueue.iter_playable_items();\n",
        )
        flattening_violations = GUARDRAIL.find_playlist_topology_boundary_violations(
            self.repository.root
        )
        self.assertTrue(
            any(
                violation.path == Path(state_path)
                and "flatten canonical" in violation.rule
                for violation in flattening_violations
            )
        )

        # External read model не получает raw persistence/open identity даже для metadata.
        external_path = "crates/app-egui/src/playlist_runtime/external_projection.rs"
        self.repository.write(
            external_path,
            "top_level_entry(entry_id)\nlocator.expose_secret_for_persistence();\n",
        )
        secret_violations = GUARDRAIL.find_playlist_topology_boundary_violations(
            self.repository.root
        )
        self.assertTrue(
            any(
                violation.path == Path(external_path)
                and "secret-bearing identity" in violation.rule
                for violation in secret_violations
            )
        )

    def test_progressive_web_boundaries_reject_legacy_openers_and_transient_persistence(
        self,
    ) -> None:
        """S27 удерживает single-open path и secret separation в полном app tree."""

        # Presence marker включает S27 policy только для repository с web composition.
        self.repository.write(
            "crates/app-egui/src/web_media_open.rs",
            "pub fn prepare_yt_dlp_web_media() {}\n",
        )
        # Каждый production consumer и manual runner получает свой exact anchor.
        for relative_path, anchors, _ in GUARDRAIL.PROGRESSIVE_WEB_SOURCE_ANCHORS:
            self.repository.write(relative_path, "\n".join(anchors) + "\n")
        # Остальные точечные runtime paths создаются безопасными до fail-инъекций.
        for relative_path in GUARDRAIL.PROGRESSIVE_WEB_LEGACY_SCAN_PATHS:
            absolute_path = self.repository.root / relative_path
            if absolute_path.exists():
                continue
            # Source roots без расширения — каталоги: на них же опираются
            # durable/presentation scan paths ниже (например, media-source-open).
            if not relative_path.suffix:
                absolute_path.mkdir(parents=True, exist_ok=True)
                continue
            self.repository.write(relative_path, "# safe progressive runtime path\n")
        # Directory roots durable/presentation scan-а должны существовать даже без fixtures.
        for relative_path in GUARDRAIL.PROGRESSIVE_WEB_TRANSIENT_SECRET_SCAN_PATHS:
            absolute_path = self.repository.root / relative_path
            if relative_path.suffix:
                if not absolute_path.exists():
                    self.repository.write(relative_path, "// safe projection\n")
                continue
            absolute_path.mkdir(parents=True, exist_ok=True)

        self.assertEqual(
            [],
            GUARDRAIL.find_progressive_web_boundary_violations(self.repository.root),
        )

        # Старый WebM-only manual scenario снова сделал бы удалённый path видимым.
        legacy_path = Path("scripts/media-regression.sh")
        self.repository.write(
            legacy_path,
            "selected_webm_opens_over_yt_dlp_range_sources\n",
        )
        legacy_violations = GUARDRAIL.find_progressive_web_boundary_violations(
            self.repository.root
        )
        self.assertTrue(
            any(
                violation.path == legacy_path
                and "legacy service-owned WebM opener" in violation.rule
                for violation in legacy_violations
            )
        )

        # Перенесённая в media-source-open web-open composition тоже сканируется.
        self.repository.write(legacy_path, "# safe progressive runtime path\n")
        moved_composition_path = Path(
            "crates/media-source-open/src/web_media_open/legacy_reintroduced.rs"
        )
        self.repository.write(
            moved_composition_path,
            "fn reopen() { open_streaming_media_from(); }\n",
        )
        moved_violations = GUARDRAIL.find_progressive_web_boundary_violations(
            self.repository.root
        )
        self.assertTrue(
            any(
                violation.path == moved_composition_path
                and "legacy service-owned WebM opener" in violation.rule
                for violation in moved_violations
            )
        )
        self.repository.write(moved_composition_path, "// safe composition\n")

        # После удаления legacy fixture durable config не может принять auth transport type.
        durable_path = Path("crates/config/src/transient_auth.rs")
        self.repository.write(
            durable_path,
            "pub struct PersistedAuth(SecretRequestContext);\n",
        )
        transient_violations = GUARDRAIL.find_progressive_web_boundary_violations(
            self.repository.root
        )
        self.assertTrue(
            any(
                violation.path == durable_path
                and "transient transport secrets" in violation.rule
                for violation in transient_violations
            )
        )

    def test_only_concrete_audio_adapter_may_depend_on_ac3_decoder(self) -> None:
        """Декодер AC-3 остаётся деталью `audio`: остальные видят только audio-core."""

        packages = {
            "audio": package_with_dependencies("audio", (("ac3-decoder", None),)),
            "player-core": package_with_dependencies(
                "player-core", (("ac3-decoder", None),)
            ),
            "ac3-decoder": package_with_dependencies("ac3-decoder", ()),
        }
        violations = GUARDRAIL.find_dependency_violations(
            GUARDRAIL.direct_normal_dependencies(packages),
            GUARDRAIL.direct_all_manifest_dependencies(packages),
            frozenset({"ac3-decoder"}),
        )
        decoder_edges = {
            violation.owner
            for violation in violations
            if violation.dependency == "ac3-decoder"
        }
        # Ровно один запрещённый edge: player-core; audio разрешён.
        self.assertEqual(decoder_edges, {"player-core"})

    def test_failure_output_contains_file_and_rule(self) -> None:
        """CLI diagnostic остаётся actionable: path и policy rule печатаются вместе."""

        violation = GUARDRAIL.SourcePolicyViolation(
            path=Path("crates/app-egui/src/va_probe.rs"),
            line_number=7,
            rule="VA display belongs to video-vaapi",
            matched_text="vaGetDisplay(native)",
        )
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            GUARDRAIL.print_failures([], [], [], [violation])
        diagnostic = stderr.getvalue()
        self.assertIn("crates/app-egui/src/va_probe.rs:7", diagnostic)
        self.assertIn("VA display belongs to video-vaapi", diagnostic)


if __name__ == "__main__":
    unittest.main()
