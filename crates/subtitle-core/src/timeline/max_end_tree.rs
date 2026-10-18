//! Неявное дерево максимумов времени окончания.
//!
//! Дерево строится поверх массива событий, отсортированного по началу. Каждый узел хранит
//! максимальный `end` в своём поддереве. Это позволяет при запросе «что активно в момент T»
//! отбрасывать целые поддеревья, в которых все события уже закончились (`max_end <= T`),
//! и не просматривать их по одному.

use crate::time::SubtitleTime;

/// Побитовое дерево отрезков (power-of-two layout): листья лежат в `nodes[leaf_count..]`,
/// потомки узла `k` — это `2k` и `2k + 1`, корень — `1`.
#[derive(Debug, Clone, Default)]
pub(super) struct MaxEndTree {
    nodes: Vec<SubtitleTime>,
    leaf_count: usize,
    /// Реальное число элементов; хвост листьев (padding) заполнен `PADDING_END`.
    element_count: usize,
}

/// Значение для листьев-«пустышек»: никогда не больше любого запрошенного времени.
const PADDING_END: SubtitleTime = SubtitleTime::from_micros(i64::MIN);

impl MaxEndTree {
    /// Строит дерево за O(n) из окончаний в порядке сортировки по началу.
    ///
    /// Число элементов ограничено владельцем (`MAX_TIMELINE_EVENTS`), поэтому
    /// степень двойки всегда помещается в `usize`.
    pub(super) fn build(ends: impl ExactSizeIterator<Item = SubtitleTime>) -> Self {
        let element_count = ends.len();
        let leaf_count = element_count.max(1).next_power_of_two();
        let mut nodes = vec![PADDING_END; leaf_count.saturating_mul(2)];
        for (position, end) in ends.enumerate() {
            if let Some(leaf) = nodes.get_mut(leaf_count.saturating_add(position)) {
                *leaf = end;
            }
        }
        // Внутренние узлы заполняются снизу вверх: максимум из двух потомков.
        for node in (1..leaf_count).rev() {
            let left = node_value(&nodes, node.saturating_mul(2));
            let right = node_value(&nodes, node.saturating_mul(2).saturating_add(1));
            if let Some(slot) = nodes.get_mut(node) {
                *slot = left.max(right);
            }
        }
        Self {
            nodes,
            leaf_count,
            element_count,
        }
    }

    /// Заменяет окончание в позиции `position` и пересчитывает предков за O(log n).
    ///
    /// Возвращает `false`, если позиции нет (ничего не изменено).
    pub(super) fn set_end(&mut self, position: usize, end: SubtitleTime) -> bool {
        if position >= self.element_count {
            return false;
        }
        let mut node = self.leaf_count.saturating_add(position);
        match self.nodes.get_mut(node) {
            Some(leaf) => *leaf = end,
            None => return false,
        }
        // Поднимаемся к корню: каждый предок — максимум из двух потомков.
        while node > 1 {
            node /= 2;
            let left = node_value(&self.nodes, node.saturating_mul(2));
            let right = node_value(&self.nodes, node.saturating_mul(2).saturating_add(1));
            if let Some(slot) = self.nodes.get_mut(node) {
                *slot = left.max(right);
            }
        }
        true
    }

    /// Вызывает `visit(position)` для каждой позиции из префикса `[0, prefix_len)`,
    /// у которой `end > time`, в порядке возрастания позиции.
    ///
    /// Поддеревья с `max_end <= time` и целиком за префиксом пропускаются, поэтому
    /// цена — O((k + 1) · log n), где k — число найденных позиций.
    pub(super) fn visit_positions_ending_after(
        &self,
        time: SubtitleTime,
        prefix_len: usize,
        visit: &mut impl FnMut(usize),
    ) {
        if self.element_count == 0 {
            return;
        }
        let prefix_len = prefix_len.min(self.element_count);
        self.visit_node(1, 0, self.leaf_count, time, prefix_len, visit);
    }

    fn visit_node(
        &self,
        node: usize,
        range_start: usize,
        range_len: usize,
        time: SubtitleTime,
        prefix_len: usize,
        visit: &mut impl FnMut(usize),
    ) {
        // Диапазон целиком за префиксом или все события в нём уже закончились.
        if range_start >= prefix_len || node_value(&self.nodes, node) <= time {
            return;
        }
        if range_len == 1 {
            visit(range_start);
            return;
        }
        let half = range_len / 2;
        let left_child = node.saturating_mul(2);
        self.visit_node(left_child, range_start, half, time, prefix_len, visit);
        self.visit_node(
            left_child.saturating_add(1),
            range_start.saturating_add(half),
            half,
            time,
            prefix_len,
            visit,
        );
    }
}

/// Безопасное чтение узла: несуществующий узел ведёт себя как padding.
fn node_value(nodes: &[SubtitleTime], node: usize) -> SubtitleTime {
    nodes.get(node).copied().unwrap_or(PADDING_END)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros(values: &[i64]) -> Vec<SubtitleTime> {
        values
            .iter()
            .copied()
            .map(SubtitleTime::from_micros)
            .collect()
    }

    fn query(tree: &MaxEndTree, time: i64, prefix_len: usize) -> Vec<usize> {
        let mut found = Vec::new();
        tree.visit_positions_ending_after(SubtitleTime::from_micros(time), prefix_len, &mut |p| {
            found.push(p);
        });
        found
    }

    #[test]
    fn empty_tree_visits_nothing() {
        let tree = MaxEndTree::build(Vec::<SubtitleTime>::new().into_iter());
        assert_eq!(query(&tree, 0, 10), Vec::<usize>::new());
    }

    #[test]
    fn visits_only_positions_ending_strictly_after_time() {
        let tree = MaxEndTree::build(micros(&[10, 50, 20, 50, 5]).into_iter());
        assert_eq!(query(&tree, 20, 5), vec![1, 3]);
        assert_eq!(query(&tree, 4, 5), vec![0, 1, 2, 3, 4]);
        assert_eq!(query(&tree, 50, 5), Vec::<usize>::new());
    }

    #[test]
    fn set_end_updates_ancestors_and_rejects_unknown_position() {
        let mut tree = MaxEndTree::build(micros(&[100, 50, 70]).into_iter());
        assert!(tree.set_end(0, SubtitleTime::from_micros(10)));
        assert_eq!(query(&tree, 60, 3), vec![2]);
        assert_eq!(query(&tree, 5, 3), vec![0, 1, 2]);
        assert!(!tree.set_end(3, SubtitleTime::from_micros(999)));
        assert_eq!(query(&tree, 60, 3), vec![2]);
    }

    #[test]
    fn prefix_limits_visited_positions() {
        let tree = MaxEndTree::build(micros(&[100, 100, 100, 100]).into_iter());
        assert_eq!(query(&tree, 0, 2), vec![0, 1]);
        assert_eq!(query(&tree, 0, 0), Vec::<usize>::new());
        // Префикс длиннее массива не выходит за реальные элементы (padding не виден).
        assert_eq!(query(&tree, i64::MIN, 99), vec![0, 1, 2, 3]);
    }
}
