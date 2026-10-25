#!/usr/bin/env python3
"""Быстрый диагностический прогон: плеер в изолированном XDG + срез CPU по потокам.

Это НЕ протокол бенчмарка из docs/benchmarks (нет cgroup, XWayland fullscreen,
RPC-проверок качества, чередования). Цель — за минуту увидеть, как распределён
CPU по потокам и GPU-движкам у Fastiplayer и VLC на одних и тех же файлах,
до и после правки. Для публикуемых чисел используй строгий протокол.

Пример:
  python3 docs/benchmarks/tools/dev/run_pass.py \
      fp-hw-h264=fastiplayer:hw:h264 vlc-hw-h264=vlc:hw:h264 \
      --media av1=/path/to/av1-4k60.mp4 fp-sw-av1=fastiplayer:sw:av1

Сценарий: label=player:mode:codec[:intf]
  player — fastiplayer | vlc | mpv; mode — hw | sw; codec — ключ из --media или
  встроенные h264/hevc (фикстуры make_fixtures.py в --fixtures);
  intf (только VLC) — qt (по умолчанию) | dummy.

Рендер Fastiplayer задаёт --render-profile (auto | vulkan | opengles): строка
``profile`` секции [render] шаблона заменяется перед копированием. Опции mpv
(например ``--vo=gpu --gpu-api=opengl --profile=fast``) — через --mpv-arg.
--window-mode x11 убирает WAYLAND_DISPLAY (плеер уходит в XWayland), wayland —
оставляет как в сессии. --compositor добавляет в отчёт CPU kwin_wayland и Xwayland
за то же окно (решение владельца 12 плана gles2-renderer: «процесс + композитор»).
"""

import argparse
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
TOOLS_DIR = Path(__file__).resolve().parent
HW_TEMPLATE = REPO / "docs/benchmarks/tools/t480s/hardware-template.toml"
SW_TEMPLATE = REPO / "docs/benchmarks/tools/t480s/software-template.toml"


def preread(path):
    """Прогреваем page cache, чтобы окно не мерило диск."""
    with open(path, "rb") as handle:
        while handle.read(16 * 1024 * 1024):
            pass


RENDER_PROFILES = ("auto", "vulkan", "opengles")
WINDOW_MODES = ("wayland", "x11")


def isolated_env(attempt_root, window_mode):
    """Свежие XDG-каталоги на попытку: без личного config/resume/плейлистов.

    ``window_mode == "x11"`` убирает WAYLAND_DISPLAY: winit и mpv тогда идут
    через XWayland (как строгий протокол v3), иначе — нативный Wayland сессии.
    """
    environment = dict(os.environ)
    if window_mode == "x11":
        environment.pop("WAYLAND_DISPLAY", None)
        environment["XDG_SESSION_TYPE"] = "x11"
    for name in ("CONFIG", "DATA", "CACHE", "STATE"):
        directory = attempt_root / name.lower()
        directory.mkdir(parents=True, exist_ok=True)
        environment[f"XDG_{name}_HOME"] = str(directory)
    return environment


def config_with_render_profile(template_text, render_profile):
    """Подменяет ``profile = "..."`` в секции [render] шаблона.

    Ровно одна замена, иначе ошибка: молча оставить чужой рендер в замере нельзя.
    """
    replaced, count = re.subn(r'(?m)^(\[render\]\n(?:[^\[\n].*\n)*?)profile = "[a-z]+"',
                              lambda match: f'{match.group(1)}profile = "{render_profile}"',
                              template_text, count=1)
    if count != 1:
        raise SystemExit("template has no [render] profile line to override")
    return replaced


def build_command(player, mode, media, attempt_root, environment, vlc_intf, binary, fastiplayer_args,
                  render_profile, mpv_args):
    """Командная строка плеера; конфиг Fastiplayer копируется из шаблона t480s.

    ``fastiplayer_args`` — опции Fastiplayer перед файлом (например ``--fullscreen``,
    как в протоколе v3); ``render_profile`` — рендер Fastiplayer (None = как в шаблоне);
    ``mpv_args`` — опции mpv поверх ``--no-config`` и выбора декода.
    """
    if player == "fastiplayer":
        config_dir = attempt_root / "config" / "fastiplayer"
        config_dir.mkdir(parents=True, exist_ok=True)
        template_text = (HW_TEMPLATE if mode == "hw" else SW_TEMPLATE).read_text()
        if render_profile is not None:
            template_text = config_with_render_profile(template_text, render_profile)
        (config_dir / "config.toml").write_text(template_text)
        environment.setdefault("RUST_LOG", "info")
        return [str(binary), *fastiplayer_args, str(media)]
    if player == "mpv":
        # Тот же базис, что у эталона протокола v3 (collect.py::mpv_command), плюс опции вывода.
        return ["mpv", "--no-config", f"--hwdec={'vaapi' if mode == 'hw' else 'no'}", "--fs",
                "--volume=0", *mpv_args, str(media)]
    command = ["vlc", "--ignore-config", "--no-one-instance", "--gain=0",
               "--no-video-title-show", f"--avcodec-hw={'vaapi' if mode == 'hw' else 'none'}"]
    if vlc_intf == "dummy":
        command += ["--intf", "dummy"]
    return command + [str(media)]


