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
        output = subprocess.run(['ffprobe', '-v', 'error', '-count_frames', '-select_streams', 'v:0',
                                 '-show_entries', 'stream=nb_read_frames', '-of', 'csv=p=0', str(path)],
                                capture_output=True, text=True, check=True).stdout
        return int(output.strip())

    def run_generator(self, output_root, *only):
        command = [sys.executable, str(Path(make_matrix.__file__)), str(self.master), str(output_root),
                   '--start', '0', '--duration', '1', '--jobs', '1']
        for scenario in only:
            command += ['--only', scenario]
        return subprocess.run(command, capture_output=True, text=True)

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


if __name__ == '__main__':
    unittest.main()
