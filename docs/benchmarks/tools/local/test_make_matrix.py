"""Функциональные проверки генератора матрицы на настоящем ffmpeg с крошечным мастером."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import make_matrix


@unittest.skipUnless(shutil.which('ffmpeg') and shutil.which('ffprobe'), 'ffmpeg/ffprobe required')
class MatrixGeneratorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.root = Path(cls.directory.name)
        # 2 с синтетического 1080p60 со звуком: мастер меньше реального, но с той же частотой.
        cls.master = cls.root / 'master.mp4'
        subprocess.run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=1920x1080:rate=60',
                        '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000', '-t', '2',
                        '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p', '-c:a', 'aac',
                        str(cls.master)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def frame_count(self, path):
        # JSON, а не CSV: у MPEG-TS поток печатается и внутри программы, и отдельно.
        output = subprocess.run(['ffprobe', '-v', 'error', '-count_frames', '-select_streams', 'v:0',
                                 '-show_entries', 'stream=nb_read_frames', '-of', 'json', str(path)],
                                capture_output=True, text=True, check=True).stdout
        return int(json.loads(output)['streams'][0]['nb_read_frames'])

    def run_generator(self, output_root, *only, preset='matrix', master=None):
        command = [sys.executable, str(Path(make_matrix.__file__)), str(master or self.master), str(output_root),
                   '--start', '0', '--duration', '1', '--jobs', '1', '--preset', preset]
        for scenario in only:
            command += ['--only', scenario]
        return subprocess.run(command, capture_output=True, text=True)

    def video_stream(self, path):
        return next(s for s in make_matrix.probe(path)['streams'] if s['codec_type'] == 'video')

    def test_t410_720p_variant_is_generated_with_its_own_size_and_rate(self):
        output_root = self.root / 't410-720p'
        completed = self.run_generator(output_root, 'h264-720p30', preset='t410')
        self.assertEqual(completed.returncode, 0, completed.stderr)
        entry = json.loads((output_root / 'manifest.json').read_text())['variants'][0]
        produced = output_root / entry['path']
        self.assertEqual((entry['width'], entry['height'], entry['target_video_mbps']), (1280, 720, 5))
        self.assertEqual(self.frame_count(produced), 30)
        stream = self.video_stream(produced)
        self.assertEqual((stream['width'], stream['height'], stream['r_frame_rate']), (1280, 720, '30/1'))

    def test_t410_mpeg2_576i_is_interlaced_top_field_first_pal_anamorphic(self):
        output_root = self.root / 't410-576i'
        completed = self.run_generator(output_root, 'mpeg2-576i25', preset='t410')
        self.assertEqual(completed.returncode, 0, completed.stderr)
        entry = json.loads((output_root / 'manifest.json').read_text())['variants'][0]
        self.assertEqual((entry['scan'], entry['audio_codec']), ('interlaced', 'mp2'))
        produced = output_root / entry['path']
        stream = self.video_stream(produced)
        # 1 с мастера 60p → 50 полей → 25 чересстрочных кадров 720×576 с SAR PAL 16:9.
        self.assertEqual((stream['codec_name'], stream['field_order'], stream['sample_aspect_ratio']),
                         ('mpeg2video', 'tt', '64:45'))
        self.assertEqual(self.frame_count(produced), 25)

    def test_progressive_mpeg2_is_rejected_for_an_interlaced_scene(self):
        progressive = self.root / 'progressive-576.ts'
        subprocess.run(['ffmpeg', '-v', 'error', '-y', '-i', str(self.master), '-t', '1', '-vf',
                        'fps=25,scale=720:576', '-c:v', 'mpeg2video', '-c:a', 'mp2', str(progressive)], check=True)
        scene = next(v for v in make_matrix.PRESETS['t410'] if v.scenario == 'mpeg2-576i25')
        with self.assertRaisesRegex(ValueError, 'field_order'):
            make_matrix.verify(scene, progressive, 1)

    def test_t410_hdr_scene_keeps_pq_tags_and_sdr_master_is_rejected(self):
        hdr_master = self.root / 'hdr-master.mp4'
        subprocess.run(['ffmpeg', '-v', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=size=1920x1080:rate=60',
                        '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000', '-t', '2',
                        '-vf', 'setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc',
                        '-c:v', 'libx265', '-preset', 'ultrafast', '-x265-params', 'log-level=error',
                        '-pix_fmt', 'yuv420p10le', '-c:a', 'aac', str(hdr_master)], check=True)
        output_root = self.root / 't410-hdr'
        completed = self.run_generator(output_root, preset='t410-hdr', master=hdr_master)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        entry = json.loads((output_root / 'manifest.json').read_text())['variants'][0]
        self.assertEqual((entry['scenario'], entry['dynamic_range']), ('hevc10-hdr-1080p30', 'hdr10'))
        stream = self.video_stream(output_root / entry['path'])
        self.assertEqual((stream['profile'], stream['color_transfer'], stream['color_primaries']),
                         ('Main 10', 'smpte2084', 'bt2020'))
        # 8-битный SDR-мастер не может дать HDR10-сцену: генератор обязан провалиться.
        rejected = self.run_generator(self.root / 't410-hdr-from-sdr', preset='t410-hdr')
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn('HDR10', rejected.stderr)

    def test_thirty_fps_variant_drops_every_second_frame_and_is_verified(self):
        output_root = self.root / 'thirty'
        completed = self.run_generator(output_root, 'h264-1080p30')
        self.assertEqual(completed.returncode, 0, completed.stderr)
        manifest = json.loads((output_root / 'manifest.json').read_text())
        entry = manifest['variants'][0]
        self.assertEqual((entry['scenario'], entry['fps'], entry['codec']), ('h264-1080p30', 30, 'h264'))
        produced = output_root / entry['path']
        self.assertEqual(self.frame_count(produced), 30)
        self.assertEqual(entry['sha256'], make_matrix.sha256(produced))
        self.assertGreater(entry['actual_video_mbps'], 0)

    def test_partial_runs_merge_manifest_and_wrong_file_is_rejected(self):
        output_root = self.root / 'merge'
        self.assertEqual(self.run_generator(output_root, 'h264-1080p30').returncode, 0)
        self.assertEqual(self.run_generator(output_root, 'vp9-1080p60').returncode, 0)
        manifest = json.loads((output_root / 'manifest.json').read_text())
        self.assertEqual([v['scenario'] for v in manifest['variants']], ['h264-1080p30', 'vp9-1080p60'])
        # Подмена: H.264-файл под именем VP9-сцены не проходит проверку ffprobe.
        vp9 = next(v for v in make_matrix.MATRIX if v.scenario == 'vp9-1080p60')
        h264 = next(v for v in make_matrix.MATRIX if v.scenario == 'h264-1080p30')
        with self.assertRaises(ValueError):
            make_matrix.verify(vp9, output_root / h264.relative_path, 1)

    def test_other_segment_cannot_be_merged_into_existing_manifest(self):
        output_root = self.root / 'segment'
        self.assertEqual(self.run_generator(output_root, 'h264-1080p30').returncode, 0)
        command = [sys.executable, str(Path(make_matrix.__file__)), str(self.master), str(output_root),
                   '--start', '0.5', '--duration', '1', '--only', 'h264-1080p60']
        completed = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn('another master or segment', completed.stderr)

    def test_matrix_covers_owner_grid_plus_main10(self):
        scenarios = {v.scenario for v in make_matrix.MATRIX}
        self.assertEqual(len(scenarios), 17)
        for codec in ('h264', 'hevc', 'av1', 'vp9'):
            for resolution in ('1080p30', '1080p60', '4k30', '4k60'):
                self.assertIn(f'{codec}-{resolution}', scenarios)
        main10 = next(v for v in make_matrix.MATRIX if v.bit_depth == 10)
        self.assertEqual((main10.scenario, main10.pixel_format), ('hevc10-4k60', 'yuv420p10le'))
        self.assertIs(make_matrix.PRESETS['matrix'], make_matrix.MATRIX)

    def test_t410_preset_covers_plan_scenes_with_unique_files(self):
        t410 = make_matrix.PRESETS['t410']
        # Обычные сцены, стресс-лесенка и MPEG-2 из файла сессии 00 плана gles2-renderer;
        # ступенька HEVC 1080p60 25 Мбит/с — решение владельца 68 (сессия 12a).
        self.assertEqual({v.scenario for v in t410}, {
            'h264-720p30', 'h264-720p60', 'h264-1080p30', 'h264-1080p60', 'h264-stress-1080p60',
            'hevc-stress-1080p60', 'hevc-720p30', 'hevc-1080p30', 'hevc-1080p60', 'vp9-720p30', 'vp9-720p60', 'vp9-1080p30',
            'vp9-1080p60', 'av1-720p30', 'av1-1080p30', 'av1-1080p60', 'mpeg2-576i25', 'mpeg2-1080i30'})
        self.assertEqual(len({v.relative_path for v in t410}), len(t410))
        normal, stress = (next(v for v in t410 if v.scenario == name) for name in ('h264-1080p60',
                                                                                  'h264-stress-1080p60'))
        self.assertLess(normal.video_mbps, stress.video_mbps)
        # HDR: обычная сцена и проба выше потолка T410 (решение владельца 68, сессия 12a).
        self.assertEqual({(v.scenario, v.profile, v.pixel_format) for v in make_matrix.PRESETS['t410-hdr']},
                         {('hevc10-hdr-1080p30', 'main10', 'yuv420p10le'),
                          ('hevc10-hdr-1080p60', 'main10', 'yuv420p10le')})


if __name__ == '__main__':
    unittest.main()
