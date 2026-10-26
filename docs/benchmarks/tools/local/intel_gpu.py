"""Глобальная загрузка GPU через ``intel_gpu_top`` — диагностическая колонка (протокол v4).

На Intel Gen5 (T410, i915) fdinfo не отдаёт ``drm-engine-*``, поэтому занятость GPU
процесса не измерить. ``intel_gpu_top`` даёт ГЛОБАЛЬНУЮ занятость движков (все процессы,
включая композитор). Это описательное число: оно никогда не складывается с CPU и не
участвует в вердиктах. Колонка включается только в diagnostic-попытках и только при
``"intel_gpu_top": true`` в ``common``.

``sudo -n`` не должен падать серией: нет прав или нет утилиты — колонка ``NOT TESTED`` с
причиной, попытка остаётся валидной.
"""

import json
from pathlib import Path
import subprocess

# Период сэмплов, мс (-s). Раз в секунду, как интервал замера CPU.
SAMPLE_PERIOD_MS = 1000
COMMAND = ['sudo', '-n', 'intel_gpu_top', '-J', '-s', str(SAMPLE_PERIOD_MS)]
# Движки, по которым берём среднее: ключ отчёта -> имя движка в JSON (прибавка «/N» у
# новых версий утилиты — номер экземпляра — допускается).
ENGINES = {'render_3d_busy_percent': 'Render/3D', 'video_busy_percent': 'Video'}
STOP_WAIT_SECONDS = 5


def parse_stream(text):
    """Объекты потока ``intel_gpu_top -J``: JSON-значения подряд, возможно в массиве ``[ … ]``.

    Убитая по таймауту утилита оставляет последний объект обрезанным: разбор идёт
    ``raw_decode`` по очереди и на первой неразбираемой части останавливается молча
    (обрезанный хвост — ожидаемое состояние, а не ошибка), цельные объекты сохраняются.
    """
    decoder, position, objects = json.JSONDecoder(), 0, []
    while True:
        while position < len(text) and text[position] in ' \t\r\n,[]':
            position += 1
        if position >= len(text):
            return objects
        try:
            value, position = decoder.raw_decode(text, position)
        except json.JSONDecodeError:
            return objects
        if isinstance(value, dict):
            objects.append(value)


def _engine_busy(sample, base_name):
    """``busy`` движка ``base_name`` (или ``base_name/N``) в одном сэмпле; нет — None."""
    for name, engine in sample.get('engines', {}).items():
        if name == base_name or name.startswith(base_name + '/'):
            return engine.get('busy')
    return None


def summarize(objects):
    """Средние занятости движков по сэмплам окна; первый сэмпл отбрасывается.

    Первый сэмпл intel_gpu_top считан за неполный интервал и занижен. Меньше двух
    сэмплов или нет ни одного нужного движка — ``NOT TESTED`` с причиной, а не 0.
    """
    samples = objects[1:]
    if not samples:
        return not_tested(f'{len(objects)} intel_gpu_top sample(s): need at least 2 (first is discarded)')
    result = {'status': 'MEASURED', 'scope': 'GLOBAL GPU load of all processes; descriptive only, '
                                             'never added to CPU, not used in verdicts',
              'samples': len(samples)}
    for key, engine in ENGINES.items():
        values = [busy for busy in (_engine_busy(sample, engine) for sample in samples)
                  if isinstance(busy, (int, float))]
        result[key] = sum(values) / len(values) if values else None
    if all(result[key] is None for key in ENGINES):
        return not_tested('intel_gpu_top output has no Render/3D or Video engine')
    return result


def not_tested(reason):
    return {'status': 'NOT TESTED', 'reason': reason}


class IntelGpuTop:
    """Один запуск ``intel_gpu_top`` на окно замера: ``start()`` до окна, ``stop()`` после."""

    def __init__(self, output_path, command=None):
        self.output_path = Path(output_path)
        self.command = command or COMMAND
        self.process = None
        self.stream = None
        self.start_failure = None

    def start(self):
        """Запускает утилиту; невозможность запуска — не ошибка попытки, а причина NOT TESTED."""
        try:
            self.stream = self.output_path.open('w')
            self.process = subprocess.Popen(self.command, stdout=self.stream, stderr=subprocess.PIPE, text=True)
        except OSError as error:
            self.start_failure = f'cannot start {self.command[0]}: {error}'
            if self.stream:
                self.stream.close()

    def stop(self):
        """Останавливает утилиту и возвращает сводку (:func:`summarize`) или NOT TESTED с причиной."""
        if self.start_failure:
            return not_tested(self.start_failure)
        if self.process.poll() is None:  # уже вышла (sudo отказал) — не трогаем, нужен её код и stderr
            self.process.terminate()  # sudo пробрасывает SIGTERM дочернему intel_gpu_top
        try:
            _, stderr = self.process.communicate(timeout=STOP_WAIT_SECONDS)
        except subprocess.TimeoutExpired:
            self.process.kill()
            _, stderr = self.process.communicate()
        self.stream.close()
        objects = parse_stream(self.output_path.read_text(errors='replace'))
        if not objects:
            reason = (stderr or '').strip() or f'no output (exit {self.process.returncode})'
            return not_tested(f'intel_gpu_top produced no samples: {reason}')
        return summarize(objects)
