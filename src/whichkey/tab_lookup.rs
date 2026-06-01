//! Helpers for resolving `PaneManifest` tab keys.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabRef {
    pub position: usize,
    pub tab_id: usize,
}

pub fn resolve_tab_key(
    keys: &[usize],
    tabs: &[TabRef],
    position: usize,
    tab_id: Option<usize>,
) -> Option<usize> {
    let max_position = tabs.iter().map(|tab| tab.position).max().unwrap_or(0);
    let looks_like_tab_ids = !tabs.is_empty() && keys.iter().any(|key| *key > max_position);

    let direct = if looks_like_tab_ids {
        tab_id
            .and_then(|id| keys.iter().copied().find(|key| *key == id))
            .or_else(|| keys.iter().copied().find(|key| *key == position))
    } else {
        keys.iter()
            .copied()
            .find(|key| *key == position)
            .or_else(|| tab_id.and_then(|id| keys.iter().copied().find(|key| *key == id)))
    };

    direct.or_else(|| {
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        sorted.get(position).copied()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_position_keyed_manifest() {
        let keys = vec![0, 1, 2];
        let tabs = vec![
            TabRef {
                position: 0,
                tab_id: 7,
            },
            TabRef {
                position: 1,
                tab_id: 8,
            },
            TabRef {
                position: 2,
                tab_id: 9,
            },
        ];

        assert_eq!(resolve_tab_key(&keys, &tabs, 1, Some(8)), Some(1));
    }

    #[test]
    fn resolves_tab_id_keyed_manifest() {
        let keys = vec![0, 2, 3];
        let tabs = vec![
            TabRef {
                position: 0,
                tab_id: 0,
            },
            TabRef {
                position: 1,
                tab_id: 2,
            },
            TabRef {
                position: 2,
                tab_id: 3,
            },
        ];

        assert_eq!(resolve_tab_key(&keys, &tabs, 1, Some(2)), Some(2));
    }

    #[test]
    fn falls_back_to_sorted_ordinal_when_direct_lookup_fails() {
        let keys = vec![10, 20, 30];
        let tabs = vec![
            TabRef {
                position: 0,
                tab_id: 0,
            },
            TabRef {
                position: 1,
                tab_id: 1,
            },
            TabRef {
                position: 2,
                tab_id: 2,
            },
        ];

        assert_eq!(resolve_tab_key(&keys, &tabs, 1, Some(99)), Some(20));
    }
}
