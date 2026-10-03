//! Terminal table cells measured in display columns, so wide characters
//! (CJK, emoji) and `…` line up the way a terminal draws them. `format!`
//! padding counts `char`s and is ignored by `Display` impls that call
//! `write_str`, so table code pads with [`cell`] instead of `{:<N}`.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display columns `text` takes in a terminal.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// `value` on one line with control characters removed, cut to at most
/// `max` display columns. A cut ends with `…` so truncation is visible.
pub fn fit(value: &str, max: usize) -> String {
    let clean: String = xcb_core::display_text(value, usize::MAX)
        .chars()
        .map(|ch| if ch == '\n' || ch == '\t' { ' ' } else { ch })
        .collect();
    if width(&clean) <= max {
        return clean;
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in clean.chars() {
        let columns = ch.width().unwrap_or(0);
        if used + columns + 1 > max {
            break;
        }
        out.push(ch);
        used += columns;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

/// `value` fitted to `columns` and padded with spaces to exactly `columns`.
pub fn cell(value: &str, columns: usize) -> String {
    let mut text = fit(value, columns);
    let pad = columns.saturating_sub(width(&text));
    text.extend(std::iter::repeat_n(' ', pad));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_pad_and_cut_by_display_columns() {
        assert_eq!(cell("abc", 5), "abc  ");
        assert_eq!(cell("abcdef", 4), "abc…");
        // Each CJK character takes two columns.
        assert_eq!(cell("日本語", 8), "日本語  ");
        assert_eq!(width(&cell("日本語テキスト", 7)), 7);
        assert_eq!(cell("日本語テキスト", 7), "日本語…");
        assert_eq!(cell("a\u{1b}[31mb\nc", 6), "a[31m…");
        assert_eq!(fit("", 3), "");
        assert_eq!(fit("anything", 0), "");
    }
}
