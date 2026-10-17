//! Идентичность по указателю: ключи кэшей ссылаются на значения других кэшей.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// `Arc<T>`, который сравнивается и хешируется ПО АДРЕСУ значения, а не по содержимому
/// (libass: ключи кэшей хранят указатели на значения нижележащих кэшей).
///
/// ## Почему это безопасно
/// Ключ держит сильную ссылку на значение, поэтому, пока ключ жив, адрес не может быть отдан
/// другому значению: два разных `ArcId`, живущих одновременно, всегда указывают на разные
/// значения, а два равных — на одно и то же. Побочное следствие (как и в libass): значение,
/// на которое ссылается ключ, остаётся в памяти, даже если его вытеснили из собственного кэша,
/// пока жив ключ.
#[derive(Debug)]
pub(crate) struct ArcId<T>(Arc<T>);

impl<T> ArcId<T> {
    /// Оборачивает общую ссылку (сама ссылка остаётся у вызывающего).
    pub(crate) fn new(value: &Arc<T>) -> Self {
        Self(Arc::clone(value))
    }
}

impl<T> Clone for ArcId<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> PartialEq for ArcId<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for ArcId<T> {}

impl<T> Hash for ArcId<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}
