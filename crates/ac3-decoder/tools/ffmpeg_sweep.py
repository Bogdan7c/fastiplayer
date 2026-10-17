#!/usr/bin/env python3
"""Dev-only дифференциальная сверка декодера AC-3 с бинарником ffmpeg.

Не запускается в CI и не нужен для `cargo test`. Скрипт только готовит
материал; метрики (PSNR/SNR/max|e| по каналам, поиск задержки, спектральное
разделение остатка на dither и не-dither бины) считает dev-тест крейта
`ffmpeg_differential_sweep` (src/tests/ffmpeg_differential.rs), у которого
есть доступ к внутренним флагам блоков (blksw) и к режимам dither.

Что делает:
1. Кодирует матрицу синтетических сигналов кодером ffmpeg `ac3`
   (частоты 48/44,1/32 кГц, все раскладки ±LFE, которые даёт кодер,
   битрейты от минимального до максимального, coupling вкл/выкл,
   rematrixing вкл/выкл, Annex D, транзиенты, почти тишина, перегрузка).
2. Строит производный поток, которого кодер ffmpeg не выдаёт: принудительный
   `blksw = 1` в блоке 0 каждого кадра (путь коротких преобразований). CRC
   не пересчитан: такие потоки декодируются с `crc=skip` (ffmpeg CRC по
   умолчанию не проверяет). bsid 9/10 сюда не входят: декодер ffmpeg n9.0.2
   на кадрах AC-3 с заменённым bsid 9/10 сообщает «exponent out-of-range»,
   то есть понимает их синтаксис иначе (спецификации эти bsid не описывают);
   наш путь проверяется тестом самосогласованности (PCM = PCM исходного
   bsid 8, частота ÷2/÷4).
3. Добавляет реальные потоки (каталог `--real-dir`) в нескольких режимах DRC.
4. Декодирует всё ffmpeg-ом в f32le, переставляет каналы в порядок
   декодера (A/52 Таблица 5.8, LFE последним) и пишет
   `<out>/<case>.ac3`, `<out>/<case>.ref.f32`, `<out>/manifest.tsv`.

Запуск (из корня репозитория):
    python3 crates/ac3-decoder/tools/ffmpeg_sweep.py --out <scratch>/sweep \
        [--real-dir <dir с реальными .ac3>] [--quick]
    AC3_DECODER_FFMPEG_SWEEP_DIR=<scratch>/sweep cargo test -p ac3-decoder \
        --release ffmpeg_differential_sweep -- --ignored --nocapture

Требуется ffmpeg с кодером/декодером ac3 (проверено на n9.0.2) и numpy.
"""

import argparse
import itertools
import os
import subprocess
import sys

import numpy as np

# Порядок каналов ffmpeg — по номеру бита позиции в AVChannelLayout.
FFMPEG_ORDER = {
    "Left": 0, "Right": 1, "Center": 2, "LowFrequencyEffects": 3,
    "CenterSurround": 8, "LeftSurround": 9, "RightSurround": 10,
}

# Позиции декодера по acmod (A/52 Таблица 5.8), LFE добавляется последним.
ACMOD_POSITIONS = {
    0: ["Left", "Right"], 1: ["Center"], 2: ["Left", "Right"],
    3: ["Left", "Center", "Right"], 4: ["Left", "Right", "CenterSurround"],
    5: ["Left", "Center", "Right", "CenterSurround"],
    6: ["Left", "Right", "LeftSurround", "RightSurround"],
    7: ["Left", "Center", "Right", "LeftSurround", "RightSurround"],
}

