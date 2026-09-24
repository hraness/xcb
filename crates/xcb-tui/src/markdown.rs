//! A deliberately small terminal Markdown renderer. Input is sanitized before
//! parsing, links remain visible text, and inline scanning has a fixed budget.
//! This does not interpret HTML, terminal escapes, or executable link actions.
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::{clean, expand_tabs, muted};

const INLINE_SCAN_BYTES: usize = 8 * 1024;
const INLINE_SPANS: usize = 128;

pub(super) fn lines(text: &str) -> Vec<Line<'static>> {
    let text = clean(text);
    let mut out = Vec::new();
    let mut fence: Option<(char, usize, bool)> = None;
    for raw in text.lines() {
        let line = expand_tabs(raw);
        let trimmed = line.trim_start();
        let indent = line.len().saturating_sub(trimmed.len());
        let marker = trimmed.chars().next().filter(|ch| matches!(ch, '`' | '~'));
        let run = marker.map_or(0, |ch| {
            trimmed.chars().take_while(|next| *next == ch).count()
        });
        if let Some((ch, length, diff)) = fence {
            if indent <= 3
                && marker == Some(ch)
                && run >= length
                && trimmed[run..].trim().is_empty()
            {
                fence = None;
                continue;
            }
            let style = if diff && (line.starts_with('+') || line.starts_with('-')) {
                Style::default().fg(if line.starts_with('+') {
                    Color::Green
                } else {
                    Color::Red
                })
            } else if diff && line.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            out.push(Line::from(vec![
                Span::styled("  ", muted()),
                Span::styled(line, style),
            ]));
            continue;
        }
        if indent <= 3 && run >= 3 {
            let language = trimmed[run..].trim();
            fence = Some((
                marker.expect("a fence marker"),
                run,
                matches!(language, "diff" | "patch"),
            ));
            out.push(Line::from(Span::styled(
                if language.is_empty() {
                    "  code".to_owned()
                } else {
                    format!("  {language}")
                },
                muted(),
            )));
            continue;
        }
        let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if indent <= 3 && (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ')
        {
            let heading = trimmed[hashes + 1..].trim_end();
            let without_hashes = heading.trim_end_matches('#');
            let heading = if without_hashes
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
            {
                without_hashes.trim_end()
            } else {
                heading
            };
            out.push(Line::from(inline(
                heading,
                Style::default().add_modifier(Modifier::BOLD),
            )));
        } else if let Some(body) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled(format!("{}│ ", " ".repeat(indent)), muted())];
            spans.extend(inline(body, muted()));
            out.push(Line::from(spans));
        } else if let Some(body) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let mut spans = vec![Span::styled(format!("{}• ", " ".repeat(indent)), muted())];
            spans.extend(inline(body, Style::default()));
            out.push(Line::from(spans));
        } else if matches!(trimmed, "---" | "***" | "___") {
            out.push(Line::from(Span::styled("───", muted())));
        } else {
            // Numbered lists intentionally retain their numbering and indent.
            out.push(Line::from(inline(&line, Style::default())));
        }
    }
    out
}

fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut index = 0;
    let mut plain = String::new();
    let mut search_budget = INLINE_SCAN_BYTES;
    while index < text.len()
        && index < INLINE_SCAN_BYTES
        && spans.len() < INLINE_SPANS
        && search_budget > 0
    {
        let tail = &text[index..];
        // Limit each delimiter search too: malformed markup cannot make a
        // long line quadratic, nor allocate one span for every punctuation.
        let mut end = (INLINE_SCAN_BYTES - index)
            .min(tail.len())
            .min(search_budget);
        while !tail.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = &tail[..end];
        let paired = [
            ("**", Modifier::BOLD),
            ("__", Modifier::BOLD),
            ("`", Modifier::empty()),
            ("*", Modifier::ITALIC),
            ("_", Modifier::ITALIC),
        ];
        let mut consumed = 0;
        for (delimiter, modifier) in paired {
            if let Some(rest) = candidate.strip_prefix(delimiter) {
                let close = rest.find(delimiter);
                search_budget = search_budget.saturating_sub(
                    close.map_or(rest.len(), |position| position + delimiter.len()),
                );
                if let Some(close) = close
                    && close > 0
                    && (delimiter != "_"
                        || !text[..index]
                            .chars()
                            .next_back()
                            .is_some_and(char::is_alphanumeric))
                {
                    flush(&mut spans, &mut plain, base);
                    let style = if delimiter == "`" {
                        base.fg(Color::Cyan)
                    } else {
                        base.add_modifier(modifier)
                    };
                    spans.push(Span::styled(rest[..close].to_owned(), style));
                    consumed = delimiter.len() * 2 + close;
                    break;
                }
            }
        }
        if consumed == 0
            && let Some(rest) = candidate.strip_prefix('[')
        {
            let close = rest.find("](");
            search_budget =
                search_budget.saturating_sub(close.map_or(rest.len(), |position| position + 2));
            if let Some(close) = close {
                let end = rest[close + 2..].find(')');
                search_budget = search_budget
                    .saturating_sub(end.map_or(rest.len() - close - 2, |position| position + 1));
                if let Some(end) = end {
                    let label = &rest[..close];
                    let target = &rest[close + 2..close + 2 + end];
                    flush(&mut spans, &mut plain, base);
                    spans.push(Span::styled(
                        label.to_owned(),
                        base.add_modifier(Modifier::UNDERLINED),
                    ));
                    spans.push(Span::styled(format!(" ({target})"), muted()));
                    consumed = close + end + 4;
                }
            }
        }
        if consumed > 0 {
            index += consumed;
        } else {
            let ch = tail.chars().next().expect("nonempty tail");
            plain.push(ch);
            index += ch.len_utf8();
        }
    }
    plain.push_str(&text[index..]);
    flush(&mut spans, &mut plain, base);
    spans
}

fn flush(spans: &mut Vec<Span<'static>>, plain: &mut String, style: Style) {
    if !plain.is_empty() {
        spans.push(Span::styled(std::mem::take(plain), style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markdown_retains_code_links_and_incomplete_streams() {
        let rendered = lines(
            "# Title\n- **bold** and `code`\n[docs](https://example.test)\n```diff\n-old\n+new\n@@ hunk\n```\nunfinished **text",
        );
        assert!(
            rendered[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(rendered[1].to_string().contains("• bold and code"));
        assert_eq!(rendered[2].to_string(), "docs (https://example.test)");
        assert!(
            rendered[2].spans[0]
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert_eq!(rendered[4].spans[1].style.fg, Some(Color::Red));
        assert_eq!(rendered[5].spans[1].style.fg, Some(Color::Green));
        assert_eq!(rendered.last().unwrap().to_string(), "unfinished **text");
        assert_eq!(lines("# C#")[0].to_string(), "C#");
        assert_eq!(lines("## Title ##")[0].to_string(), "Title");
    }
    #[test]
    fn inline_work_and_control_characters_are_bounded() {
        let text = format!("\u{1b}[31m{}", "[**あ".repeat(100_000));
        let rendered = lines(&text);
        assert!(rendered[0].spans.len() <= INLINE_SPANS + 2);
        let text = rendered.iter().map(Line::to_string).collect::<String>();
        assert!(!text.contains('\u{1b}'));
        assert!(text.len() < 270_000);
    }
}
