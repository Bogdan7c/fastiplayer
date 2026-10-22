//! Обход ошибки Vulkan WSI драйвера Mesa на Wayland: видео замирает после паузы.
//!
//! Mesa (проверено на 26.2.4 + KWin) в режиме FIFO задаёт каждому кадру целевое время
//! показа через протокол `wp_commit_timing_v1` («предыдущая цель + период обновления»).
//! На паузе плеер кадры не рисует — так и задумано, это экономит CPU. После
//! продолжения Mesa сначала берёт устаревшие цели из цепочки до паузы, видит, что
//! кадр показан «с опозданием» на длину паузы, и сдвигает следующую цель на столько же
//! вперёд. Композитор честно держит этот коммит и наш frame callback до этого
//! времени: видео стоит примерно столько же, сколько стояла пауза
//! (доказательство протоколом — `user/cpu-parity/results/07a.md`).
//!
//! Отладочное слово `nowlts` в `MESA_VK_WSI_DEBUG` отключает только целевые метки
//! времени: кадры по-прежнему темпирует `wp_fifo_v1` (vsync), как до появления commit
//! timing в Mesa. Переменная читается драйвером при создании Vulkan-ресурсов, поэтому
//! выставляется до создания окна и рендерера. Другие драйверы и X11 её не читают.
//! Если Mesa когда-нибудь уберёт это слово, оно просто перестанет действовать.

use std::ffi::{OsStr, OsString};

/// Переменная окружения отладочных флагов Vulkan WSI в Mesa.
const MESA_WSI_DEBUG_VARIABLE: &str = "MESA_VK_WSI_DEBUG";

/// Флаг Mesa «не отправлять Wayland timestamps» (`wp_commit_timing_v1`).
const DISABLE_WAYLAND_COMMIT_TIMING_FLAG: &str = "nowlts";

/// Разделитель, которым флаг дописывается к уже заданным пользователем флагам.
const MESA_DEBUG_FLAG_SEPARATOR: &str = ",";

/// Итог применения обхода: что стало с `MESA_VK_WSI_DEBUG`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MesaCommitTimingWorkaround {
    /// Переменной не было: записан только наш флаг.
    Enabled,
    /// У пользователя были свои флаги: наш дописан после них.
    AppendedToUserFlags,
    /// Флаг уже стоял (пользователь задал сам): окружение не тронуто.
    AlreadyPresent,
}

/// Новое значение переменной вместе с тем, как оно получено.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MesaWsiDebugUpdate {
    Write {
        value: OsString,
        outcome: MesaCommitTimingWorkaround,
    },
    Keep,
}

/// Отключает Wayland commit timing в Mesa для этого процесса.
///
/// Вызывается ПЕРВЫМ в `main`, пока в процессе один поток: изменение окружения при
/// других потоках — гонка с их чтением `getenv` (поэтому `set_var` в Rust 2024 unsafe).
pub(crate) fn disable_mesa_wayland_commit_timing() -> MesaCommitTimingWorkaround {
    let current_value = std::env::var_os(MESA_WSI_DEBUG_VARIABLE);
    match mesa_wsi_debug_with_commit_timing_disabled(current_value.as_deref()) {
        MesaWsiDebugUpdate::Keep => MesaCommitTimingWorkaround::AlreadyPresent,
        MesaWsiDebugUpdate::Write { value, outcome } => {
            // SAFETY: вызывается первой строкой `main` до инициализации tracing, создания
            // event loop-а и любых потоков; одновременного чтения окружения быть не может.
            unsafe { std::env::set_var(MESA_WSI_DEBUG_VARIABLE, value) };
            outcome
        }
    }
}

/// Решает, каким должно стать значение `MESA_VK_WSI_DEBUG`, не трогая окружение.
fn mesa_wsi_debug_with_commit_timing_disabled(current_value: Option<&OsStr>) -> MesaWsiDebugUpdate {
    let Some(current_value) = current_value.filter(|value| !value.is_empty()) else {
        return MesaWsiDebugUpdate::Write {
            value: OsString::from(DISABLE_WAYLAND_COMMIT_TIMING_FLAG),
            outcome: MesaCommitTimingWorkaround::Enabled,
        };
    };
    if contains_debug_flag(current_value, DISABLE_WAYLAND_COMMIT_TIMING_FLAG) {
        return MesaWsiDebugUpdate::Keep;
    }
    // Значение может быть не UTF-8: дописываем к исходным байтам, а не к их копии.
    let mut value = current_value.to_os_string();
    value.push(MESA_DEBUG_FLAG_SEPARATOR);
    value.push(DISABLE_WAYLAND_COMMIT_TIMING_FLAG);
    MesaWsiDebugUpdate::Write {
        value,
        outcome: MesaCommitTimingWorkaround::AppendedToUserFlags,
    }
}

/// Есть ли флаг среди слов списка. Mesa делит список запятыми и пробелами.
fn contains_debug_flag(flags: &OsStr, flag: &str) -> bool {
    flags
        .to_string_lossy()
        .split([',', ' '])
        .any(|listed_flag| listed_flag == flag)
}

#[cfg(test)]
mod tests;
