#!/usr/bin/env bash
# Генерирует тестовые потоки AC-3 для crate-а ac3-decoder.
#
# Источник сигнала — синтетический (aevalsrc): на каждом канале свой тон плюс
# немного детерминированного шума, чтобы задействовать разные квантователи.
# Каждый файл — ровно 6 кадров (6 × 1536 сэмплов). Ожидаемые значения полей
# записаны в README.md (их же проверяют тесты src/tests/).
#
# Запуск: из каталога crates/ac3-decoder/tests/fixtures. Нужен ffmpeg с
# кодером ac3 (проверено на ffmpeg n9.0.2).
set -euo pipefail
cd "$(dirname "$0")"

FRAMES=6
SAMPLES=$((FRAMES * 1536))

# Частоты тонов по ролям каналов (одинаковые во всех файлах, чтобы тест мог
# проверить, что каждый канал декодирован на своём месте):
L=440; R=1000; C=2000; LFE=60; LS=3000; RS=7000

# signal <rate> <layout> <freq...> → описание lavfi-источника; частоты идут в
# порядке каналов ffmpeg для этого layout.
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
    -af "atrim=end_sample=${SAMPLES}" -c:a ac3 -b:a "$bitrate" "$@" -f ac3 "$name"
}

encode mono-48k-96k.ac3 48000 mono 96k "$C" -dialnorm -27 -audio_service_type co
encode stereo-48k-192k.ac3 48000 stereo 192k "$L $R" -dsur_mode on -dialnorm -20
encode stereo-44k1-128k-plain.ac3 44100 stereo 128k "$L $R" -channel_coupling 0 -stereo_rematrixing 0 -dsur_mode off
encode stereo-48k-32k.ac3 48000 stereo 32k "$L $R"
encode 3f-48k-256k.ac3 48000 3.0 256k "$L $R $C" -center_mixlev 0.5 -room_type large -mixing_level 105
encode 2f1r-32k-192k.ac3 32000 "3.0(back)" 192k "$L $R $LS" -surround_mixlev 0.707
encode 3f1r-48k-256k.ac3 48000 4.0 256k "$L $R $C $LS" -audio_service_type vi
encode 2f2r-48k-320k.ac3 48000 quad 320k "$L $R $LS $RS" -copyright 1 -original 0
encode 3f2r-48k-448k-cpl.ac3 48000 5.0 448k "$L $R $C $LS $RS" -channel_coupling 1 -cpl_start_band 5
encode mono-lfe-48k-128k.ac3 48000 "FC+LFE" 128k "$C $LFE"
encode stereo-lfe-44k1-192k.ac3 44100 2.1 192k "$L $R $LFE"
encode 2f1r-lfe-48k-256k.ac3 48000 "FL+FR+LFE+BC" 256k "$L $R $LFE $LS"
encode 3f-lfe-32k-256k.ac3 32000 3.1 256k "$L $R $C $LFE"
encode 3f1r-lfe-48k-384k.ac3 48000 4.1 384k "$L $R $C $LFE $LS"
encode 5p1-48k-640k-annexd.ac3 48000 5.1 640k "$L $R $C $LFE $LS $RS" -dmix_mode ltrt -ltrt_cmixlev 0.841 -ltrt_surmixlev 0.5 \
  -loro_cmixlev 1.0 -loro_surmixlev 0.707 -dsurex_mode on -dheadphone_mode off -ad_conv_type hdcd
encode 5p1-44k1-448k-annexd.ac3 44100 5.1 448k "$L $R $C $LFE $LS $RS" -dmix_mode loro -dsurex_mode dpliiz
encode 5p1-32k-192k.ac3 32000 5.1 192k "$L $R $C $LFE $LS $RS" -audio_service_type ka -per_frame_metadata 1
# Тоны выше начала coupling: проверка восстановления каналов из coupling канала.
encode stereo-48k-160k-cpl-high.ac3 48000 stereo 160k "5000 11000" -channel_coupling 1 -cpl_start_band 1

# --- Пакет D2 (синтез PCM) ---------------------------------------------------
# Перегрузка: «квадрат» почти полной шкалы — после декодирования пики > 1,0
# (выход не обрезается). Запятые в выражении экранированы для filtergraph.
SQUARE="0.98*sgn(sin(2*PI*440*t))|0.98*sgn(sin(2*PI*1000*t))"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "aevalsrc=${SQUARE}:s=48000:c=stereo" \
  -af "atrim=end_sample=${SAMPLES}" -c:a ac3 -b:a 192k -f ac3 overload-stereo-48k-192k.ac3

# Rematrixing: L и R почти одинаковы в полосах 0 (1,17–2,3 кГц) и 2
# (3,4–5,7 кГц) — кодер выбирает L+R/L−R, а малая разность (тоны 1700 и
# 4800 Гц) получает биты, то есть R' не «пустой». Coupling выключен, чтобы
# rematrixing шёл до верхней полосы.
REMAT_L="0.4*sin(2*PI*1500*t)+0.05*sin(2*PI*1700*t)+0.3*sin(2*PI*4500*t)+0.04*sin(2*PI*4800*t)"
REMAT_R="0.4*sin(2*PI*1500*t)-0.05*sin(2*PI*1700*t)+0.3*sin(2*PI*4500*t)-0.04*sin(2*PI*4800*t)"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "aevalsrc=${REMAT_L}|${REMAT_R}:s=48000:c=stereo" \
  -af "atrim=end_sample=${SAMPLES}" -c:a ac3 -b:a 256k -channel_coupling 0 -f ac3 stereo-48k-256k-rematrix.ac3

# Короткие преобразования: кодер ffmpeg blksw не выставляет, поэтому blksw = 1
# ставится всем каналам в блоке 0 каждого кадра (в AC-3 blksw не влияет на
# разбор остальных полей). CRC не пересчитан: тест декодирует с CrcPolicy::Skip.
python3 ../../tools/ffmpeg_sweep.py --derive-short-blocks stereo-48k-192k.ac3 stereo-48k-192k-short0.ac3

# Эталонные выдержки PCM декодера ffmpeg: f32le, interleaved, порядок каналов
# ffmpeg (FL FR FC LFE BC SL SR — по номеру бита позиции). Кадры [from, to).
reference() {
  local name=$1 from=$2 to=$3
  ffmpeg -hide_banner -loglevel error -y -f ac3 -i "$name.ac3" \
    -af "atrim=start_sample=$((from * 1536)):end_sample=$((to * 1536))" -f f32le "$name.ffmpeg-f32le"
}
reference stereo-48k-192k 0 2
reference stereo-48k-192k-short0 2 4
reference stereo-44k1-128k-plain 2 4
reference 2f1r-32k-192k 2 3
reference 3f2r-48k-448k-cpl 2 3
reference 5p1-48k-640k-annexd 2 3
reference overload-stereo-48k-192k 2 4
reference stereo-48k-256k-rematrix 2 4