# Раскладки ffmpeg → роли каналов в порядке ffmpeg (для частот тонов).
LAYOUTS = {
    "mono": ["C"], "stereo": ["L", "R"], "3.0": ["L", "R", "C"],
    "3.0(back)": ["L", "R", "S"], "4.0": ["L", "R", "C", "S"],
    "quad(side)": ["L", "R", "Ls", "Rs"], "5.0(side)": ["L", "R", "C", "Ls", "Rs"],
    "FC+LFE": ["C", "LFE"], "2.1": ["L", "R", "LFE"],
    "FL+FR+LFE+BC": ["L", "R", "LFE", "S"], "3.1": ["L", "R", "C", "LFE"],
    # 2/2+LFE кодер ffmpeg кодирует как 3/2+LFE с молчащим C — заодно проверка тишины в канале.
    "4.1": ["L", "R", "C", "LFE", "S"], "FL+FR+LFE+SL+SR": ["L", "R", "LFE", "Ls", "Rs"],
    "5.1(side)": ["L", "R", "C", "LFE", "Ls", "Rs"],
}
TONE_HZ = {"L": 440, "R": 1000, "C": 2000, "LFE": 60, "S": 3000, "Ls": 3000, "Rs": 7000}
RATES = [48000, 44100, 32000]
BITRATES_KBPS = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640]
FRAMES = 32
SAMPLES_PER_FRAME = 1536


def run(command):
    """Запускает команду; возвращает True при успехе (stderr сохраняется в ошибке)."""
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    return result.returncode == 0, result.stderr


def channel_expression(role, index, signal):
    """Выражение aevalsrc для одного канала."""
    frequency = TONE_HZ[role]
    noise = f"0.03*(random({index})-0.5)"
    if signal == "tones":
        return f"0.4*sin(2*PI*{frequency}*t)+{noise}"
    if signal == "transient":
        # Удары каждые 0,125 с с быстрым спадом: кодер переключает окна/экспоненты.
        return f"0.9*sin(2*PI*{frequency}*t)*exp(-40*mod(t,0.125))+0.2*(random({index})-0.5)*lt(mod(t,0.125),0.004)"
    if signal == "silence":
        # −80 dBFS: проверка малых экспонент.
        return f"0.0001*sin(2*PI*{frequency}*t)"
    if signal == "overload":
        # Почти полная шкала, «квадрат» с бесконечным спектром: после
        # декодирования пики > 1,0 (их нельзя обрезать).
        return f"0.98*sgn(sin(2*PI*{frequency}*t))"
    raise ValueError(signal)


def encode(path, rate, layout, bitrate, signal, options):
    roles = LAYOUTS[layout]
    # Запятые внутри выражений экранируются: в синтаксисе filtergraph они разделяют фильтры.
    expressions = "|".join(
        channel_expression(role, index, signal).replace(",", "\\,") for index, role in enumerate(roles)
    )
    source = f"aevalsrc={expressions}:s={rate}:c={layout}"
    command = [
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", source,
        "-af", f"atrim=end_sample={FRAMES * SAMPLES_PER_FRAME}", "-c:a", "ac3",
        "-b:a", f"{bitrate}k", *options, "-f", "ac3", path,
    ]
    return run(command)


