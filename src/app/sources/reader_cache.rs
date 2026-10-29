use std::{collections::BTreeMap, sync::Arc};
pub(super) const COMPRESSED_LIMIT: usize = 16 * 1024 * 1024;
#[derive(Default)]
pub(super) struct Reader {
    pub compressed: BTreeMap<usize, Arc<Vec<u8>>>,
    pub decoded: BTreeMap<usize, ()>,
    pub ratios: Vec<f32>,
    pub target: usize,
    pub fetched: std::collections::HashSet<usize>,
    pub failed: std::collections::HashSet<usize>,
}
impl Reader {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn target(&mut self, page: usize, count: usize) {
        if page == 0 {
            self.target = self.target.max(2);
        } else if page < 4 {
            self.target = self.target.max(4);
        } else if page + 1 >= self.target {
            self.target = self.target.max(page + 6);
        }
        self.target = self.target.min(count);
    }
    pub fn insert(&mut self, index: usize, bytes: Arc<Vec<u8>>, current: usize) {
        self.fetched.insert(index);
        self.compressed.insert(index, bytes);
        while self.compressed.values().map(|v| v.len()).sum::<usize>() > COMPRESSED_LIMIT {
            let victim = self
                .compressed
                .keys()
                .filter(|&&i| i != current)
                .max_by_key(|&&i| i.abs_diff(current))
                .copied();
            if let Some(i) = victim {
                self.compressed.remove(&i);
            } else {
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lookahead_advances_in_batches_and_clamps() {
        let mut r = Reader::default();
        r.target(0, 40);
        assert_eq!(r.target, 2);
        r.target(1, 40);
        assert_eq!(r.target, 4);
        r.target(4, 40);
        assert_eq!(r.target, 10);
        r.target(5, 40);
        assert_eq!(r.target, 10);
        r.target(9, 40);
        assert_eq!(r.target, 15);
        r.target(14, 17);
        assert_eq!(r.target, 17);
    }
    #[test]
    fn compressed_cache_stays_bounded_without_repeated_prefetch() {
        let mut r = Reader::default();
        for i in 0..8 {
            r.insert(i, Arc::new(vec![0; 3 * 1024 * 1024]), 0);
        }
        assert!(r.compressed.values().map(|v| v.len()).sum::<usize>() <= COMPRESSED_LIMIT);
        assert!(r.compressed.contains_key(&0));
        assert_eq!(r.fetched.len(), 8);
    }
}
