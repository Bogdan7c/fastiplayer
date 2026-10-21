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
  player — fastiplayer | vlc; mode — hw | sw; codec — ключ из --media или
  встроенные h264/hevc (фикстуры make_fixtures.py в --fixtures);
  intf (только VLC) — qt (по умолчанию) | dummy.
"""

import argparse
import os
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


def isolated_env(attempt_root):
    """Свежие XDG-каталоги на попытку: без личного config/resume/плейлистов."""
    environment = dict(os.environ)
    for name in ("CONFIG", "DATA", "CACHE", "STATE"):
        directory = attempt_root / name.lower()
        directory.mkdir(parents=True, exist_ok=True)
        environment[f"XDG_{name}_HOME"] = str(directory)
    return environment


def build_command(player, mode, media, attempt_root, environment, vlc_intf, binary, fastiplayer_args):
    """Командная строка плеера; конфиг Fastiplayer копируется из шаблона t480s.

    ``fastiplayer_args`` — опции Fastiplayer перед файлом (например ``--fullscreen``,
    как в протоколе v3); у VLC не используются.
    """
    if player == "fastiplayer":
        config_dir = attempt_root / "config" / "fastiplayer"
        config_dir.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(HW_TEMPLATE if mode == "hw" else SW_TEMPLATE, config_dir / "config.toml")
        environment.setdefault("RUST_LOG", "info")
        return [str(binary), *fastiplayer_args, str(media)]
    command = ["vlc", "--ignore-config", "--no-one-instance", "--gain=0",
               "--no-video-title-show", f"--avcodec-hw={'vaapi' if mode == 'hw' else 'none'}"]
    if vlc_intf == "dummy":
        command += ["--intf", "dummy"]
    return command + [str(media)]


def run_attempt(label, player, mode, media, out_dir, warmup, duration, vlc_intf, extra_env, binary,
                fastiplayer_args):
    attempt_root = out_dir / label
    if attempt_root.exists():
        shutil.rmtree(attempt_root)
    attempt_root.mkdir(parents=True)
    preread(media)
    environment = isolated_env(attempt_root)
    environment.update(extra_env)
    command = build_command(player, mode, media, attempt_root, environment, vlc_intf, binary,
                            fastiplayer_args)
    with open(attempt_root / "player.log", "wb") as log:
        process = subprocess.Popen(command, env=environment, stdout=log, stderr=subprocess.STDOUT,
                                   cwd=str(attempt_root), start_new_session=True)
        sampler = subprocess.run(
            [sys.executable, str(TOOLS_DIR / "thread_cpu.py"), str(process.pid),
             "--warmup", str(warmup), "--duration", str(duration), "--label", label],
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
    arguments = parser.parse_args()

    media = {
        "h264": arguments.fixtures / "synthetic-h264-1080p60.mp4",
        "hevc": arguments.fixtures / "synthetic-hevc-4k60.mp4",
    }
    media.update(dict(item.split("=", 1) for item in arguments.media))
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
                    vlc_intf, extra_env, arguments.binary, tuple(arguments.fastiplayer_args))
        time.sleep(2)


if __name__ == "__main__":
    main()
