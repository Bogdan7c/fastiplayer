#!/usr/bin/env python3
"""Генерирует два synthetic SDR fixtures; существующие файлы не перезаписываются."""

import argparse
from pathlib import Path
import subprocess


def generate_fixture(output, render_node, codec):
    """Каждый fixture содержит moving test pattern и stereo sine audio."""
    if output.exists():
        raise FileExistsError(f'fixture already exists: {output.name}')
    hevc = codec == 'hevc'
    source_size = '960x540' if hevc else '1920x1080'
    filters = 'format=nv12,hwupload'
    if hevc:
        # Это простой upscaled pattern, не эквивалент natural 4K complexity.
        filters += ',scale_vaapi=w=3840:h=2160'
    command = [
        'ffmpeg', '-hide_banner', '-nostdin', '-n', '-vaapi_device', render_node,
        '-f', 'lavfi', '-i', f'testsrc2=size={source_size}:rate=60',
        '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
        '-t', '85', '-vf', filters, '-c:v', f'{codec}_vaapi',
        '-profile:v', 'main' if hevc else 'high', '-qp', '24', '-g', '120',
        '-c:a', 'aac', '-b:a', '128k', '-ac', '2',
        '-color_primaries', 'bt709', '-color_trc', 'bt709', '-colorspace', 'bt709',
        '-movflags', '+faststart', str(output),
    ]
    subprocess.run(command, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--render-node', default='/dev/dri/renderD128')
    arguments = parser.parse_args()
    arguments.directory.mkdir(parents=True, exist_ok=True)
    generate_fixture(arguments.directory / 'synthetic-h264-1080p60.mp4', arguments.render_node, 'h264')
    generate_fixture(arguments.directory / 'synthetic-hevc-4k60.mp4', arguments.render_node, 'hevc')


if __name__ == '__main__':
    main()
