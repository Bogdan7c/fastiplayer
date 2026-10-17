#!/usr/bin/env python3
"""Генератор эталонов libass для crates/subtitle-raster.

Компилирует render_reference.c во временный каталог (не в репозиторий), прогоняет все
кейсы crates/subtitle-raster/tests/fixtures/libass/cases/*.ass и пишет рядом
expected/<case>.png (композит RGBA) и expected/<case>.json (список картинок).
Только стандартная библиотека Python 3; PNG пишется вручную через zlib + struct.

Параметры кадра берутся из комментария-заголовка кейса:
    ; reference: width=640 height=360 time_ms=1000
Необязательно можно задать размер хранения видео (ass_set_storage_size; по умолчанию он равен
размеру кадра):
    ; reference: width=1280 height=720 time_ms=1000 storage_width=640 storage_height=360

Один кейс может рендериться в нескольких моментах (анимации): `time_ms=0,1000,2500` (через запятую,
без пробелов). Тогда на каждый момент пишется своя пара файлов
`expected/<case>@t<время в мс, 5 цифр>.png/.json` (например, `ass_move@t01000.png`). Кейс с ОДНИМ
моментом пишет `expected/<case>.png/.json` без суффикса — как раньше.
"""

import json
import pathlib
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib

TOOL_DIR = pathlib.Path(__file__).resolve().parent
REPO_ROOT = TOOL_DIR.parent.parent
RASTER_CRATE = REPO_ROOT / "crates" / "subtitle-raster"
FONTS_DIR = RASTER_CRATE / "fonts"
FIXTURES_DIR = RASTER_CRATE / "tests" / "fixtures" / "libass"
CASES_DIR = FIXTURES_DIR / "cases"
EXPECTED_DIR = FIXTURES_DIR / "expected"

HEADER_PATTERN = re.compile(
    r"^;\s*reference:\s*width=(\d+)\s+height=(\d+)\s+time_ms=(\d+(?:,\d+)*)"
    r"(?:\s+storage_width=(\d+)\s+storage_height=(\d+))?\s*$",
    re.MULTILINE,
)
IMAGE_HEADER = struct.Struct("<iiiiiiI")


def run_checked(command, **kwargs):
    """Запускает команду и при ошибке завершает скрипт с понятным сообщением."""
    try:
        return subprocess.run(command, check=True, capture_output=True, **kwargs)
    except (subprocess.CalledProcessError, FileNotFoundError) as error:
        stderr = getattr(error, "stderr", b"") or b""
        sys.exit(f"command failed: {' '.join(map(str, command))}\n{stderr.decode(errors='replace')}")


def compile_tool(build_dir):
    """Собирает render_reference из C-исходника системным libass; бинарь живёт во временном каталоге."""
    flags = run_checked(["pkg-config", "--cflags", "--libs", "libass"]).stdout.decode().split()
    binary = build_dir / "render_reference"
    run_checked(["cc", "-O2", "-o", str(binary), str(TOOL_DIR / "render_reference.c"), *flags])
    return binary


def parse_case_header(case_path):
    """Читает width/height/time_ms и необязательный размер хранения из заголовка кейса.

    Возвращает (width, height, times_ms, storage): times_ms — список моментов (один или
    несколько, в порядке записи), storage — (w, h) или None, если размер хранения в заголовке не
    задан (тогда он равен размеру кадра).
    """
    match = HEADER_PATTERN.search(case_path.read_text(encoding="utf-8"))
    if match is None:
        sys.exit(f"{case_path}: нет заголовка '; reference: width=.. height=.. time_ms=..'")
    width, height = int(match.group(1)), int(match.group(2))
    times_ms = [int(value) for value in match.group(3).split(",")]
    if len(set(times_ms)) != len(times_ms):
        sys.exit(f"{case_path}: моменты time_ms повторяются: {times_ms}")
    storage = None
    if match.group(4) is not None:
        storage = (int(match.group(4)), int(match.group(5)))
    return width, height, times_ms, storage


def reference_stem(case_stem, time_ms, times_count):
    """Имя файлов эталона: без суффикса для одного момента, `<case>@t<мс:05>` для нескольких."""
    if times_count == 1:
        return case_stem
    return f"{case_stem}@t{time_ms:05d}"


