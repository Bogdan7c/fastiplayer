#!/usr/bin/env python3
"""Dev-подбор привязки TPNP (Annex E §3.7) к эталону, который TPNP применяет.

Не нужен для `cargo test`. Им установлено (пакет D4), что положение
транзиента в координатах выхода нашего декодера — `4·transprocloc + 256`, а
`PN = 256 + transloc mod 256`: на 7 событиях потока Dolby
csi_miami_5.1_256_spx против PCM декодера Nero остаток в исправленных
областях −23,4 дБ при сдвиге +256 и не лучше −5,9 дБ при других (ffmpeg
n9.0.2 TPNP не применяет — с ним сравнивать нельзя).

Вход:
1. наш PCM без TPNP: `ac3_decode_raw <поток> <ours.f32> --tpnp off`
   (interleaved f32le в порядке каналов декодера);
2. PCM эталона (f32le, interleaved), `--lag` — на сколько отсчётов эталон
   отстаёт (у Nero 256);
3. события из `ac3_decode_raw <поток> /dev/null --inspect` (строки с
   `Some(TransientPreNoise`) — передаются как `кадр:transprocloc·4:translen`.

Запуск:
    python3 -I tpnp_reference_fit.py ours.f32 ref.f32 --channels 6 \\
        --ours-channel 1 --ref-channel 2 --lag 256 --frame 1536 \\
        --events 128:1788:252,333:1520:246
"""

import argparse

import numpy as np

TC1 = 256
TC2 = 128


def fade_in(length):
    phase = (np.arange(length) + 0.5) / length * np.pi / 2
    return np.sin(phase) ** 2


def apply_tpnp(pcm, transient, translen, pre_noise):
    """Псевдокод §3.7.2 (окна sin²/cos²) над копией сигнала."""
    output = pcm.copy()
    total = pre_noise + translen + TC1
    synth_start = transient - (2 * TC1 + 2 * pre_noise)
    synth = pcm[synth_start:synth_start + 2 * TC1 + pre_noise]
    start = transient - total
    leading = fade_in(TC1)
    trailing_out = 1 - fade_in(TC2)
    output[start:start + TC1] = pcm[start:start + TC1] * (1 - leading) + synth[:TC1] * leading
    output[start + TC1:start + total - TC2] = synth[TC1:total - TC2]
    tail = slice(start + total - TC2, start + total)
    output[tail] = pcm[tail] * (1 - trailing_out) + synth[total - TC2:total] * trailing_out
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("ours")
    parser.add_argument("reference")
    parser.add_argument("--channels", type=int, required=True)
    parser.add_argument("--ours-channel", type=int, required=True)
    parser.add_argument("--ref-channel", type=int, required=True)
    parser.add_argument("--lag", type=int, default=0)
    parser.add_argument("--frame", type=int, default=1536)
    parser.add_argument("--events", required=True)
    arguments = parser.parse_args()
    count = arguments.channels
    ours = np.fromfile(arguments.ours, "<f4").reshape(-1, count)[:, arguments.ours_channel].astype(np.float64)
    reference = np.fromfile(arguments.reference, "<f4")
    reference = reference[: len(reference) // count * count].reshape(-1, count)
    reference = reference[arguments.lag:, arguments.ref_channel].astype(np.float64)
    events = [tuple(int(part) for part in item.split(":")) for item in arguments.events.split(",")]

    def score(offset, base):
        total = 0.0
        for frame, location, translen in events:
            transient = frame * arguments.frame + location + offset
            fixed = apply_tpnp(ours, transient, translen, base + transient % 256)
            window = slice(transient - 1600, transient + 300)
            total += np.sum((fixed[window] - reference[window]) ** 2) / np.sum(reference[window] ** 2)
        return 10 * np.log10(total / len(events))

    results = [(score(offset, base), offset, base) for base in (0, 256, 512) for offset in range(-768, 769, 4)]
    results.sort()
    for value, offset, base in results[:5]:
        print(f"offset {offset:+5d}  PN = {base} + transloc mod 256  residual {value:6.2f} dB")


if __name__ == "__main__":
    main()
