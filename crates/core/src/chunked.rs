//! An append-only list stored in shared fixed-size chunks, so that a grown
//! copy shares everything but the last partial chunk with the original. The
//! index publishes a new snapshot per batch while indexing; copying whole
//! lists of 100k+ frame records per batch would be quadratic.

use std::ops::Index;
use std::sync::Arc;

const CHUNK: usize = 4096;

#[derive(Debug)]
pub struct Chunked<T> {
    /// All chunks hold `CHUNK` items except possibly the last.
    chunks: Vec<Arc<[T]>>,
    len: usize,
}

impl<T> Clone for Chunked<T> {
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.clone(),
            len: self.len,
        }
    }
}

impl<T> Default for Chunked<T> {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Chunked<T> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, i: usize) -> Option<&T> {
        (i < self.len).then(|| &self.chunks[i / CHUNK][i % CHUNK])
    }

    pub fn last(&self) -> Option<&T> {
        self.len.checked_sub(1).and_then(|i| self.get(i))
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + '_ {
        self.chunks.iter().flat_map(|c| c.iter())
    }

    /// Items `from..` in order.
    pub fn iter_from(&self, from: usize) -> impl Iterator<Item = &T> + '_ {
        (from..self.len).map(move |i| &self[i])
    }

    /// Items `..=to` in reverse order.
    pub fn iter_rev_to(&self, to: usize) -> impl Iterator<Item = &T> + '_ {
        let end = if self.len == 0 {
            0
        } else {
            to.min(self.len - 1) + 1
        };
        (0..end).rev().map(move |i| &self[i])
    }
}

impl<T: Clone> Chunked<T> {
    /// Appends `items`. Only the last partial chunk is copied.
    pub fn extend(&mut self, items: impl IntoIterator<Item = T>) {
        let mut tail: Vec<T> = match self.chunks.last() {
            Some(c) if c.len() < CHUNK => {
                let tail = c.to_vec();
                self.chunks.pop();
                tail
            }
            _ => Vec::new(),
        };
        for item in items {
            tail.push(item);
            self.len += 1;
            if tail.len() == CHUNK {
                self.chunks.push(Arc::from(std::mem::take(&mut tail)));
            }
        }
        if !tail.is_empty() {
            self.chunks.push(Arc::from(tail));
        }
    }
}

impl<T> Index<usize> for Chunked<T> {
    type Output = T;

    fn index(&self, i: usize) -> &T {
        assert!(i < self.len, "index {i} out of bounds (len {})", self.len);
        &self.chunks[i / CHUNK][i % CHUNK]
    }
}

impl<T: Clone> FromIterator<T> for Chunked<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut c = Self::default();
        c.extend(iter);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_across_chunks_and_shares_full_ones() {
        let mut a: Chunked<u32> = (0..CHUNK as u32 + 10).collect();
        let snapshot = a.clone();
        a.extend(CHUNK as u32 + 10..3 * CHUNK as u32);
        assert_eq!(a.len(), 3 * CHUNK);
        assert!((0..a.len()).all(|i| a[i] == i as u32));
        assert!(a.iter().copied().eq(0..3 * CHUNK as u32));
        // The full first chunk is shared, the snapshot is unchanged.
        assert!(Arc::ptr_eq(&a.chunks[0], &snapshot.chunks[0]));
        assert_eq!(snapshot.len(), CHUNK + 10);
        assert_eq!(snapshot.get(CHUNK + 10), None);
        assert!(a.iter_rev_to(5).copied().eq((0..=5).rev()));
        assert!(a
            .iter_from(3 * CHUNK - 2)
            .copied()
            .eq([3 * CHUNK as u32 - 2, 3 * CHUNK as u32 - 1]));
    }
}
