//! Выбор одной строки сценария внутри страницы: переменная окружения процесса стенда.
//!
//! У некоторых команд на одной странице живёт несколько строк (ленты, реклама, серии,
//! перезагрузка, память выбора, возврат вкладки, режим 2), и `--page` выбирает их все.
//! Каждый такой сценарий с давних пор слушает свою переменную `BRIDGE_BENCH_*_ROW`: строка
//! идёт, если её имя содержит значение переменной (для ручной отладки: `pool`, `return-mse`).
//! Сценарии этой сессии (серии, перезагрузка, память выбора, реклама) понимают ещё точный
//! отбор: значение с `=` впереди (`=delayed-next`) берёт строку ровно с этим именем — иначе
//! `delayed-next` захватил бы и `delayed-next-hidden`.
//! Быстрый набор (сессия 16b3c2) запускает каждую строку отдельным процессом стенда и
//! ставит этому процессу нужную переменную через [`RowFilter`] — так выбор строки
//! типизирован, а имя переменной знает только её сценарий.

/// Отбор строк одного сценария: имя переменной процесса и часть имени строки.
#[derive(Debug, Clone, Copy)]
pub struct RowFilter {
    variable: &'static str,
    part: &'static str,
    /// Каталог имён всех строк сценария (те же имена, что видит отбор).
    catalog: fn() -> Vec<String>,
    /// Как сценарий читает значение: старые (ленты, режим 2, возврат вкладки) — только
    /// подстрока, сценарии 16b3c2 — ещё и точное имя через `=`.
    matching: RowMatching,
}

/// Как сценарий сравнивает значение переменной с именем строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowMatching {
    /// Имя строки содержит значение.
    Substring,
    /// Как `Substring`, но `=имя` — ровно эта строка.
    SubstringOrExact,
}

impl RowFilter {
    /// Строки лент `capture` (по `step_id`, например `feed-pool`).
    pub const fn feed(part: &'static str) -> Self {
        Self::new(
            super::capture::FEED_ROW_FILTER_VARIABLE,
            part,
            super::capture::feed_row_names,
            RowMatching::Substring,
        )
    }

    /// Строки «реклама → фильм» `capture` (по `step_id`, например `ad-preroll`).
    pub const fn ad_hop(part: &'static str) -> Self {
        Self::new(
            super::capture::AD_HOP_ROW_FILTER_VARIABLE,
            part,
            super::capture::ad_hop_row_names,
            RowMatching::SubstringOrExact,
        )
    }

    /// Строки режима 2 (`auto-handoff`, по имени строки).
    pub const fn auto_handoff(part: &'static str) -> Self {
        Self::new(
            super::auto_handoff::SUBJECT_FILTER_VARIABLE,
            part,
            super::auto_handoff::row_names,
            RowMatching::Substring,
        )
    }

    /// Строки возврата вкладки (`tab-return`, по имени строки).
    pub const fn tab_return(part: &'static str) -> Self {
        Self::new(
            super::tab_return::SUBJECT_FILTER_VARIABLE,
            part,
            super::tab_return::row_names,
            RowMatching::Substring,
        )
    }

    /// Строки пауз между сериями (`episodes`, по имени строки).
    pub const fn episodes(part: &'static str) -> Self {
        Self::new(
            super::episodes::ROW_FILTER_VARIABLE,
            part,
            super::episodes::row_names,
            RowMatching::SubstringOrExact,
        )
    }

    /// Строки перезагрузки страницы (`page-reload`, по имени строки).
    pub const fn page_reload(part: &'static str) -> Self {
        Self::new(
            super::page_reload::ROW_FILTER_VARIABLE,
            part,
            super::page_reload::row_names,
            RowMatching::SubstringOrExact,
        )
    }

    /// Строки памяти выбора (`series-memory`: `restart`, `incognito`, `autoplay`).
    pub const fn series_memory(part: &'static str) -> Self {
        Self::new(
            super::series_memory::ROW_FILTER_VARIABLE,
            part,
            super::series_memory::row_names,
            RowMatching::SubstringOrExact,
        )
    }

