//! Единая project-owned граница чтения недоверенного XML.
//!
//! Crate принимает только уже полученный byte slice, поэтому сам не открывает
//! файлы, URL или external entities. Форматные владельцы XSPF, DASH, ISM и HDS
//! получают namespace-resolved события и самостоятельно валидируют свои schema.

// Budget module владеет caller-defined ограничениями и их complete builder-ом.
mod budget;
// Event module владеет parser-neutral XML vocabulary для будущих domain parser-ов.
mod event;
// Error module не выпускает наружу quick-xml types или unbounded input fragments.
mod error;
// Reader module применяет budgets и security policy до публикации каждого события.
mod reader;

// Публичный API budget-ов собран в facade, чтобы callers не зависели от layout модулей.
pub use budget::{MissingXmlBudget, XmlBudgetKind, XmlBudgets, XmlBudgetsBuilder};
// Публичные события не раскрывают concrete parser implementation.
pub use event::{XmlAttribute, XmlElement, XmlEvent, XmlExpandedName, XmlText};
// Typed error сохраняет важные различия policy, malformed input и exhausted budgets.
pub use error::XmlReadError;
// BoundedXmlReader является единственным production entry point этого crate.
pub use reader::BoundedXmlReader;
