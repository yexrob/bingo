//! Visual review harness for the Slack skin: renders representative frames
//! into a `Buffer` and writes them out as HTML, one `<i>` per terminal cell
//! with an explicit `ch` advance, so a browser screenshot shows the exact
//! terminal grid (a browser's CJK metrics are not 2:1, and without the
//! per-cell advance the preview invents misalignment the terminal never has).
//!
//! Opt-in — set `BINGO_PREVIEW_DIR` to a directory and run
//! `cargo test shoot_it`; the normal suite writes nothing.

#[cfg(test)]
mod preview {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    use crate::agents::AgentState;
    use crate::channels::ChannelMode;
    use crate::tui::slack::*;
    use crate::tui::theme::Theme;
    use crate::tui::view;

    fn snap() -> Snapshot {
        Snapshot {
            workspace: "bingo".into(),
            channels: vec![
                ChannelItem {
                    name: "dev-room".into(),
                    seq: 6,
                    unread: 2,
                    frozen: false,
                    mode: ChannelMode::Serial,
                    members: vec!["main".into(), "user".into(), "scout".into(), "qa".into()],
                },
                ChannelItem {
                    name: "design".into(),
                    seq: 12,
                    unread: 0,
                    frozen: false,
                    mode: ChannelMode::Free,
                    members: vec!["main".into(), "user".into()],
                },
                ChannelItem {
                    name: "release".into(),
                    seq: 41,
                    unread: 0,
                    frozen: true,
                    mode: ChannelMode::Serial,
                    members: vec!["main".into()],
                },
            ],
            dms: vec![
                DmItem {
                    name: "scout".into(),
                    state: AgentState::Running,
                    description: "code reconnaissance".into(),
                    unread: 0,
                },
                DmItem {
                    name: "qa".into(),
                    state: AgentState::Idle,
                    description: "acceptance".into(),
                    unread: 3,
                },
                DmItem {
                    name: "ui-ux".into(),
                    state: AgentState::Stopped,
                    description: "interface review".into(),
                    unread: 0,
                },
            ],
        }
    }

    fn posts(now: u64) -> Vec<Post> {
        vec![
            Post {
                from: "scout".into(),
                you: false,
                at: now - 90_000,
                text: "I swept term.rs yesterday; only one spot left in the write layer.".into(),
                kind: PostKind::Said,
            },
            Post {
                from: "scout".into(),
                you: false,
                at: now - 3600,
                text: "Found the regression: after a resize the viewport height was one row short,\
                       so the hint at the bottom got pushed off."
                    .into(),
                kind: PostKind::Said,
            },
            Post {
                from: "scout".into(),
                you: false,
                at: now - 3580,
                text: "⏺ Read(src/tui/term.rs:410)".into(),
                kind: PostKind::Tool,
            },
            Post {
                from: "user".into(),
                you: true,
                at: now - 3000,
                text: "Don't change it yet; wait until I finish reading D38.".into(),
                kind: PostKind::Said,
            },
            Post {
                from: "qa".into(),
                you: false,
                at: now - 600,
                text: "Reproduced at 80x24; minimal steps attached.".into(),
                kind: PostKind::Said,
            },
            Post {
                from: "qa".into(),
                you: false,
                at: now - 60,
                text: "writing a regression test now".into(),
                kind: PostKind::Typing,
            },
        ]
    }

    fn frame(width: u16, height: u16, ws: &Workspace, theme: &Theme, now: u64) -> Buffer {
        let pal = Palette::new(theme);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let main = layout(area);
        let snap = snap();
        let h = height as usize;
        let w = main.width as usize;
        let conv = ws.open.clone().unwrap_or(Conv::Channel("dev-room".into()));
        let header = header_rows(&snap, &conv, &pal, w);
        let (composer, _) = composer_rows(ws, &conv, &pal, w);
        let viewport = h.saturating_sub(header.len() + composer.len()).max(1);
        // The text chip, not the portrait: a browser screenshot cannot show a
        // kitty placement, and a preview that silently dropped the gutter would
        // be measuring a layout the terminal never draws.
        let content = message_rows(&posts(now), 4, &pal, w, &Avatars::default());
        let start = content.len().saturating_sub(viewport);
        let mut slice: Vec<_> = content.iter().skip(start).cloned().collect();
        while slice.len() < viewport {
            slice.push(blank_row());
        }
        view::render_rows(&header, pal.main_text, &mut buf, main);
        let at = |buf: &mut Buffer, rows: &[crate::tui::chat::Row], off: u16| {
            view::render_rows(
                rows,
                pal.main_text,
                buf,
                Rect::new(main.x, main.y + off, main.width, main.height - off),
            );
        };
        at(&mut buf, &slice, header.len() as u16);
        at(&mut buf, &composer, (header.len() + viewport) as u16);
        if let Some(sw) = &ws.switcher {
            let (rows, _) = switcher_rows(&snap, sw, &pal, w);
            let top = header.len() + viewport.saturating_sub(rows.len() + 1);
            at(&mut buf, &rows, top as u16);
        }
        buf
    }

