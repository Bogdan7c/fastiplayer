#!/usr/bin/env python3
"""Dev-only дифференциальная сверка PCM E-AC-3 с бинарником ffmpeg (пакет D4).

Не запускается в CI и не нужен для `cargo test`. Скрипт только готовит
материал; метрики считает dev-тест `eac3_ffmpeg_differential_sweep`
(src/tests/eac3_differential.rs: три декодера — без dither-а, с dither-ом и
с другим зерном шума SPX, — PSNR/SNR, спектральный разбор остатка на
чистые / dither / SPX-шумовые бины).

Что делает:
1. Кодирует матрицу синтетических сигналов кодером ffmpeg `eac3`
   (частоты 48/44,1/32 кГц, все раскладки ±LFE кодера, битрейты от
   минимального до максимального, coupling вкл/выкл, rematrixing вкл/выкл,
   транзиенты, почти тишина, перегрузка, метаданные микширования).
   Кодер ffmpeg пишет только независимые substream-ы по 6 блоков без
   AHT/SPX/TPNP — эти инструменты проверяются реальными потоками.
2. Пониженные частоты (Annex E Таблица E2.3) сюда не входят: декодер ffmpeg
   n9.0.2 их не реализует («Reduced sampling rate is not implemented»);
   наш путь проверяет коммитнутый тест эквивалентности (тот же кадр с
   fscod = 3/fscod2 даёт тот же PCM, что исходный 48/44,1/32 кГц).
3. Добавляет реальные потоки (`--real-dir`, файлы .eac3/.ac3; access unit-ы
   «ядро AC-3 + зависимые E-AC-3» тоже) в нескольких режимах DRC.
4. Декодирует всё ffmpeg-ом в f32le (порядок каналов ffmpeg; тест сам
   переставляет каналы) и пишет `<out>/<case>.eac3`, `<out>/<case>.ref.f32`,
   `<out>/manifest.tsv` (имя, семейство, DRC, CRC).

TPNP: ffmpeg (n9.0.2) его не применяет — проверено сверкой на потоках с
параметрами TPNP (см. Serena-память ac3-decoder/core, раздел D4), поэтому
тест декодирует с `TransientPreNoiseProcessing::Skip` (`AC3_DIFF_TPNP=apply`
— для эталонов, которые TPNP применяют, например PCM декодера Nero).
Известные расхождения ffmpeg в потоках с AHT (наш выход совпадает с Nero):
см. память; `AC3_EMULATE_FFMPEG_HEBAP4=1` эмулирует сдвиг строк VQ hebap 4,
`AC3_FIT_BLOCK_GAIN=1` подбирает множитель блока (эталон с иным DRC).

Запуск (из корня репозитория):
    python3 -I crates/ac3-decoder/tools/eac3_sweep.py --out <scratch>/eac3-sweep \\
        [--real-dir <dir с реальными .eac3/.ac3>] [--quick]
    AC3_DECODER_EAC3_SWEEP_DIR=<scratch>/eac3-sweep cargo test -p ac3-decoder \\
        --release eac3_ffmpeg_differential_sweep -- --ignored --nocapture

Требуется ffmpeg с кодером/декодером eac3 (проверено на n9.0.2).
"""

import argparse
import itertools
import os
import subprocess
import sys

LAYOUTS = {
    "mono": ["C"], "stereo": ["L", "R"], "3.0": ["L", "R", "C"],
    "3.0(back)": ["L", "R", "S"], "4.0": ["L", "R", "C", "S"],
    "quad(side)": ["L", "R", "Ls", "Rs"], "5.0(side)": ["L", "R", "C", "Ls", "Rs"],
    "FC+LFE": ["C", "LFE"], "2.1": ["L", "R", "LFE"],
    "FL+FR+LFE+BC": ["L", "R", "LFE", "S"], "3.1": ["L", "R", "C", "LFE"],
    "4.1": ["L", "R", "C", "LFE", "S"], "FL+FR+LFE+SL+SR": ["L", "R", "LFE", "Ls", "Rs"],
    "5.1(side)": ["L", "R", "C", "LFE", "Ls", "Rs"],
}
TONE_HZ = {"L": 440, "R": 1000, "C": 2000, "LFE": 60, "S": 3000, "Ls": 3000, "Rs": 7000}
RATES = [48000, 44100, 32000]
BITRATES_KBPS = [48, 64, 96, 128, 192, 256, 384, 640, 1024, 1536, 3072]
FRAMES = 32
SAMPLES_PER_FRAME = 1536


def run(command):
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    return result.returncode == 0, result.stderr


def channel_expression(role, index, signal):
    """Выражение aevalsrc для канала (те же сигналы, что в ffmpeg_sweep.py)."""
    frequency = TONE_HZ[role]
    noise = f"0.03*(random({index})-0.5)"
    if signal == "tones":
        return f"0.4*sin(2*PI*{frequency}*t)+{noise}"
    if signal == "transient":
        return (f"0.9*sin(2*PI*{frequency}*t)*exp(-40*mod(t,0.125))"
                f"+0.2*(random({index})-0.5)*lt(mod(t,0.125),0.004)")
    if signal == "silence":
        return f"0.0001*sin(2*PI*{frequency}*t)"
    if signal == "overload":
        return f"0.98*sgn(sin(2*PI*{frequency}*t))"
    raise ValueError(signal)


