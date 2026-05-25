/// Move `selected` down by one, clamping at `len.saturating_sub(1)`.
pub fn select_next(selected: &mut usize, len: usize) {
    if len == 0 {
        return;
    }
    *selected = (*selected + 1).min(len - 1);
}

/// Move `selected` up by one, saturating at 0.
pub fn select_prev(selected: &mut usize) {
    *selected = selected.saturating_sub(1);
}

/// Jump to the first item.
pub fn select_first(selected: &mut usize) {
    *selected = 0;
}

/// Jump to the last item.
pub fn select_last(selected: &mut usize, len: usize) {
    if len == 0 {
        return;
    }
    *selected = len - 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_clamps_at_last() {
        let mut s = 0;
        select_next(&mut s, 3);
        assert_eq!(s, 1);
        select_next(&mut s, 3);
        assert_eq!(s, 2);
        select_next(&mut s, 3);
        assert_eq!(s, 2);
    }

    #[test]
    fn next_noop_on_empty() {
        let mut s = 0;
        select_next(&mut s, 0);
        assert_eq!(s, 0);
    }

    #[test]
    fn prev_saturates_at_zero() {
        let mut s = 2;
        select_prev(&mut s);
        assert_eq!(s, 1);
        select_prev(&mut s);
        assert_eq!(s, 0);
        select_prev(&mut s);
        assert_eq!(s, 0);
    }

    #[test]
    fn first_and_last() {
        let mut s = 1;
        select_first(&mut s);
        assert_eq!(s, 0);
        select_last(&mut s, 5);
        assert_eq!(s, 4);
    }

    #[test]
    fn last_noop_on_empty() {
        let mut s = 0;
        select_last(&mut s, 0);
        assert_eq!(s, 0);
    }
}