    const fn new(
        variable: &'static str,
        part: &'static str,
        catalog: fn() -> Vec<String>,
        matching: RowMatching,
    ) -> Self {
        Self {
            variable,
            part,
            catalog,
            matching,
        }
    }

    /// Переменная окружения и её значение для процесса стенда, который гоняет строку.
    pub fn env_pair(self) -> (&'static str, &'static str) {
        (self.variable, self.part)
    }

    /// Пройдёт ли строка с именем `row_name`, если процессу поставлен этот фильтр.
    pub fn admits(self, row_name: &str) -> bool {
        match self.matching {
            RowMatching::Substring => row_name.contains(self.part),
            RowMatching::SubstringOrExact => admits(Some(self.part), row_name),
        }
    }

    /// Какие строки каталога сценария этот фильтр пропускает: проверка, что отбор быстрого
    /// набора выбирает ровно задуманную строку, а не пусто и не две.
    pub fn matching_rows(self) -> Vec<String> {
        (self.catalog)()
            .into_iter()
            .filter(|row_name| self.admits(row_name))
            .collect()
    }
}

/// Префикс точного отбора: `=имя` — только строка с этим именем.
const EXACT_PREFIX: char = '=';

/// Пройдёт ли строка `row_name` при значении переменной `filter` (`None` — переменной нет,
/// идут все строки). Чистая часть [`row_admitted_by_environment`].
pub(crate) fn admits(filter: Option<&str>, row_name: &str) -> bool {
    filter.is_none_or(|value| match value.strip_prefix(EXACT_PREFIX) {
        Some(exact) => row_name == exact,
        None => row_name.contains(value),
    })
}

/// Пройдёт ли строка `row_name` по переменной `variable` этого процесса.
pub(crate) fn row_admitted_by_environment(variable: &str, row_name: &str) -> bool {
    admits(std::env::var(variable).ok().as_deref(), row_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_filter_every_row_runs() {
        assert!(admits(None, "delayed-next"));
    }

    #[test]
    fn an_exact_filter_keeps_only_the_row_with_that_name() {
        assert!(admits(Some("=delayed-next"), "delayed-next"));
        assert!(!admits(Some("=delayed-next"), "delayed-next-hidden"));
        assert!(!admits(Some("=next"), "delayed-next"));
    }

    #[test]
    fn a_filter_keeps_rows_whose_name_contains_it() {
        assert!(admits(Some("autoplay"), "series-memory-autoplay"));
        assert!(!admits(Some("autoplay"), "series-memory-restart"));
    }

    #[test]
    fn a_filter_finds_its_rows_in_the_scenario_catalog() {
        assert_eq!(
            RowFilter::series_memory("autoplay").matching_rows(),
            ["series-memory-autoplay"]
        );
        assert_eq!(
            RowFilter::episodes("delayed-next").matching_rows(),
            ["delayed-next", "delayed-next-hidden"],
            "a part matches every row that contains it"
        );
        assert_eq!(
            RowFilter::episodes("=delayed-next").matching_rows(),
            ["delayed-next"],
            "an exact filter matches the one row with that name"
        );
        assert!(
            RowFilter::page_reload("no-such-row")
                .matching_rows()
                .is_empty()
        );
        // Ленты читают только подстроку: `=feed-pool` там не значит «ровно».
        assert!(RowFilter::feed("=feed-pool").matching_rows().is_empty());
    }

    #[test]
    fn each_scenario_names_its_own_variable() {
        let variables = [
            RowFilter::feed("x"),
            RowFilter::ad_hop("x"),
            RowFilter::auto_handoff("x"),
            RowFilter::tab_return("x"),
            RowFilter::episodes("x"),
            RowFilter::page_reload("x"),
            RowFilter::series_memory("x"),
        ]
        .map(|filter| filter.env_pair().0);
        for (index, variable) in variables.iter().enumerate() {
            assert!(variable.starts_with("BRIDGE_BENCH_"), "{variable}");
            assert!(
                !variables[index + 1..].contains(variable),
                "{variable} is shared by two scenarios"
            );
        }
    }
}
