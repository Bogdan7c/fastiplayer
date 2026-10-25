//! Замеры стенда: CPU потока и процесса, перцентили.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "стенд замера (пример, не production): отказ окружения прерывает прогон с \
              понятным сообщением"
)]

use std::time::Duration;

/// Тактов процессора в секунду для `/proc/self/stat` (на Linux x86_64 всегда 100).
const CLOCK_TICKS_PER_SECOND: f64 = 100.0;

/// CPU-время текущего потока (первое поле `/proc/thread-self/schedstat`, наносекунды).
pub fn thread_cpu_time() -> Duration {
    let schedstat = std::fs::read_to_string("/proc/thread-self/schedstat").expect("schedstat");
    let nanos = schedstat
        .split_whitespace()
        .next()
        .and_then(|field| field.parse().ok())
        .expect("schedstat cpu time");
    Duration::from_nanos(nanos)
}

/// CPU-время всего процесса (utime + stime из `/proc/self/stat`).
pub fn process_cpu_time() -> Duration {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
    // Имя процесса в скобках может содержать пробелы: поля считаем после последней `)`.
    let after_name = &stat[stat.rfind(')').expect("stat comm") + 1..];
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    // Относительно `state` (индекс 0): utime — 11, stime — 12.
    let ticks: u64 =
        fields[11].parse::<u64>().expect("utime") + fields[12].parse::<u64>().expect("stime");
    Duration::from_secs_f64(ticks as f64 / CLOCK_TICKS_PER_SECOND)
}

/// Перцентиль выборки в миллисекундах (выборка должна быть отсортирована).
pub fn percentile_ms(sorted: &[Duration], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[index].as_secs_f64() * 1000.0
}
