#!/usr/bin/env bash
# Генерирует тестовые потоки E-AC-3 (Annex E) для crate-а ac3-decoder.
#
# Сигнал — тот же, что у фикстур AC-3 (../generate.sh): на каждом канале свой
# тон амплитуды 0,4 плюс детерминированный шум ±0,015. Каждый файл — ровно
# 6 кадров E-AC-3 по 6 блоков (6 × 1536 сэмплов). Ожидаемые значения полей —
# в README.md (их проверяют тесты src/tests/eac3/).
#
# Запуск: из каталога crates/ac3-decoder/tests/fixtures/eac3. Нужен ffmpeg с
# кодером eac3 (проверено на ffmpeg n9.0.2).
set -euo pipefail
cd "$(dirname "$0")"

FRAMES=6
SAMPLES=$((FRAMES * 1536))
L=440; R=1000; C=2000; LFE=60; LS=3000; RS=7000

signal() {
  local rate=$1 layout=$2 exprs="" index=0 frequency
  shift 2
  for frequency in "$@"; do
    exprs+="0.4*sin(2*PI*${frequency}*t)+0.03*(random(${index})-0.5)|"
    index=$((index + 1))
  done
  echo "aevalsrc=${exprs%|}:s=${rate}:c=${layout}"
}

# encode <name> <rate> <layout> <bitrate> "<freq...>" [extra ffmpeg options...]
encode() {
  local name=$1 rate=$2 layout=$3 bitrate=$4 frequencies=$5
  shift 5
  # shellcheck disable=SC2086 # частоты намеренно разбиваются на слова
  ffmpeg -hide_banner -loglevel error -y -f lavfi -i "$(signal "$rate" "$layout" $frequencies)" \
    -af "atrim=end_sample=${SAMPLES}" -c:a eac3 -b:a "$bitrate" "$@" -f eac3 "$name"
}

encode mono-48k-64k.eac3 48000 mono 64k "$C" -dialnorm -27 -mixing_level 105 -room_type small
encode stereo-48k-96k.eac3 48000 stereo 96k "$L $R" -dsur_mode on -dheadphone_mode on
encode stereo-44k1-192k.eac3 44100 stereo 192k "$L $R" -copyright 1 -original 0
encode stereo-32k-640k.eac3 32000 stereo 640k "$L $R"
encode stereo-48k-128k-cpl-high.eac3 48000 stereo 128k "5000 11000" -channel_coupling 1 -cpl_start_band 1
encode 3f-48k-256k.eac3 48000 3.0 256k "$L $R $C" -dmix_mode loro -loro_cmixlev 0.5
encode 2f1r-48k-192k.eac3 48000 "3.0(back)" 192k "$L $R $LS"
encode quad-44k1-256k.eac3 44100 quad 256k "$L $R $LS $RS"
encode 2p1-48k-192k.eac3 48000 2.1 192k "$L $R $LFE"
encode 5p1-48k-384k.eac3 48000 5.1 384k "$L $R $C $LFE $LS $RS" \
  -dmix_mode ltrt -ltrt_cmixlev 0.841 -ltrt_surmixlev 0.5 -loro_cmixlev 1.0 -loro_surmixlev 0.707 \
  -dsurex_mode on -ad_conv_type hdcd -mixing_level 100 -room_type large
encode 5p1-32k-1000k.eac3 32000 5.1 1000k "$L $R $C $LFE $LS $RS"

# Выдержки PCM декодера ffmpeg (f32le, порядок каналов ffmpeg) для сверки
# синтеза E-AC-3 (src/tests/eac3/pcm_reference.rs): отсчёты
# [from, to) потока на канал.
reference() {
  local name=$1 from=$2 to=$3
  ffmpeg -hide_banner -loglevel error -y -f eac3 -i "$name.eac3" \
    -af "atrim=start_sample=${from}:end_sample=${to}" -f f32le "$name.ffmpeg-f32le"
}
reference stereo-48k-96k 0 3072
reference 5p1-48k-384k 3072 4608
reference 5p1-32k-1000k 3072 4608
