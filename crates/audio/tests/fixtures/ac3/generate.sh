#!/usr/bin/env bash
# Генерация фикстур AC-3 / E-AC-3 для тестов `crates/audio/src/decoder/ac3/tests.rs`.
#
# Запуск: crates/audio/tests/fixtures/ac3/generate.sh
# Требуется ffmpeg с энкодерами ac3/eac3 (проверено на n9.0.2). Тесты ffmpeg не
# запускают: они читают закоммиченные файлы через include_bytes!.
#
# В каждом канале — свой синус (частоты перечислены в README.md), поэтому тест
# по энергии тонов доказывает не только декодирование, но и раскладку каналов.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# Длительность 0.5 с = 15–16 syncframe-ов по 1536 samples при 48 кГц.
duration=0.5
rate=48000

# Синус частоты $1 с амплитудой 0.25 (−12 dBFS).
tone() {
    printf 'sine=frequency=%s:sample_rate=%s:duration=%s,volume=2.0' "$1" "$rate" "$duration"
}

# Stereo AC-3: L = 440 Гц, R = 1000 Гц.
ffmpeg -hide_banner -loglevel error -y \
    -filter_complex "$(tone 440)[l];$(tone 1000)[r];[l][r]join=inputs=2:channel_layout=stereo[out]" \
    -map "[out]" -c:a ac3 -b:a 192k -f ac3 "$here/tones-stereo.ac3"

# 5.1(side) AC-3 и E-AC-3: FL 300, FR 700, FC 500, LFE 60, SL 900, SR 1100 Гц.
surround_graph="$(tone 300)[fl];$(tone 700)[fr];$(tone 500)[fc];$(tone 60)[lfe];$(tone 900)[sl];$(tone 1100)[sr];"
surround_graph+="[fl][fr][fc][lfe][sl][sr]join=inputs=6:channel_layout=5.1(side):map=0.0-FL|1.0-FR|2.0-FC|3.0-LFE|4.0-SL|5.0-SR[out]"

ffmpeg -hide_banner -loglevel error -y -filter_complex "$surround_graph" \
    -map "[out]" -c:a ac3 -b:a 448k -f ac3 "$here/tones-5.1.ac3"

ffmpeg -hide_banner -loglevel error -y -filter_complex "$surround_graph" \
    -map "[out]" -c:a eac3 -b:a 384k -f eac3 "$here/tones-5.1.eac3"
