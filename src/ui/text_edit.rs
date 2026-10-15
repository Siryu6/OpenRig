//! Text edits of the on-screen keyboard, at the caret. Offsets are UTF-8
//! byte offsets, as Slint's TextInput reports them.

/// Clamp `offset` into `text`, back to the start of the character it falls in
fn boundary(text: &str, offset: i32) -> usize {
    let mut offset = (offset.max(0) as usize).min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Replace the bytes from `start` to `end` with `insert`. Returns the new
/// text and the caret, just after the inserted text.
pub fn replace(text: &str, start: i32, end: i32, insert: &str) -> (String, usize) {
    let (start, end) = (boundary(text, start), boundary(text, end));
    let (start, end) = (start.min(end), start.max(end));
    let edited = format!("{}{insert}{}", &text[..start], &text[end..]);
    (edited, start + insert.len())
}

/// Erase the bytes from `start` to `end`, or the character before `start`
/// when they are equal (Backspace). Returns the new text and the caret.
pub fn erase(text: &str, start: i32, end: i32) -> (String, usize) {
    let (start, end) = (boundary(text, start), boundary(text, end));
    let (start, end) = (start.min(end), start.max(end));
    let start = if start == end {
        text[..start]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    } else {
        start
    };
    replace(text, start as i32, end as i32, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_at_the_caret() {
        assert_eq!(replace("helo", 3, 3, "l"), ("hello".into(), 4));
        assert_eq!(replace("", 0, 0, "a"), ("a".into(), 1));
        // A selection is replaced, in either direction
        assert_eq!(replace("hello", 4, 1, "a"), ("hao".into(), 2));
        // Offsets past the end, or inside a character, are clamped
        assert_eq!(replace("ab", 9, 9, "c"), ("abc".into(), 3));
        assert_eq!(replace("é", 1, 1, "x"), ("xé".into(), 1));
    }

    #[test]
    fn erases_before_the_caret() {
        assert_eq!(erase("hello", 2, 2), ("hllo".into(), 1));
        assert_eq!(erase("hello", 0, 0), ("hello".into(), 0));
        assert_eq!(erase("hello", 1, 4), ("ho".into(), 1));
        assert_eq!(erase("aé", 3, 3), ("a".into(), 1));
    }
}
