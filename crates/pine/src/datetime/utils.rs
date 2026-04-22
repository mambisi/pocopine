//! Port of reka's `utils.ts`.

/// Split a slice into contiguous sub-vectors of length `size`.
/// The trailing chunk may be shorter when `slice.len()` isn't a
/// multiple of `size`.
pub fn chunk<T: Clone>(slice: &[T], size: usize) -> Vec<Vec<T>> {
    if size == 0 {
        return Vec::new();
    }
    slice
        .chunks(size)
        .map(|c| c.to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_evenly() {
        assert_eq!(
            chunk(&[1, 2, 3, 4, 5, 6], 3),
            vec![vec![1, 2, 3], vec![4, 5, 6]]
        );
    }

    #[test]
    fn trailing_remainder() {
        assert_eq!(
            chunk(&[1, 2, 3, 4, 5, 6, 7, 8], 3),
            vec![vec![1, 2, 3], vec![4, 5, 6], vec![7, 8]]
        );
    }

    #[test]
    fn size_zero_is_empty() {
        assert!(chunk(&[1, 2, 3], 0).is_empty());
    }
}
