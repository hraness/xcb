//! Soft rows keep editing positions in the original draft; wrapping never adds newlines.
use ratatui::buffer::CellWidth;
use ratatui_textarea::{DataCursor, TextArea};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) struct Glyph {
    pub text: String,
    pub source: (usize, usize),
    pub x: usize,
}

pub(crate) struct Layout {
    pub rows: Vec<Vec<Glyph>>,
    pub cursor: (usize, usize),
}

pub(crate) fn layout(textarea: &TextArea<'_>, width: u16) -> Layout {
    let width = usize::from(width.max(1));
    let DataCursor(cursor_row, cursor_col) = textarea.cursor();
    let mut rows = vec![Vec::new()];
    let mut cursor = (0, 0);
    for (row, line) in textarea.lines().iter().enumerate() {
        if row > 0 {
            rows.push(Vec::new());
        }
        let mut x = 0;
        let mut col = 0;
        let mut glyphs = line.graphemes(true).peekable();
        let mut word_start = true;
        while let Some(grapheme) = glyphs.next() {
            if word_start && !grapheme.chars().all(char::is_whitespace) {
                let word_cells = usize::from(grapheme.cell_width())
                    + glyphs
                        .clone()
                        .take_while(|next| !next.chars().all(char::is_whitespace))
                        .map(|next| usize::from(next.cell_width()))
                        .take(width)
                        .sum::<usize>();
                if x > 0 && word_cells <= width && x + word_cells > width {
                    rows.push(Vec::new());
                    x = 0;
                }
            }
            word_start = grapheme.chars().all(char::is_whitespace);
            let text = if grapheme == "\t" {
                " ".repeat(4 - x % 4)
            } else {
                xcb_core::display_text(grapheme, 256)
            };
            let cells = usize::from(text.cell_width());
            if x > 0 && x + cells > width {
                rows.push(Vec::new());
                x = 0;
            }
            if (row, col) == (cursor_row, cursor_col) {
                cursor = (rows.len() - 1, x);
            }
            // Extremely narrow terminals cannot paint half a wide glyph.
            let text = if cells > width { " ".into() } else { text };
            rows.last_mut().expect("row exists").push(Glyph {
                text,
                source: (row, col),
                x,
            });
            x += cells.min(width);
            col += grapheme.chars().count();
        }
        if x >= width && row + 1 < textarea.lines().len() {
            if (row, col) == (cursor_row, cursor_col) {
                cursor = (rows.len() - 1, width - 1);
            }
            continue;
        }
        if x >= width {
            rows.push(Vec::new());
            x = 0;
        }
        if (row, col) == (cursor_row, cursor_col) {
            cursor = (rows.len() - 1, x);
        }
        rows.last_mut().expect("row exists").push(Glyph {
            text: " ".into(),
            source: (row, col),
            x,
        });
    }
    Layout { rows, cursor }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_textarea::CursorMove;

    #[test]
    fn wraps_without_mutating_text_and_keeps_wide_glyphs_whole() {
        let mut text = TextArea::from(["abcd界e"]);
        text.move_cursor(CursorMove::End);
        let view = layout(&text, 5);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.rows[1][0].text, "界");
        assert_eq!(view.cursor, (1, 3));
        assert_eq!(text.lines(), &["abcd界e"]);
    }

    #[test]
    fn exact_width_allocates_a_caret_row_and_resize_reflows() {
        let mut text = TextArea::from(["abcdefgh"]);
        text.move_cursor(CursorMove::End);
        assert_eq!(layout(&text, 4).cursor, (2, 0));
        assert_eq!(layout(&text, 9).cursor, (0, 8));
    }

    #[test]
    fn explicit_newline_after_exact_width_does_not_add_a_blank_row() {
        let mut text = TextArea::from(["abcd", "ef"]);
        text.move_cursor(CursorMove::Bottom);
        text.move_cursor(CursorMove::End);
        let view = layout(&text, 4);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.cursor, (1, 2));
    }

    #[test]
    fn empty_lines_tabs_and_combining_clusters_keep_source_coordinates() {
        let mut text = TextArea::from(["a\t界e\u{301}", ""]);
        text.move_cursor(CursorMove::Bottom);
        assert_eq!(layout(&text, 6).cursor, (2, 0));
        let view = layout(&text, 6);
        assert_eq!(view.rows[1][1].source, (0, 3));
        assert_eq!(view.rows[1][1].text, "e\u{301}");
    }
}
