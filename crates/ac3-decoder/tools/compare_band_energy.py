"""Сверка формы среднего спектра: наши коэффициенты против MDCT от PCM эталона.

Dev-инструмент пакета D3 (E-AC-3): частотные коэффициенты нашего декодера
(до синтеза PCM) сравниваются с PCM, который выдал внешний эталонный
декодер (бинарник ffmpeg или FLAC от другого декодера). PCM эталона
раскладывается тем же MDCT (N = 512, синусное окно, шаг 256), средняя
энергия по бинам обеих сторон нормируется на полосу бинов 13–48, и
печатается разница в дБ по полосам по 12 бинов. Так видны ошибки масштаба
SPX/AHT/coupling, не зависящие от абсолютного уровня и задержки.

Порядок:
1. CSV наших средних энергий по бинам (строка: бин, канал0, канал1, …):
   AC3_DIAG_FILE=<поток> AC3_DIAG_OUT=<csv> cargo test -p ac3-decoder --release \
       dump_mean_bin_energy -- --ignored
2. PCM эталона БЕЗ DRC: ffmpeg -drc_scale 0 -i <поток> -f f32le -acodec pcm_f32le <ref.f32>
   (наши коэффициенты здесь — до DRC; с DRC эталона переменное во времени
   усиление, связанное со спектром, даёт ложный «наклон» полос до −3 дБ —
   так появились наводки D3 rio_bravo −3 дБ и matrix2 commentary −0,6…−1 дБ,
   закрытые в D4). Точная сверка PCM — dev-тест eac3_ffmpeg_differential_sweep.
3. python3 -I compare_band_energy.py <csv> <ref.f32> <каналов> [карта] [каналов_эталона]
   «карта» — индексы каналов эталона для наших каналов по порядку, например
   для 3/2 против 5.1(side) ffmpeg: 0,2,1,4,5 (L C R Ls Rs) и 6.
"""
import sys

import numpy as np

MDCT_LENGTH = 512
HOP = MDCT_LENGTH // 2
BAND_WIDTH = 12
REFERENCE_BAND = slice(13, 49)


def mdct_power(signal):
    """Средняя по блокам энергия коэффициентов MDCT (бины 0–255)."""
    n = np.arange(MDCT_LENGTH)
    window = np.sin(np.pi * (n + 0.5) / MDCT_LENGTH)
    k = np.arange(HOP)
    basis = np.cos(2 * np.pi / MDCT_LENGTH * np.outer(n + 0.5 + MDCT_LENGTH / 4, k + 0.5))
    blocks = (len(signal) - MDCT_LENGTH) // HOP
    frames = np.stack([signal[i * HOP: i * HOP + MDCT_LENGTH] * window for i in range(blocks)])
    return ((frames @ basis) ** 2).mean(axis=0)


def main():
    ours_path, pcm_path, channels = sys.argv[1], sys.argv[2], int(sys.argv[3])
    mapping = [int(x) for x in sys.argv[4].split(',')] if len(sys.argv) > 4 else list(range(channels))
    reference_channels = int(sys.argv[5]) if len(sys.argv) > 5 else channels
    ours = np.loadtxt(ours_path, delimiter=',')[:, 1:]
    pcm = np.fromfile(pcm_path, dtype='<f4')
    pcm = pcm[: len(pcm) // reference_channels * reference_channels].reshape(-1, reference_channels)
    for channel in range(channels):
        reference = mdct_power(pcm[:, mapping[channel]])
        mine = ours[:, channel]
        reference = reference / reference[REFERENCE_BAND].mean()
        mine = mine / mine[REFERENCE_BAND].mean()
        cells = []
        for start in range(1, 253, BAND_WIDTH):
            band = slice(start, start + BAND_WIDTH)
            expected, actual = reference[band].mean(), mine[band].mean()
            if expected <= 0 or actual <= 0:
                cells.append(f"{start:3d}:  --")
            else:
                cells.append(f"{start:3d}:{10 * np.log10(actual / expected):+5.1f}")
        print(f"ch{channel}", ' '.join(cells))


main()
