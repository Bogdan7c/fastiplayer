//! Безопасное превращение «относительного URL» в путь внутри корня.
//!
//! Общий для каталога медиа и статики сайта: единственное место, где решается, что такое
//! «выход за корень». Правило простое и строгое — разрешены только обычные компоненты пути.

use std::path::{Component, Path, PathBuf};

/// Склеивает `root` и `relative`, если `relative` целиком состоит из обычных компонентов.
///
/// Отказ (`None`) для: пустого пути, `..`, `.`-сегментов, абсолютных путей, обратной косой
/// черты (на Windows это разделитель), символа NUL. Файловую систему не трогает.
pub(crate) fn join_inside(root: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() || relative.contains('\0') || relative.contains('\\') {
        return None;
    }
    let mut joined = root.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => joined.push(part),
            // `..`, `.`, корень и префикс диска — всё это выход из корня или неоднозначность.
            Component::ParentDir
            | Component::CurDir
            | Component::RootDir
            | Component::Prefix(_) => return None,
        }
    }
    (joined != root).then_some(joined)
}

/// Как [`join_inside`], но результат обязан быть существующим обычным файлом.
///
/// Дополнительно разыменовывает символические ссылки и убеждается, что настоящий путь всё
/// ещё лежит внутри настоящего корня (ссылка не должна вести наружу).
pub(crate) fn resolve_existing_file(root: &Path, relative: &str) -> Option<PathBuf> {
    let candidate = join_inside(root, relative)?;
    let real_candidate = candidate.canonicalize().ok()?;
    let real_root = root.canonicalize().ok()?;
    (real_candidate.starts_with(real_root) && real_candidate.is_file()).then_some(real_candidate)
}