def encode(path, rate, layout, bitrate, signal, options):
    expressions = "|".join(
        channel_expression(role, index, signal).replace(",", "\\,")
        for index, role in enumerate(LAYOUTS[layout])
    )
    source = f"aevalsrc={expressions}:s={rate}:c={layout}"
    command = [
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", source,
        "-af", f"atrim=end_sample={FRAMES * SAMPLES_PER_FRAME}", "-c:a", "eac3",
        "-b:a", f"{bitrate}k", *options, "-f", "eac3", path,
    ]
    return run(command)


def reference_decode(directory, name, options):
    source = os.path.join(directory, f"{name}.eac3")
    target = os.path.join(directory, f"{name}.ref.f32")
    return run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *options, "-i", source,
                "-f", "f32le", "-acodec", "pcm_f32le", target])


def synthetic_cases(quick):
    """(имя, семейство, rate, layout, bitrate, signal, опции кодера)."""
    rates = RATES[:1] if quick else RATES
    for rate, layout, bitrate in itertools.product(rates, LAYOUTS, BITRATES_KBPS):
        if quick and bitrate not in (96, 384, 1024):
            continue
        yield (f"grid-{layout}-{rate}-{bitrate}", "layout×rate×bitrate", rate, layout, bitrate, "tones", [])
    for layout, coupling in itertools.product(["stereo", "3.0", "quad(side)", "5.1(side)"], [0, 1]):
        for bitrate in (96, 256, 640):
            yield (f"cpl{coupling}-{layout}-{bitrate}", f"coupling {'on' if coupling else 'off'}", 48000,
                   layout, bitrate, "tones", ["-channel_coupling", str(coupling)])
    for rate, rematrix, bitrate in itertools.product(rates, [0, 1], [64, 128, 192, 384]):
        yield (f"remat{rematrix}-{rate}-{bitrate}", f"rematrix {'on' if rematrix else 'off'}", rate,
               "stereo", bitrate, "tones", ["-stereo_rematrixing", str(rematrix), "-channel_coupling", "0"])
    for signal, rate, layout in itertools.product(["transient", "silence", "overload"], rates,
                                                  ["mono", "stereo", "5.1(side)"]):
        bitrate = {"mono": 96, "stereo": 192, "5.1(side)": 448}[layout]
        yield (f"{signal}-{layout}-{rate}", signal, rate, layout, bitrate, signal, [])
    metadata = [
        ["-dmix_mode", "ltrt", "-ltrt_cmixlev", "0.841", "-loro_surmixlev", "0.707", "-mixing_level", "100",
         "-room_type", "large", "-dialnorm", "-20"],
        ["-dsur_mode", "on", "-dheadphone_mode", "on", "-copyright", "1", "-original", "0"],
    ]
    for index, (options, layout) in enumerate(itertools.product(metadata, ["stereo", "5.1(side)"])):
        yield (f"meta{index}-{layout}", "mixing/informational metadata", 48000, layout, 384, "tones", options)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", required=True)
    parser.add_argument("--real-dir")
    parser.add_argument("--quick", action="store_true")
    arguments = parser.parse_args()
    os.makedirs(arguments.out, exist_ok=True)
    manifest, skipped = [], []
    for name, family, rate, layout, bitrate, signal, options in synthetic_cases(arguments.quick):
        path = os.path.join(arguments.out, f"{name}.eac3")
        ok, error = encode(path, rate, layout, bitrate, signal, options)
        if not ok:
            skipped.append((name, error.strip().splitlines()[-1] if error.strip() else "encoder failed"))
            continue
        manifest.append((name, family, "standard", "verify", []))
    if arguments.real_dir:
        # «heavy» (compr) не сверяется: декодер eac3 ffmpeg n9.0.2 слова compr не
        # применяет — его -heavy_compr 1 даёт ровно тот же PCM, что dynrng
        # (проверено на 7.1 Dolby, где compr = 245 ≠ 0 во всех кадрах);
        # наш путь compr проверяют коммитнутые тесты.
        drc_modes = {"standard": [], "off": ["-drc_scale", "0"], "scaled:50:50": ["-drc_scale", "0.5"]}
        for file_name in sorted(os.listdir(arguments.real_dir)):
            stem, extension = os.path.splitext(file_name)
            if extension not in (".eac3", ".ac3"):
                continue
            for drc, options in drc_modes.items():
                name = f"real-{stem}-drc-{drc.replace(':', '_')}"
                target = os.path.join(arguments.out, f"{name}.eac3")
                if not os.path.exists(target):
                    os.symlink(os.path.abspath(os.path.join(arguments.real_dir, file_name)), target)
                manifest.append((name, f"real DRC {drc}", drc, "verify", options))
    written = []
    for name, family, drc, crc, options in manifest:
        ok, error = reference_decode(arguments.out, name, options)
        if not ok:
            skipped.append((name, error.strip().splitlines()[-1] if error.strip() else "decode failed"))
            continue
        written.append((name, family, drc, crc))
    with open(os.path.join(arguments.out, "manifest.tsv"), "w") as handle:
        handle.write("".join("\t".join(row) + "\n" for row in written))
    print(f"cases: {len(written)}, skipped: {len(skipped)}")
    for name, reason in skipped:
        print(f"  skipped {name}: {reason}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
