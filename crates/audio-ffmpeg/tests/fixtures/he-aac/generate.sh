#!/usr/bin/env bash
# Фикстуры HE-AAC для тестов декодера и сквозных тестов плеера.
#
# Нужны: ffmpeg (генерация тонов, ремукс в fMP4/MPEG-TS), fdkaac (кодер
# Fraunhofer FDK AAC, пакет `fdkaac`), python3. Тестам во время запуска
# ничего из этого не нужно.
#
# Сигналы подобраны так, чтобы «глухой» звук (только ядро AAC без SBR) и
# моно вместо стерео (без PS) ломали тесты:
# - v1 стерео: слева 440 Гц, справа 14 кГц — выше половины частоты ядра
#   (11,025 кГц), его даёт только SBR;
# - v1 моно 48 кГц: 440 Гц + 15 кГц (выше 12 кГц половины частоты ядра 24 кГц);
# - v2: слева 440 Гц, справа 1 кГц — ядро моно, развести тоны по каналам может только PS.
#
# «Паспорт» (AudioSpecificConfig) — как у сайтов лент (решение 351):
# иерархический `2b920800` (Instagram), `2b098800` (Instagram, моно 24 кГц),
# `eb8a0800` (TikTok). fdkaac сам пишет обратно совместимую форму — она
# остаётся отдельной фикстурой `*-backward-compatible.m4a`.
set -euo pipefail

cd "$(dirname "$0")"
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

duration_seconds=2

# Стерео-сигнал из двух тонов: первый — левый канал, второй — правый.
stereo_tones() {
    local left_hz=$1 right_hz=$2 rate=$3 output=$4
    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i "sine=frequency=${left_hz}:duration=${duration_seconds}:sample_rate=${rate}" \
        -f lavfi -i "sine=frequency=${right_hz}:duration=${duration_seconds}:sample_rate=${rate}" \
        -filter_complex "[0][1]amerge=inputs=2,volume=0.5" -c:a pcm_s16le "$output"
}

stereo_tones 440 14000 44100 "$work_dir/v1-stereo.wav"
stereo_tones 440 1000 44100 "$work_dir/v2-ps.wav"
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "sine=frequency=440:duration=${duration_seconds}:sample_rate=48000" \
    -f lavfi -i "sine=frequency=15000:duration=${duration_seconds}:sample_rate=48000" \
    -filter_complex "[0][1]amix=inputs=2" -ac 1 -c:a pcm_s16le "$work_dir/v1-mono-48k.wav"

# --no-timestamp: одинаковые байты при повторной генерации.
encode() {
    fdkaac --silent --no-timestamp "$@"
}

encode -p 5 -b 48000 -o v1-stereo-backward-compatible.m4a "$work_dir/v1-stereo.wav"
encode -p 5 -b 32000 -o "$work_dir/v1-mono-48k.m4a" "$work_dir/v1-mono-48k.wav"
encode -p 29 -b 32000 -o "$work_dir/v2-ps.m4a" "$work_dir/v2-ps.wav"

python3 rewrite_asc_hierarchical.py v1-stereo-backward-compatible.m4a v1-stereo-hierarchical.m4a 2b920800
python3 rewrite_asc_hierarchical.py "$work_dir/v1-mono-48k.m4a" v1-mono-48k-hierarchical.m4a 2b098800
python3 rewrite_asc_hierarchical.py "$work_dir/v2-ps.m4a" v2-ps-hierarchical.m4a eb8a0800

# Фрагментированный MP4, как его кормит MSE страница (мост): тот же паспорт и кадры;
# без хвостовой коробки `mfra` — страница её в SourceBuffer не шлёт.
ffmpeg -hide_banner -loglevel error -y -i v2-ps-hierarchical.m4a -c copy -map_metadata -1 \
    -movflags +frag_keyframe+empty_moov+default_base_moof+skip_trailer -frag_duration 500000 \
    -fflags +bitexact v2-ps-hierarchical-fragmented.mp4

# Неявная сигнализация (радио, IPTV): ADTS, в заголовке только ядро AAC-LC,
# о SBR/PS сказано лишь внутри кадров. ADTS в MPEG-TS теряет заголовок у демуксера.
encode -p 5 -b 48000 -f 2 -o v1-stereo-implicit.adts "$work_dir/v1-stereo.wav"
encode -p 29 -b 32000 -f 2 -o v2-ps-implicit.adts "$work_dir/v2-ps.wav"
ffmpeg -hide_banner -loglevel error -y -i v1-stereo-implicit.adts -c copy -map_metadata -1 \
    -fflags +bitexact -f mpegts v1-stereo-implicit.ts

ls -l ./*.m4a ./*.mp4 ./*.adts ./*.ts
