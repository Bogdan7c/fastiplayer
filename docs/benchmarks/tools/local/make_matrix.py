#!/usr/bin/env python3
"""Генерирует наборы сцен для замеров из одного мастера: матрицу HW-квалификации и пресеты T410.

Решение владельца (сессия 06 cpu-parity): одна и та же картинка во всех 16 файлах,
чтобы кодеки и разрешения сравнивались между собой честно, и «тяжёлые, но жизненные»
битрейты — у верхней границы уровня кодека, который реально встречается у камер,
Blu-ray и веб-загрузок, но без лабораторного запредела.

Мастер и отрезок задаются аргументами; рецепт полностью описан таблицами ``PRESETS``
(``matrix`` — 16 + Main10 сессии 06; ``t410`` и ``t410-hdr`` — старое железо, план
``user/gles2-renderer/`` сессия 00) и функцией ``encoder_arguments``. Готовый файл
проверяется ffprobe (кодек, размер, частота, длительность, наличие звука), а хэши и фактический битрейт пишутся в
``manifest.json`` рядом с файлами. Существующий файл никогда не перезаписывается
молча: повторный запуск без ``--force`` только перепроверяет его.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
import json
from pathlib import Path
import subprocess
import sys

from host import sha256, write_json


@dataclass(frozen=True)
class Variant:
    """Одна сцена матрицы: что кодируем и каким человекочитаемым именем называем файл."""

    resolution: str  # метка каталога и имени: 720p30, 1080p60, 4k60, 576i25, 1080i30
    width: int
    height: int
    fps: int  # кадров в секунду; у чересстрочных — полных кадров (полей вдвое больше)
    codec: str  # ключ кодека в протоколе: h264, hevc, av1, vp9, mpeg2
    profile: str  # профиль для имени файла и проверки ffprobe
    video_mbps: int  # целевой средний битрейт видео
    bit_depth: int = 8  # 10 — сцены HEVC Main10 (путь P010)
    scan: str = 'progressive'  # 'interlaced' — MPEG-2 сцены эфира/DVD (поля сверху первыми)
    dynamic_range: str = 'sdr'  # 'hdr10' — PQ/BT.2020, нагрузка HDR→SDR на рендер
    tier: str = ''  # различает сцены одного кодека и разрешения, например 'stress'

    @property
    def pixel_format(self):
        return 'yuv420p10le' if self.bit_depth == 10 else 'yuv420p'

    @property
    def interlaced(self):
        return self.scan == 'interlaced'

    @property
    def hdr(self):
        return self.dynamic_range == 'hdr10'

    @property
    def container(self):
        # VP9 живёт в WebM с Opus (как у YouTube); MPEG-2 — в MPEG-TS с MP2 (как эфир DVB);
        # остальное — MP4 с AAC.
        return {'vp9': 'webm', 'mpeg2': 'ts'}.get(self.codec, 'mp4')

    @property
    def audio_label(self):
        return {'vp9': 'opus', 'mpeg2': 'mp2'}.get(self.codec, 'aac')

    @property
    def relative_path(self):
        range_label = f'-{self.dynamic_range}' if self.hdr else ''
        name = (f'{self.resolution}_{self.codec}-{self.profile}{range_label}_{self.video_mbps}mbps_'
                f'{self.audio_label}.{self.container}')
        return Path(self.resolution) / name

    @property
    def scenario(self):
        """Нейтральная метка сцены для плана серии и публичного отчёта."""
        depth = '10' if self.bit_depth == 10 else ''
        qualifiers = ''.join(f'-{label}' for label in ('hdr' if self.hdr else '', self.tier) if label)
        return f'{self.codec}{depth}{qualifiers}-{self.resolution}'


# Битрейты, Мбит/с. Ориентиры — верх «жизненного» диапазона и предел уровня кодека
# (уровень — ограничение стандарта, которое соблюдают камеры и сервисы):
# H.264 — Blu-ray/камеры (1080p30 25 = предел High@4.0, 4K60 120 — экшн-камеры);
# HEVC — телефоны/дроны в main tier (4K60 60 = предел Main@5.2);
# AV1 — веб с высоким качеством (4K60 40 = предел Main@5.1 main tier);
# VP9 — веб/YouTube-оригиналы (4K60 50, внутри Level 5.1).
BITRATES_MBPS = {
    'h264': {'1080p30': 25, '1080p60': 40, '4k30': 80, '4k60': 120},
    'hevc': {'1080p30': 15, '1080p60': 20, '4k30': 40, '4k60': 60},
    'av1': {'1080p30': 10, '1080p60': 15, '4k30': 30, '4k60': 40},
    'vp9': {'1080p30': 12, '1080p60': 20, '4k30': 35, '4k60': 50},
}
PROFILES = {'h264': 'high', 'hevc': 'main', 'av1': 'main', 'vp9': 'profile0', 'mpeg2': 'main'}
RESOLUTIONS = {
    '720p30': (1280, 720, 30), '720p60': (1280, 720, 60),
    '1080p30': (1920, 1080, 30), '1080p60': (1920, 1080, 60),
    '4k30': (3840, 2160, 30), '4k60': (3840, 2160, 60),
    # Чересстрочные: 576i — PAL/DVD (25 кадров = 50 полей), 1080i — HD-эфир (30 кадров = 60 полей).
    '576i25': (720, 576, 25), '1080i30': (1920, 1080, 30),
}
MATRIX_RESOLUTIONS = ('1080p30', '1080p60', '4k30', '4k60')
MATRIX = [Variant(resolution, *RESOLUTIONS[resolution], codec, PROFILES[codec], rates[resolution])
          for codec, rates in BITRATES_MBPS.items() for resolution in MATRIX_RESOLUTIONS]
# Дополнительная сцена плана сессии 06: 10-битный HEVC (аппаратный путь P010) на том же
# мастере и с тем же битрейтом, что 8-битный HEVC 4K60, — отличается только глубина цвета.
MATRIX.append(Variant('4k60', 3840, 2160, 60, 'hevc', 'main10', BITRATES_MBPS['hevc']['4k60'], bit_depth=10))


def t410_variant(resolution, codec, video_mbps, **options):
    """Сцена с размером и частотой из ``RESOLUTIONS`` и профилем кодека по умолчанию."""
    return Variant(resolution, *RESOLUTIONS[resolution], codec, PROFILES[codec], video_mbps, **options)


# Сцены ThinkPad T410 (план user/gles2-renderer, сессия 00). «Обычные» H.264 — жизненный
# битрейт веб-загрузок и телефонов (то, что владелец старого ноутбука реально смотрит);
# стресс-лесенка — по возрастанию нагрузки на процессор, 1080p-ступени совпадают с матрицей
# сессии 06 по битрейту, чтобы цифры двух наборов можно было сопоставлять.
T410 = [
    t410_variant('720p30', 'h264', 5), t410_variant('720p60', 'h264', 8),
    t410_variant('1080p30', 'h264', 10), t410_variant('1080p60', 'h264', 15),
    t410_variant('1080p60', 'h264', BITRATES_MBPS['h264']['1080p60'], tier='stress'),
    t410_variant('720p30', 'hevc', 4), t410_variant('1080p30', 'hevc', BITRATES_MBPS['hevc']['1080p30']),
    t410_variant('1080p60', 'hevc', BITRATES_MBPS['hevc']['1080p60']),
    t410_variant('720p30', 'vp9', 4), t410_variant('720p60', 'vp9', 6),
    t410_variant('1080p30', 'vp9', BITRATES_MBPS['vp9']['1080p30']),
    t410_variant('1080p60', 'vp9', BITRATES_MBPS['vp9']['1080p60']),
    t410_variant('720p30', 'av1', 3), t410_variant('1080p30', 'av1', BITRATES_MBPS['av1']['1080p30']),
    t410_variant('1080p60', 'av1', BITRATES_MBPS['av1']['1080p60']),
    # MPEG-2: решение по аппаратному MPEG-2 (бэклог плана) принимается по замеру программного.
    # Битрейты — DVD (576i) и HD-эфир (1080i).
    t410_variant('576i25', 'mpeg2', 6, scan='interlaced'),
    t410_variant('1080i30', 'mpeg2', 20, scan='interlaced'),
]
# Отдельный пресет, потому что нужен другой мастер — 10-битный HDR10 (PQ/BT.2020):
# программный 10-бит + HDR→SDR, нагрузка и на декодер, и на шейдер рендера.
T410_HDR = [Variant('1080p30', *RESOLUTIONS['1080p30'], 'hevc', 'main10', BITRATES_MBPS['hevc']['1080p30'],
                    bit_depth=10, dynamic_range='hdr10')]
PRESETS = {'matrix': MATRIX, 't410': T410, 't410-hdr': T410_HDR}

# Имя кодека и профиля так, как их печатает ffprobe: проверка результата, а не имени файла.
FFPROBE_CODEC = {'mpeg2': 'mpeg2video'}
FFPROBE_PROFILE = {('h264', 'high'): 'High', ('hevc', 'main'): 'Main', ('hevc', 'main10'): 'Main 10',
                   ('av1', 'main'): 'Main', ('vp9', 'profile0'): 'Profile 0', ('mpeg2', 'main'): 'Main'}
# Буфер VBV MPEG-2 ограничен стандартом (Main profile: Main level 1 835 008 бит, High level
# 9 781 248 бит); правило «2× битрейта» других кодеков дало бы недопустимый поток.
MPEG2_VBV_BITS = {'576i25': 1_835_008, '1080i30': 9_781_248}
# Анаморфные сцены: SAR, при котором кадр показывается как 16:9 мастера (PAL 16:9).
ANAMORPHIC_SAR = {'576i25': '64/45'}
# Цветовые метки HDR10, которые проверяются у готового файла и передаются энкодеру.
HDR10_COLOR = {'color_primaries': 'bt2020', 'color_transfer': 'smpte2084', 'color_space': 'bt2020nc'}


def encoder_arguments(variant):
    """Аргументы видео- и аудиоэнкодера одного варианта.

    Везде одинаковы: ключевой кадр раз в 2 секунды (как у стриминга), однопроходный
    VBR со средним ``video_mbps``, пиками до 1,5× и буфером 2× (у SVT-AV1 потолок
    пиков не задаётся этим способом — средний битрейт и запрет недобора), 4:2:0 (8-bit, кроме
    дополнительной Main10-сцены).
    """
    rate = variant.video_mbps * 1_000_000
    gop = 2 * variant.fps
    vbv = ['-maxrate', str(rate * 3 // 2), '-bufsize', str(rate * 2)]
    common = ['-b:v', str(rate), '-g', str(gop), '-pix_fmt', variant.pixel_format]
    if variant.codec == 'h264':
        video = ['-c:v', 'libx264', '-preset', 'slow', '-profile:v', 'high', *common, *vbv]
    elif variant.codec == 'hevc':
        # hvc1 — тег, с которым HEVC в MP4 открывают и Apple, и VLC, и браузеры.
        video = ['-c:v', 'libx265', '-preset', 'medium', '-profile:v', variant.profile, '-tag:v', 'hvc1',
                 *common, *vbv]
        if variant.hdr:
            # Метки PQ/BT.2020 в потоке и контейнере: по ним плеер включает HDR→SDR.
            video += ['-color_primaries', 'bt2020', '-color_trc', 'smpte2084', '-colorspace', 'bt2020nc',
                      '-x265-params', 'hdr10=1:repeat-headers=1']
    elif variant.codec == 'av1':
        # Однопроходный VBR SVT-AV1 по умолчанию недобирает цель на 20–30 % (замер
        # сессии 06: 7,1 вместо 10 Мбит/с); undershoot-pct=0 запрещает недобор,
        # перебор остаётся ограничен штатным overshoot-pct энкодера.
        video = ['-c:v', 'libsvtav1', '-preset', '6', '-svtav1-params', 'undershoot-pct=0', *common]
    elif variant.codec == 'mpeg2':
        # ilme/ildct — чересстрочное кодирование движения и DCT.
        # Порядок полей (верхнее первым) энкодер берёт из кадров фильтра ``interlace``.
        video = ['-c:v', 'mpeg2video', '-flags', '+ilme+ildct', *common,
                 '-maxrate', str(rate * 3 // 2), '-bufsize', str(MPEG2_VBV_BITS[variant.resolution])]
    elif variant.codec == 'vp9':
        # Тайлы по ширине — как у реальных VP9-файлов YouTube; row-mt только ускоряет энкод.
        tiles = '2' if variant.width >= 3840 else '1'
        video = ['-c:v', 'libvpx-vp9', '-deadline', 'good', '-cpu-used', '3', '-row-mt', '1',
                 '-tile-columns', tiles, *common, *vbv]
    else:
        raise ValueError(f'unknown codec {variant.codec}')
    audio = {'vp9': ['-c:a', 'libopus', '-b:a', '160k'],
             'mpeg2': ['-c:a', 'mp2', '-b:a', '192k']}.get(variant.codec, ['-c:a', 'aac', '-b:a', '256k'])
    return [*video, *audio, '-ar', '48000', '-ac', '2']


def ffmpeg_command(variant, master, start_seconds, duration_seconds, output):
    """Полная команда: точный отрезок мастера → масштаб/частота → энкодер → файл."""
    filters = []
    # У чересстрочной сцены каждый кадр мастера становится полем: частота полей = 2 × кадров.
    source_rate = variant.fps * 2 if variant.interlaced else variant.fps
    if source_rate != 60:
        # Мастер 60 fps; 30 fps — выбрасывание каждого второго кадра без смешивания,
        # 50 полей 576i — выбрасывание каждого шестого.
        filters.append(f'fps={source_rate}')
    filters.append(f'scale={variant.width}:{variant.height}:flags=lanczos')
    if variant.resolution in ANAMORPHIC_SAR:
        # 576i из 16:9 мастера — анаморфный PAL 16:9 (пиксель шире квадратного).
        filters.append(f'setsar={ANAMORPHIC_SAR[variant.resolution]}')
    if variant.interlaced:
        # Два соседних кадра → два поля одного кадра (верхнее первым), с вертикальным
        # low-pass, как у настоящего эфира: иначе мерцание тонких линий.
        filters.append('interlace=scan=tff:lowpass=complex')
    if variant.hdr:
        # Метки HDR10 на кадрах: ffmpeg берёт цветовые свойства потока из кадров фильтра,
        # и без этого шага энкодер записал бы «не указано» вместо PQ/BT.2020.
        filters.append('setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc')
    container = ['-movflags', '+faststart'] if variant.container == 'mp4' else []
    return ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-n',
            '-ss', str(start_seconds), '-i', str(master), '-t', str(duration_seconds),
            '-map', '0:v:0', '-map', '0:a:0', '-vf', ','.join(filters),
            *encoder_arguments(variant), *container, str(output)]


def probe(path):
    completed = subprocess.run(['ffprobe', '-v', 'error', '-show_streams', '-show_format',
                                '-of', 'json', str(path)], capture_output=True, text=True, check=True)
    return json.loads(completed.stdout)


def verify(variant, path, duration_seconds):
    """Проверяет готовый файл по ffprobe; несоответствие — ошибка, а не предупреждение."""
    info = probe(path)
    videos = [s for s in info['streams'] if s['codec_type'] == 'video']
    audios = [s for s in info['streams'] if s['codec_type'] == 'audio']
    if len(videos) != 1 or len(audios) != 1:
        raise ValueError(f'{path}: expected one video and one audio stream')
    video = videos[0]
    profile = FFPROBE_PROFILE[(variant.codec, variant.profile)]
    actual = (video['codec_name'], video.get('profile'), video['width'], video['height'],
              video['r_frame_rate'], video['pix_fmt'])
    expected = (FFPROBE_CODEC.get(variant.codec, variant.codec), profile, variant.width, variant.height,
                f'{variant.fps}/1', variant.pixel_format)
    if actual != expected:
        raise ValueError(f'{path}: stream {actual} != expected {expected}')
    # Чересстрочность и HDR-метки — свойства потока, которые меняют путь плеера; проверяем их явно.
    if variant.interlaced and video.get('field_order') != 'tt':
        raise ValueError(f'{path}: field_order {video.get("field_order")!r} != top field first')
    if variant.hdr:
        color = {key: video.get(key) for key in HDR10_COLOR}
        if color != HDR10_COLOR:
            raise ValueError(f'{path}: color {color} != HDR10 {HDR10_COLOR}')
    duration = float(info['format']['duration'])
    if duration < duration_seconds - 0.5:
        raise ValueError(f'{path}: duration {duration:.2f}s shorter than {duration_seconds}s')
    # Битрейт видео считаем по сумме размеров пакетов: у WebM поле bit_rate потока пустое.
    # JSON, а не CSV: у MPEG-TS к строке пакета дописываются поля побочных данных.
    packets = json.loads(subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_entries',
                                         'packet=size', '-of', 'json', str(path)],
                                        capture_output=True, text=True, check=True).stdout)['packets']
    video_mbps = sum(int(packet['size']) for packet in packets) * 8 / duration / 1e6
    return {'scenario': variant.scenario, 'path': str(variant.relative_path), 'codec': variant.codec,
            'profile': variant.profile, 'width': variant.width, 'height': variant.height,
            'fps': variant.fps, 'scan': variant.scan, 'dynamic_range': variant.dynamic_range,
            'target_video_mbps': variant.video_mbps,
            'actual_video_mbps': round(video_mbps, 2), 'duration_seconds': round(duration, 3),
            'audio_codec': audios[0]['codec_name'], 'sha256': sha256(path)}


def master_rejection(variants, master_info):
    """Почему мастер не годится для выбранных сцен; ``None`` — годится.

    HDR10-сцену нельзя получить из SDR-мастера: метки PQ на кадрах выставляет сам
    генератор, поэтому проверка готового файла такую подмену не заметит — проверяем вход.
    """
    video = next(s for s in master_info['streams'] if s['codec_type'] == 'video')
    if any(variant.hdr for variant in variants) and video.get('color_transfer') != 'smpte2084':
        return f'HDR10 scenes need an HDR10 (PQ) master, got color_transfer={video.get("color_transfer")!r}'
    return None


def build_one(variant, arguments):
    output = arguments.output_root / variant.relative_path
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists() and arguments.force:
        output.unlink()
    if not output.exists():
        command = ffmpeg_command(variant, arguments.master, arguments.start, arguments.duration, output)
        completed = subprocess.run(command, capture_output=True, text=True)
        if completed.returncode:
            # Недописанный файл удаляется, чтобы следующий запуск не принял его за готовый.
            output.unlink(missing_ok=True)
            raise RuntimeError(f'{variant.relative_path}: ffmpeg exit {completed.returncode}: '
                               f'{completed.stderr.strip()[-2000:]}')
    entry = verify(variant, output, arguments.duration)
    print(f'{entry["path"]}: {entry["actual_video_mbps"]} Mbit/s, sha256 {entry["sha256"][:12]}', flush=True)
    return entry


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('master', type=Path,
                        help='4K60 мастер со звуком: 8-bit SDR, для пресета t410-hdr — 10-bit HDR10')
    parser.add_argument('output_root', type=Path, help='каталог матрицы (вне git)')
    parser.add_argument('--start', type=float, required=True, help='начало отрезка мастера, с')
    parser.add_argument('--duration', type=float, default=45, help='длина отрезка, с')
    parser.add_argument('--jobs', type=int, default=3, help='сколько файлов кодировать параллельно')
    parser.add_argument('--only', action='append', default=[], help='сцена codec-resolution; можно повторять')
    parser.add_argument('--force', action='store_true', help='перекодировать существующие файлы')
    parser.add_argument('--preset', choices=sorted(PRESETS), default='matrix', help='набор сцен')
    arguments = parser.parse_args()
    variants = [v for v in PRESETS[arguments.preset] if not arguments.only or v.scenario in arguments.only]
    if not variants:
        parser.error('no variants selected')
    master_info = probe(arguments.master)
    rejection = master_rejection(variants, master_info)
    if rejection:
        parser.error(rejection)
    manifest_path = arguments.output_root / 'manifest.json'
    segment = {'start_seconds': arguments.start, 'duration_seconds': arguments.duration}
    master_hash = sha256(arguments.master)
    # Частичный запуск (--only) дополняет манифест, а не стирает остальные сцены;
    # но смешивать файлы из разных мастеров или отрезков запрещено — проверяем до кодирования.
    previous = json.loads(manifest_path.read_text()) if manifest_path.exists() else None
    if previous and (previous['master_sha256'], previous['segment']) != (master_hash, segment):
        parser.error('existing manifest was built from another master or segment')
    entries, failures = [], []
    with ThreadPoolExecutor(max_workers=arguments.jobs) as pool:
        futures = {pool.submit(build_one, variant, arguments): variant for variant in variants}
        for future, variant in futures.items():
            try:
                entries.append(future.result())
            except (RuntimeError, ValueError, OSError, subprocess.SubprocessError) as error:
                failures.append(f'{variant.scenario}: {error}')
    merged = {entry['scenario']: entry for entry in (previous or {}).get('variants', [])}
    merged.update({entry['scenario']: entry for entry in entries})
    manifest = {'master_sha256': master_hash,
                'master_duration_seconds': float(master_info['format']['duration']),
                'segment': segment, 'variants': sorted(merged.values(), key=lambda entry: entry['scenario']),
                'failures': failures}
    write_json(manifest_path, manifest)
    for failure in failures:
        print('FAILED', failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == '__main__':
    raise SystemExit(main())