def parse_images(blob):
    """Разбирает бинарный вывод render_reference в (версия, список картинок)."""
    version, count = struct.unpack_from("<Ii", blob, 0)
    offset = 8
    images = []
    for _ in range(count):
        kind, dst_x, dst_y, width, height, stride, color = IMAGE_HEADER.unpack_from(blob, offset)
        offset += IMAGE_HEADER.size
        mask = blob[offset : offset + width * height]
        offset += width * height
        # libass хранит цвет как 0xRRGGBBAA, где AA = 255 - прозрачность (инвертированная альфа).
        red, green, blue = (color >> 24) & 255, (color >> 16) & 255, (color >> 8) & 255
        alpha = 255 - (color & 255)
        images.append(
            {
                "type": kind,
                "dst_x": dst_x,
                "dst_y": dst_y,
                "w": width,
                "h": height,
                "stride": stride,
                "color": [red, green, blue, alpha],
                "mask": mask,
            }
        )
    if offset != len(blob):
        sys.exit("render_reference: лишние байты в выводе")
    return version, images


def div255(value):
    """Деление на 255 с округлением к ближайшему (как overlay.rs)."""
    return (value + 127) // 255


def composite(width, height, images):
    """Оператор over в порядке списка; та же формула, что SubtitleOverlay::composite_rgba_over_transparent."""
    canvas = bytearray(width * height * 4)
    for image in images:
        red, green, blue, color_alpha = image["color"]
        for row in range(image["h"]):
            y = image["dst_y"] + row
            if not 0 <= y < height:
                continue
            for column in range(image["w"]):
                x = image["dst_x"] + column
                if not 0 <= x < width:
                    continue
                source_alpha = div255(image["mask"][row * image["w"] + column] * color_alpha)
                if source_alpha == 0:
                    continue
                at = (y * width + x) * 4
                dest_r, dest_g, dest_b, dest_a = canvas[at : at + 4]
                kept = 255 - source_alpha
                out_alpha = source_alpha + div255(dest_a * kept)
                denominator = out_alpha * 255
                for channel, (source, dest) in enumerate(
                    ((red, dest_r), (green, dest_g), (blue, dest_b))
                ):
                    numerator = source * source_alpha * 255 + dest * dest_a * kept
                    canvas[at + channel] = min(255, (numerator + denominator // 2) // denominator)
                canvas[at + 3] = min(255, out_alpha)
    return bytes(canvas)


def write_png(path, width, height, rgba):
    """Минимальный PNG: RGBA 8 бит, фильтр 0 на каждой строке."""

    def chunk(tag, payload):
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    rows = b"".join(b"\x00" + rgba[y * width * 4 : (y + 1) * width * 4] for y in range(height))
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(rows, 9))
    png += chunk(b"IEND", b"")
    path.write_bytes(png)


def format_version(version):
    """ass_library_version() -> 'x.y.z' (формат 0xMMmmpppp в старших разрядах)."""
    return f"{(version >> 28) & 0xF:x}.{(version >> 20) & 0xFF:x}.{(version >> 12) & 0xFF:x}"


def main():
    cases = sorted(CASES_DIR.glob("*.ass"))
    if not cases:
        sys.exit(f"нет кейсов в {CASES_DIR}")
    EXPECTED_DIR.mkdir(parents=True, exist_ok=True)
    versions = set()
    build_dir = pathlib.Path(tempfile.mkdtemp(prefix="subtitle-reference-"))
    try:
        binary = compile_tool(build_dir)
        for case in cases:
            width, height, times_ms, storage = parse_case_header(case)
            for time_ms in times_ms:
                stem = reference_stem(case.stem, time_ms, len(times_ms))
                command = [binary, str(case), str(FONTS_DIR), str(width), str(height), str(time_ms)]
                if storage is not None:
                    command += [str(storage[0]), str(storage[1])]
                blob = run_checked(command).stdout
                version, images = parse_images(blob)
                versions.add(format_version(version))
                write_png(EXPECTED_DIR / f"{stem}.png", width, height, composite(width, height, images))
                description = {
                    "libass_version": format_version(version),
                    "width": width,
                    "height": height,
                    "time_ms": time_ms,
                    **(
                        {"storage_width": storage[0], "storage_height": storage[1]}
                        if storage is not None
                        else {}
                    ),
                    "images": [
                        {key: image[key] for key in ("type", "dst_x", "dst_y", "w", "h", "stride", "color")}
                        for image in images
                    ],
                }
                json_path = EXPECTED_DIR / f"{stem}.json"
                json_path.write_text(json.dumps(description, indent=2) + "\n", encoding="utf-8")
                print(f"{stem}: {len(images)} images")
    finally:
        shutil.rmtree(build_dir, ignore_errors=True)
    print("libass version:", ", ".join(sorted(versions)))


if __name__ == "__main__":
    main()
