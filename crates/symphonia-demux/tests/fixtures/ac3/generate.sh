#!/usr/bin/env bash
# Генерация фикстур AC-3 / E-AC-3 в MKV и MP4/fMP4 для сквозных тестов
# `crates/symphonia-demux/tests/ac3_eac3_playback.rs` и вертикалей DASH/HLS fMP4
# в `crates/media-source-open/src/native_startup/{dash,hls}/`.
#
# Запуск: crates/symphonia-demux/tests/fixtures/ac3/generate.sh
# Требуется ffmpeg с libx264 и энкодерами ac3/eac3 (проверено на n9.0.2). Тесты
# ffmpeg не запускают: они читают закоммиченные файлы. Источники синтетические
# (testsrc2, синусы), ничего не скачивается.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# 1 с: два keyframe-а видео (GOP 5 при 10 к/с), чтобы seek в середину был настоящим.
duration=1
rate=48000

# Синус частоты $1 с амплитудой 0.25 (−12 dBFS).
tone() {
    printf 'sine=frequency=%s:sample_rate=%s:duration=%s,volume=2.0' "$1" "$rate" "$duration"
}

# 5.1(side): FL 300, FR 700, FC 500, LFE 60, SL 900, SR 1100 Гц — свой тон в каждом канале.
surround_graph="$(tone 300)[fl];$(tone 700)[fr];$(tone 500)[fc];$(tone 60)[lfe];$(tone 900)[sl];$(tone 1100)[sr];"
surround_graph+="[fl][fr][fc][lfe][sl][sr]join=inputs=6:channel_layout=5.1(side):map=0.0-FL|1.0-FR|2.0-FC|3.0-LFE|4.0-SL|5.0-SR[out]"

common=(-hide_banner -loglevel error -y)
video=(-f lavfi -i "testsrc2=size=64x64:rate=10:duration=$duration")
x264=(-c:v libx264 -preset ultrafast -profile:v baseline -g 5 -pix_fmt yuv420p)
output=(-fflags +bitexact -map_metadata -1)

# Метки дорожки: язык deu и название «Surround» (MKV хранит оба, MP4 — только язык).
labels=(-metadata:s:a:0 language=deu -metadata:s:a:0 title=Surround)

# $1 = audio encoder (ac3|eac3), $2 = bitrate, $3 = выходной файл; видео + 5.1.
muxed() {
    ffmpeg "${common[@]}" "${video[@]}" -filter_complex "$surround_graph" \
        -map 0:v -map "[out]" "${x264[@]}" -c:a "$1" -b:a "$2" \
        "${output[@]}" "${labels[@]}" "$3"
}

muxed ac3 256k "$here/h264-ac3-5p1.mkv"
muxed eac3 256k "$here/h264-eac3-5p1.mkv"
muxed ac3 256k "$here/h264-ac3-5p1.mp4"
muxed eac3 256k "$here/h264-eac3-5p1.mp4"

# Audio-only M4A с sample entry `ec-3` + `dec3` (`-f mp4`: muxer ipod для .m4a E-AC-3 не принимает).
ffmpeg "${common[@]}" -filter_complex "$surround_graph" -map "[out]" \
    -c:a eac3 -b:a 256k "${output[@]}" -f mp4 "$here/audio-only-eac3-5p1.m4a"

# Fragmented MP4 (init `ftyp+moov` с `dec3`, затем пары `moof+mdat` по keyframe-ам
# видео): тесты режут его по top-level box-ам на DASH/HLS init и media сегменты.
ffmpeg "${common[@]}" "${video[@]}" -filter_complex "$surround_graph" \
    -map 0:v -map "[out]" "${x264[@]}" -c:a eac3 -b:a 256k "${output[@]}" \
    -movflags +frag_keyframe+empty_moov+delay_moov+default_base_moof \
    "$here/h264-eac3-5p1-fragmented.mp4"
