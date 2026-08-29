//! What a permission prompt is about, drawn: a diff, a command, or a url.
//! Bounded by default so the question and its options stay on the screen;
//! ctrl+e lifts the bound.

use bingo_sdk::Preview;
use ratatui::text::{Line, Span};

use crate::theme;

/// A long diff would push the options off the screen.
const DIFF_ROWS: usize = 12;
/// Heredocs and `&&` chains run long.
const COMMAND_ROWS: usize = 6;

/// The preview's rows, and how many were left out.
pub fn lines(preview: &Preview, expanded: bool) -> (Vec<Line<'static>>, usize) {
    match preview {
        Preview::Diff { unified } => bound(diff(unified), DIFF_ROWS, expanded),
        Preview::Command { command, cwd } => {
            let mut rows = vec![Line::from(Span::styled(cwd.clone(), theme::dim()))];
            rows.extend(command.lines().map(command_line));
            bound(rows, COMMAND_ROWS + 1, expanded)
        }
        Preview::Url { url } => (
            vec![Line::from(Span::styled(url.clone(), theme::accent()))],
            0,
        ),
    }
}

/// A unified diff, coloured by what each line does to the file.
pub fn diff(unified: &str) -> Vec<Line<'static>> {
    unified.lines().map(diff_line).collect()
}

fn diff_line(line: &str) -> Line<'static> {
    let style = match line.as_bytes().first() {
        _ if line.starts_with("@@") => theme::accent(),
        _ if line.starts_with("+++") || line.starts_with("---") => theme::dim(),
        Some(b'+') => theme::good(),
        Some(b'-') => theme::danger(),
        _ => theme::dim(),
    };
    Line::from(Span::styled(line.to_string(), style))
}

fn command_line(line: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("$ ", theme::dim()),
        Span::raw(line.to_string()),
    ])
}

fn bound(rows: Vec<Line<'static>>, limit: usize, expanded: bool) -> (Vec<Line<'static>>, usize) {
    if expanded || rows.len() <= limit {
        return (rows, 0);
    }
    let hidden = rows.len() - limit;
    (rows.into_iter().take(limit).collect(), hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unified(rows: usize) -> String {
        (0..rows)
            .map(|i| format!("+line {i}\n"))
            .collect::<String>()
    }

    #[test]
    fn a_long_diff_is_bounded_until_it_is_expanded() {
        let preview = Preview::Diff {
            unified: unified(20),
        };
        let (rows, hidden) = lines(&preview, false);
        assert_eq!((rows.len(), hidden), (DIFF_ROWS, 8));
        let (rows, hidden) = lines(&preview, true);
        assert_eq!((rows.len(), hidden), (20, 0));
    }

    #[test]
    fn diff_rows_take_their_colour_from_the_first_column() {
        let rows = diff("@@ -1 +1 @@\n-old\n+new\n context");
        let styles: Vec<_> = rows.iter().map(|r| r.spans[0].style).collect();
        assert_eq!(
            styles,
            vec![
                theme::accent(),
                theme::danger(),
                theme::good(),
                theme::dim()
            ]
        );
    }

    #[test]
    fn a_command_preview_shows_its_directory_and_the_line() {
        let (rows, _) = lines(
            &Preview::Command {
                command: "ls -la".into(),
                cwd: "/tmp".into(),
            },
            false,
        );
        assert_eq!(
            rows.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            vec!["/tmp", "$ ls -la"]
        );
    }
}