class BitReader:
    """MSB-first читатель для разбора BSI (A/52 Таблица 5.2)."""

    def __init__(self, data):
        self.data = data
        self.position = 0

    def read(self, width):
        value = 0
        for _ in range(width):
            byte = self.data[self.position // 8]
            value = (value << 1) | ((byte >> (7 - self.position % 8)) & 1)
            self.position += 1
        return value


def frame_length(frame):
    """Длина кадра в байтах по fscod/frmsizecod (A/52 Таблица 5.18): слова
    48 кГц, ×1,5 для 32 кГц, для 44,1 кГц — формула таблицы с битом дополнения."""
    fscod = frame[4] >> 6
    frmsizecod = frame[4] & 0x3F
    words_48 = [64, 64, 80, 80, 96, 96, 112, 112, 128, 128, 160, 160, 192, 192, 224, 224, 256, 256,
                320, 320, 384, 384, 448, 448, 512, 512, 640, 640, 768, 768, 896, 896, 1024, 1024,
                1152, 1152, 1280, 1280]
    if fscod == 0:
        return words_48[frmsizecod] * 2
    if fscod == 2:
        return words_48[frmsizecod] * 3
    kbps = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640][frmsizecod // 2]
    return (kbps * 1000 * 1536 // 44100 // 16 + (frmsizecod & 1)) * 2


def split_frames(stream):
    """Кадры подряд; мусор между ними (бывает в реальных файлах) пропускается
    поиском синхрослова 0x0B77 с допустимыми fscod/frmsizecod."""
    return [frame for _, frame in split_frames_with_offsets(stream)]


def split_frames_with_offsets(stream):
    """(смещение, кадр) для каждого кадра потока."""
    frames, position = [], 0
    while position + 6 <= len(stream):
        header = stream[position:position + 6]
        if header[0] != 0x0B or header[1] != 0x77 or header[4] >> 6 == 3 or header[4] & 0x3F > 37:
            position += 1
            continue
        length = frame_length(header)
        if position + length > len(stream):
            break
        frames.append((position, bytearray(stream[position:position + length])))
        position += length
    return frames


def bsi_layout_and_end(frame):
    """(acmod, lfeon, позиция бита после BSI) — Таблица 5.2."""
    reader = BitReader(frame)
    reader.position = 40
    reader.read(5)  # bsid
    reader.read(3)  # bsmod
    acmod = reader.read(3)
    if acmod & 1 and acmod != 1:
        reader.read(2)
    if acmod & 4:
        reader.read(2)
    if acmod == 2:
        reader.read(2)
    lfeon = reader.read(1)
    for _ in range(2 if acmod == 0 else 1):
        reader.read(5)
        if reader.read(1):
            reader.read(8)
        if reader.read(1):
            reader.read(8)
        if reader.read(1):
            reader.read(7)
    reader.read(2)
    for _ in range(2):  # timecod1e/xbsi1e, timecod2e/xbsi2e: по 14 бит
        if reader.read(1):
            reader.read(14)
    if reader.read(1):
        reader.read((reader.read(6) + 1) * 8)
    return acmod, lfeon, reader.position


def flip_bit(frame, position, value):
    mask = 0x80 >> (position % 8)
    if value:
        frame[position // 8] |= mask
    else:
        frame[position // 8] &= ~mask & 0xFF


def derive_short_blocks(source, target):
    """blksw[ch] = 1 для всех fbw каналов в блоке 0 каждого кадра (§5.4.3.1).
    В AC-3 blksw не влияет на разбор остальных полей — меняется только
    обратное преобразование."""
    frames = split_frames(open(source, "rb").read())
    for frame in frames:
        acmod, _, end = bsi_layout_and_end(frame)
        channels = len(ACMOD_POSITIONS[acmod])
        for channel in range(channels):
            flip_bit(frame, end + channel, 1)
    open(target, "wb").write(b"".join(frames))


def decoder_positions(path):
    """Позиции каналов декодера по первому кадру; None, если раскладка меняется."""
    frames = split_frames(open(path, "rb").read())
    layouts = {bsi_layout_and_end(frame)[:2] for frame in frames}
    if len(layouts) != 1:
        return None
    acmod, lfeon = layouts.pop()
    return ACMOD_POSITIONS[acmod] + (["LowFrequencyEffects"] if lfeon else [])


def decoded_packet_positions(source):
    """Смещения пакетов, которые декодер ffmpeg реально выдал кадрами
    (повреждённые кадры ffmpeg пропускает, не выдавая тишину)."""
    result = subprocess.run(
        ["ffprobe", "-hide_banner", "-loglevel", "error", "-f", "ac3", "-show_entries", "frame=pkt_pos",
         "-of", "csv=p=0", source],
        capture_output=True, text=True, check=False)
    if result.returncode != 0:
        return None
    # csv может добавить пустые поля через запятую — берём первое.
    return [int(line.split(",")[0]) for line in result.stdout.split() if line.strip()]


def reference_decode(case_dir, name, ffmpeg_decoder_options):
    """ffmpeg → f32le в порядке каналов декодера, кадр к кадру с нашей нарезкой.

    Пишет `<name>.ref.f32` (кадры, которые ffmpeg не выдал, — нули),
    `<name>.ref.missing` (их номера) и `<name>.frames.tsv` (смещение и длина
    каждого кадра — ту же нарезку использует dev-тест)."""
    source = os.path.join(case_dir, f"{name}.ac3")
    positions = decoder_positions(source)
    if positions is None:
        return False, "channel layout changes mid-stream"
    raw = os.path.join(case_dir, f"{name}.ffmpeg.f32")
    # -f ac3: без пробы формата (на коротких потоках проба иногда не узнаёт сырой AC-3).
    ok, error = run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *ffmpeg_decoder_options, "-f", "ac3",
                     "-i", source, "-f", "f32le", raw])
    if not ok:
        return False, error
    decoded_positions = decoded_packet_positions(source)
    if decoded_positions is None:
        return False, "ffprobe failed"
    frames = split_frames_with_offsets(open(source, "rb").read())
    frame_index = {offset: index for index, (offset, _) in enumerate(frames)}
    ffmpeg_pcm = np.fromfile(raw, dtype="<f4").reshape(-1, len(positions))
    if ffmpeg_pcm.shape[0] != len(decoded_positions) * SAMPLES_PER_FRAME:
        return False, "ffmpeg output length does not match its frame list"
    ffmpeg_columns = sorted(range(len(positions)), key=lambda index: FFMPEG_ORDER[positions[index]])
    reordered = np.zeros((len(frames) * SAMPLES_PER_FRAME, len(positions)), dtype="<f4")
    present = set()
    stream_end = frames[-1][0] + len(frames[-1][1]) if frames else 0
    for decoded, offset in enumerate(decoded_positions):
        if offset not in frame_index:
            # ffmpeg декодирует и обрезанный последний кадр файла (дополняя его);
            # наш декодер такой кадр отвергает как Truncated — из сравнения он выпадает.
            if offset >= stream_end:
                continue
            return False, f"ffmpeg frame at {offset} is not on our frame grid"
        target = frame_index[offset]
        present.add(target)
        block = ffmpeg_pcm[decoded * SAMPLES_PER_FRAME:(decoded + 1) * SAMPLES_PER_FRAME]
        for ffmpeg_column, decoder_column in enumerate(ffmpeg_columns):
            reordered[target * SAMPLES_PER_FRAME:(target + 1) * SAMPLES_PER_FRAME, decoder_column] = \
                block[:, ffmpeg_column]
    reordered.tofile(os.path.join(case_dir, f"{name}.ref.f32"))
    with open(os.path.join(case_dir, f"{name}.ref.missing"), "w") as handle:
        handle.write("".join(f"{index}\n" for index in range(len(frames)) if index not in present))
    with open(os.path.join(case_dir, f"{name}.frames.tsv"), "w") as handle:
        handle.write("".join(f"{offset}\t{len(frame)}\n" for offset, frame in frames))
    os.remove(raw)
    return True, ""


def synthetic_cases(quick):
    """(имя, семейство, rate, layout, bitrate, signal, опции кодера)."""
    rates = RATES[:1] if quick else RATES
    for rate, layout, bitrate in itertools.product(rates, LAYOUTS, BITRATES_KBPS):
        if quick and bitrate not in (96, 192, 448):
            continue
        yield (f"grid-{layout}-{rate}-{bitrate}", "layout×rate×bitrate", rate, layout, bitrate, "tones", [])
    for layout, coupling in itertools.product(["stereo", "3.0", "quad(side)", "5.1(side)"], [0, 1]):
        for bitrate in (96, 256, 448):
            yield (f"cpl{coupling}-{layout}-{bitrate}", f"coupling {'on' if coupling else 'off'}", 48000,
                   layout, bitrate, "tones", ["-channel_coupling", str(coupling)])
    for rate, rematrix, bitrate in itertools.product(rates, [0, 1], [64, 128, 192, 384]):
        yield (f"remat{rematrix}-{rate}-{bitrate}", f"rematrix {'on' if rematrix else 'off'}", rate,
               "stereo", bitrate, "tones", ["-stereo_rematrixing", str(rematrix), "-channel_coupling", "0"])
    for signal, rate, layout in itertools.product(["transient", "silence", "overload"], rates,
                                                  ["mono", "stereo", "5.1(side)"]):
        bitrate = {"mono": 96, "stereo": 192, "5.1(side)": 448}[layout]
        yield (f"{signal}-{layout}-{rate}", signal, rate, layout, bitrate, signal, [])
    annex_d = [
        ["-dmix_mode", "ltrt", "-ltrt_cmixlev", "0.841", "-loro_surmixlev", "0.707", "-dsurex_mode", "on",
         "-dheadphone_mode", "off", "-ad_conv_type", "hdcd"],
        ["-dmix_mode", "loro", "-dsurex_mode", "dpliiz"],
        ["-dmix_mode", "dplii", "-dialnorm", "-12"],
    ]
    for index, (options, rate) in enumerate(itertools.product(annex_d, rates)):
        for layout in ("stereo", "5.1(side)"):
            yield (f"annexd{index}-{layout}-{rate}", "Annex D (bsid 6)", rate, layout, 384, "tones", options)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out")
    parser.add_argument("--derive-short-blocks", nargs=2, metavar=("IN", "OUT"),
                        help="только записать поток с blksw = 1 в блоке 0 каждого кадра (для фикстур)")
    parser.add_argument("--real-dir")
    parser.add_argument("--quick", action="store_true")
    arguments = parser.parse_args()
    if arguments.derive_short_blocks:
        derive_short_blocks(*arguments.derive_short_blocks)
        return 0
    if not arguments.out:
        parser.error("--out is required for the sweep")
    os.makedirs(arguments.out, exist_ok=True)
    manifest = []
    skipped = []
    for name, family, rate, layout, bitrate, signal, options in synthetic_cases(arguments.quick):
        path = os.path.join(arguments.out, f"{name}.ac3")
        ok, error = encode(path, rate, layout, bitrate, signal, options)
        if not ok:
            skipped.append((name, error.strip().splitlines()[-1] if error.strip() else "encoder failed"))
            continue
        manifest.append((name, family, "standard", "verify"))
        if family == "layout×rate×bitrate" and bitrate in (96, 192, 448, 640):
            derive_short_blocks(path, os.path.join(arguments.out, f"short-{name}.ac3"))
            manifest.append((f"short-{name}", "blksw=1 (block 0)", "standard", "skip"))
    if arguments.real_dir:
        drc_modes = {"standard": [], "off": ["-drc_scale", "0"], "scaled:50:50": ["-drc_scale", "0.5"],
                     "heavy": ["-heavy_compr", "1"]}
        for file_name in sorted(os.listdir(arguments.real_dir)):
            if not file_name.endswith(".ac3"):
                continue
            stem = file_name[:-4]
            for drc in drc_modes:
                name = f"real-{stem}-drc-{drc.replace(':', '_')}"
                target = os.path.join(arguments.out, f"{name}.ac3")
                if not os.path.exists(target):
                    os.symlink(os.path.abspath(os.path.join(arguments.real_dir, file_name)), target)
                manifest.append((name, f"real DRC {drc}", drc, "verify"))
    decoder_options = {"standard": [], "off": ["-drc_scale", "0"], "scaled:50:50": ["-drc_scale", "0.5"],
                       "heavy": ["-heavy_compr", "1"]}
    written = []
    for name, family, drc, crc in manifest:
        ok, error = reference_decode(arguments.out, name, decoder_options[drc])
        if not ok:
            skipped.append((name, error.strip().splitlines()[-1] if error.strip() else "decode failed"))
            continue
        written.append((name, family, drc, crc))
    with open(os.path.join(arguments.out, "manifest.tsv"), "w") as handle:
        for row in written:
            handle.write("\t".join(row) + "\n")
    print(f"cases: {len(written)}, skipped: {len(skipped)}")
    for name, reason in skipped:
        print(f"  skipped {name}: {reason}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