    /// The main transcript, for the sender bands (D50). Rendered in the chip skin
    /// for the same reason the workspace frames are: a browser cannot show a kitty
    /// placement, and faking one would be judging a layout the terminal never
    /// draws. What this *can* judge is the rest of the band — where the name sits,
    /// how the band reads against the user bubble below it and against the `⏺`
    /// markers inside a message.
    fn transcript_frame(width: u16, height: u16, theme: &Theme) -> Buffer {
        use crate::tui::activities::{Activity, ActivityKind, ToolCall, ToolStatus};
        use crate::tui::chat::{Role, UiMessage};

        let mut chat = crate::tui::test_util::chat_at(width as usize, height as usize);
        chat.theme = theme.clone();
        let msg = |role: Role, text: &str| UiMessage {
            role,
            text: text.to_string(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        };
        chat.messages
            .push(msg(Role::User, "why is the release workflow failing?"));
        let head = "The quality job fails before it reports a step: the lockfile carries \
                    four dependency pins that were rewritten alongside the version bump.";
        let tail = "Restoring them from the merge commit and letting cargo sync only the \
                    root version puts it back.";
        let mut assistant = msg(Role::Assistant, &format!("{head}\n\n{tail}"));
        // A tool between two prose segments: the shape the band has to coexist
        // with, and the reason `⏺` stayed — the second segment needs its own
        // marker or it reads as more tool output.
        let mut tool = Activity::new(ActivityKind::Tool(ToolCall::running(
            "Bash",
            "$ cargo metadata --locked",
        )));
        if let ActivityKind::Tool(t) = &mut tool.kind {
            t.status = ToolStatus::Done;
            t.result_summary = Some("4 pins restored".to_string());
        }
        assistant.activities.push(tool);
        assistant.insert_points.push(head.chars().count() + 2);
        assistant.group_of.push(None);
        chat.messages.push(assistant);
        chat.build_rows(width as usize);

        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view::render_rows(&chat.doc.rows, theme.text, &mut buf, area);
        buf
    }

    /// A cell colour, with `Reset` resolved to what the terminal would put there.
    /// This matters more than it looks: the view paints no background of its own
    /// any more, so almost every cell is `Reset` and a preview that invented a
    /// grey for it would be judging contrast against a colour no terminal shows.
    fn hex(c: Color, reset: &str) -> String {
        match c {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => reset.to_string(),
        }
    }

    /// Stand-in terminal backgrounds: a warm dark and a plain light one.
    const TERM_DARK: (&str, &str) = ("#1a1816", "#e6e0d8");
    const TERM_LIGHT: (&str, &str) = ("#faf8f5", "#1d1c1d");

    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// One buffer → an HTML block with per-run colours (screenshotted for
    /// visual review; the terminal is the real target but a browser is the only
    /// thing here that can be photographed).
    fn html(buf: &Buffer, title: &str, term: (&str, &str)) -> String {
        let (bg, fg) = term;
        let mut out = format!(
            "<h2>{}</h2><div class=\"scr\" style=\"background:{bg}\">",
            esc(title)
        );
        for y in buf.area.top()..buf.area.bottom() {
            out.push_str("<div class=\"r\">");
            let mut x = buf.area.left();
            while x < buf.area.right() {
                let cell = &buf[(x, y)];
                let w = crate::tui::line::text_width(cell.symbol()).max(1);
                // One span per cell with an explicit `ch` advance: a browser's
                // CJK metrics are not exactly 2:1, and without this the preview
                // invents misalignment the terminal grid never has.
                out.push_str(&format!(
                    "<i style=\"width:{w}ch;color:{};background:{}{}{}\">{}</i>",
                    hex(cell.fg, fg),
                    hex(cell.bg, bg),
                    if cell.modifier.contains(ratatui::style::Modifier::BOLD) {
                        ";font-weight:700"
                    } else {
                        ""
                    },
                    if cell.modifier.contains(ratatui::style::Modifier::ITALIC) {
                        ";font-style:italic"
                    } else {
                        ""
                    },
                    esc(cell.symbol())
                ));
                x += w as u16;
            }
            out.push_str("</div>");
        }
        out.push_str("</div>");
        out
    }

    /// Writes the preview page only when `BINGO_PREVIEW_DIR` points somewhere;
    /// a test that drops files into the filesystem on every run is a tool
    /// pretending to be a test.
    #[test]
    fn shoot_it() {
        let Ok(dir) = std::env::var("BINGO_PREVIEW_DIR") else {
            return;
        };
        unsafe { std::env::set_var("COLORTERM", "truecolor") };
        let now = 1_754_700_000u64;
        let mut ws = Workspace {
            open: Some(Conv::Channel("dev-room".into())),
            ..Workspace::default()
        };
        ws.entered = Some((Conv::Channel("dev-room".into()), 0));
        let typed = Workspace {
            composer: "then I'll extract the viewport height algorithm into a unit test first"
                .into(),
            focus: Focus::Composer,
            ..ws.clone()
        };
        let dm = Workspace {
            open: Some(Conv::Dm("qa".into())),
            focus: Focus::Messages,
            ..ws.clone()
        };
        let mut body = String::new();
        body.push_str(&html(
            &frame(100, 30, &ws, &Theme::dark(), now),
            "100×30 dark",
            TERM_DARK,
        ));
        body.push_str(&html(
            &frame(100, 30, &typed, &Theme::dark(), now),
            "100×30 dark · typing",
            TERM_DARK,
        ));
        body.push_str(&html(
            &frame(100, 30, &dm, &Theme::dark(), now),
            "100×30 dark · DM + messages focused",
            TERM_DARK,
        ));
        body.push_str(&html(
            &frame(100, 30, &ws, &Theme::light(), now),
            "100×30 light",
            TERM_LIGHT,
        ));
        body.push_str(&html(
            &frame(72, 24, &ws, &Theme::dark(), now),
            "72×24 dark",
            TERM_DARK,
        ));
        body.push_str(&html(
            &transcript_frame(100, 22, &Theme::dark()),
            "100×22 dark · main transcript, sender bands (chip skin)",
            TERM_DARK,
        ));
        body.push_str(&html(
            &transcript_frame(100, 22, &Theme::light()),
            "100×22 light · main transcript, sender bands (chip skin)",
            TERM_LIGHT,
        ));
        let switching = Workspace {
            switcher: Some(Switcher {
                query: "de".into(),
                sel: 1,
            }),
            ..ws.clone()
        };
        body.push_str(&html(
            &frame(100, 26, &switching, &Theme::dark(), now),
            "100×26 dark · ctrl+k quick jump",
            TERM_DARK,
        ));
        // Empty workspace: nothing spawned yet.
        {
            let pal = Palette::new(&Theme::dark());
            let area = Rect::new(0, 0, 100, 16);
            let mut buf = Buffer::empty(area);
            let main = layout(area);
            view::render_rows(
                &empty_pane_rows(&pal, main.width as usize, 16),
                pal.main_text,
                &mut buf,
                main,
            );
            body.push_str(&html(&buf, "100×16 dark · empty workspace", TERM_DARK));
        }
        let page = format!(
            "<html><head><meta charset=\"utf-8\"><style>\
             body{{background:#5a5a66;color:#ccc;font-family:-apple-system,sans-serif;padding:16px}}\
             h2{{font-size:13px;font-weight:600;color:#8a8a95;margin:22px 0 6px}}\
             .scr{{font:14px/1.35 Menlo,\"PingFang SC\",monospace;display:inline-block;\
             border-radius:6px;overflow:hidden}}\
             .r{{white-space:pre;height:1.35em}}\
             i{{display:inline-block;font-style:normal;overflow:hidden;\
             text-align:left;vertical-align:top}}</style></head><body>{body}</body></html>"
        );
        let path = std::path::Path::new(&dir).join("slack.html");
        std::fs::write(&path, page).unwrap_or_else(|e| panic!("write preview: {e}"));
        println!("preview: {}", path.display());
    }
}