def run_attempt(label, player, mode, media, out_dir, warmup, duration, vlc_intf, extra_env, binary,
                fastiplayer_args, render_profile, mpv_args, window_mode, compositor):
    attempt_root = out_dir / label
    if attempt_root.exists():
        shutil.rmtree(attempt_root)
    attempt_root.mkdir(parents=True)
    preread(media)
    environment = isolated_env(attempt_root, window_mode)
    environment.update(extra_env)
    command = build_command(player, mode, media, attempt_root, environment, vlc_intf, binary,
                            fastiplayer_args, render_profile, mpv_args)
    with open(attempt_root / "player.log", "wb") as log:
        process = subprocess.Popen(command, env=environment, stdout=log, stderr=subprocess.STDOUT,
                                   cwd=str(attempt_root), start_new_session=True)
        sampler = subprocess.run(
            [sys.executable, str(TOOLS_DIR / "thread_cpu.py"), str(process.pid),
             "--warmup", str(warmup), "--duration", str(duration), "--label", label,
             *(["--compositor"] if compositor else [])],
            capture_output=True, text=True)
        exit_code = process.poll()
        if exit_code is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
    report = sampler.stdout + sampler.stderr
    if exit_code is not None:
        report += f"\n!!! player exited early with code {exit_code}\n"
    (attempt_root / "report.txt").write_text(report)
    print(report, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("scenarios", nargs="+", help="label=player:mode:codec[:intf]")
    parser.add_argument("--fixtures", type=Path, default=REPO / "benchmark-fixtures",
                        help="каталог с synthetic-h264-1080p60.mp4 и synthetic-hevc-4k60.mp4")
    parser.add_argument("--media", action="append", default=[], help="key=/abs/path дополнительного файла")
    parser.add_argument("--out", type=Path, default=REPO / "benchmark-output" / "dev-pass",
                        help="куда класть логи и отчёты попыток (вне git)")
    parser.add_argument("--binary", type=Path, default=REPO / "target/release/fastiplayer")
    parser.add_argument("--warmup", type=float, default=15)
    parser.add_argument("--duration", type=float, default=20)
    parser.add_argument("--env", action="append", default=[], help="KEY=VALUE для плеера, например RUST_LOG=...")
    parser.add_argument("--fastiplayer-arg", action="append", default=[], dest="fastiplayer_args",
                        help="опция Fastiplayer перед файлом, например --fastiplayer-arg=--fullscreen")
    parser.add_argument("--render-profile", choices=RENDER_PROFILES, default=None,
                        help="рендер Fastiplayer в конфиге попытки (по умолчанию — как в шаблоне)")
    parser.add_argument("--mpv-arg", action="append", default=[], dest="mpv_args",
                        help="опция mpv, например --mpv-arg=--vo=gpu --mpv-arg=--gpu-api=opengl")
    parser.add_argument("--window-mode", choices=WINDOW_MODES, default="wayland",
                        help="wayland — нативное окно; x11 — через XWayland")
    parser.add_argument("--compositor", action="store_true",
                        help="добавить CPU kwin_wayland и Xwayland за то же окно")
    arguments = parser.parse_args()

    media = {
        "h264": arguments.fixtures / "synthetic-h264-1080p60.mp4",
        "hevc": arguments.fixtures / "synthetic-hevc-4k60.mp4",
    }
    media.update(dict(item.split("=", 1) for item in arguments.media))
    # Плеер запускается с cwd = каталог попытки, а XDG_*_HOME строятся от --out:
    # относительные пути молча ушли бы в чужой каталог (конфиг по умолчанию, «файл не найден»).
    media = {key: Path(path).resolve() for key, path in media.items()}
    arguments.out = arguments.out.resolve()
    arguments.binary = arguments.binary.resolve()
    extra_env = dict(item.split("=", 1) for item in arguments.env)
    arguments.out.mkdir(parents=True, exist_ok=True)

    for scenario in arguments.scenarios:
        label, spec = scenario.split("=", 1)
        parts = spec.split(":")
        if len(parts) < 3:
            parser.error(f"bad scenario {scenario!r}: expected label=player:mode:codec[:intf]")
        player, mode, codec = parts[:3]
        vlc_intf = parts[3] if len(parts) > 3 else "qt"
        if codec not in media:
            parser.error(f"unknown media key {codec!r}; pass --media {codec}=/path")
        path = Path(media[codec])
        if not path.is_file():
            parser.error(f"media file not found: {path}")
        run_attempt(label, player, mode, path, arguments.out, arguments.warmup, arguments.duration,
                    vlc_intf, extra_env, arguments.binary, tuple(arguments.fastiplayer_args),
                    arguments.render_profile, tuple(arguments.mpv_args), arguments.window_mode,
                    arguments.compositor)
        time.sleep(2)


if __name__ == "__main__":
    main()
