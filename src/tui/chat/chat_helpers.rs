use super::{Line, Row, Theme};
use crate::tui::line::{SegStyle, wrap_words};

pub(crate) fn one_line(text: &str, width: usize) -> String {
    let flat = crate::tui::line::sanitize(text);
    crate::tui::markdown::truncate(flat.as_ref(), width.max(1))
}

pub(crate) fn user_message_rows(text: &str, width: usize, theme: &Theme) -> Vec<Row> {
    // 2 prefix columns + 1 column of right padding inside the bubble.
    let body_width = width.saturating_sub(3).max(1);
    let style = SegStyle::fg(theme.text);
    wrap_words(text, body_width)
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let mut line = Line::styled(if i == 0 { "❯ " } else { "  " }, style);
            line.push_styled(text, style);
            Row::bubble(line, theme.user_message_bg)
        })
        .collect()
}

pub(crate) fn text_rows(theme: &Theme, reply: Vec<Line>) -> Vec<Row> {
    let claude = theme.claude;
    reply
        .into_iter()
        .enumerate()
        .map(|(j, line)| {
            if j == 0 {
                let mut styled = Line::styled("⏺ ", SegStyle::fg(claude));
                styled.image = line.image.clone();
                styled.segs.extend(line.segs);
                Row::new(styled)
            } else {
                Row::new(line)
            }
        })
        .collect()
}
