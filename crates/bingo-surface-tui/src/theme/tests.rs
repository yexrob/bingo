use super::*;

fn ascii() -> Theme {
    Theme {
        colors: Colors::Ansi,
        glyphs: &ASCII,
    }
}

fn answered(reply: &[u8]) -> Option<bool> {
    let (r, g, b) = answered_ground(reply)?;
    Some(background_is_light(r, g, b))
}

/// Every drawing source of this crate, with its test module cut off, as
/// `scripts/check_discipline.sh` cuts them. This file, the fixtures and
/// the screen catalogue are left out: one holds the table and the others
/// only read it back.
fn sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    read(std::path::Path::new("src"), &mut out);
    out
}

fn read(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("the crate's own sources") {
        let path = entry.expect("a readable entry").path();
        if path.is_dir() {
            read(&path, out);
            continue;
        }
        // Joined by `/` on every platform: the names below are spelled so.
        let name = path
            .strip_prefix("src")
            .unwrap_or(&path)
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if path.extension().is_none_or(|e| e != "rs")
            || name.starts_with("theme/")
            || name.starts_with("test_support/")
            || name.ends_with("_tests.rs")
            || matches!(
                name.as_str(),
                "doubles.rs"
                        // The kitty protocol carries a picture's id in the
                        // foreground colour (M46): a number in a colour's
                        // clothing, and the one place outside this table
                        // that has to spell one.
                        | "graphics/kitty.rs"
                        // Sparks blend with the already-painted ground.
                        | "transcript/sparks.rs"
                        | "welcome/embers.rs"
                        | "identity.rs"
                        | "crew_contract.rs"
                        | "motion.rs"
                        | "motion/landing.rs"
                        | "painted.rs"
                        | "screens.rs"
                        | "screens/colours.rs"
                        | "test_support.rs"
                        | "tests.rs"
                        | "theme.rs"
            )
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable source");
        let code = text.split("#[cfg(test)]\nmod ").next().unwrap_or_default();
        out.push((name, code.to_string()));
    }
}

#[test]
fn tints_follow_the_ground_without_recolouring_prose_or_accents() {
    let ask = Ask {
        truecolor: true,
        ..Ask::default()
    };
    let reference = detect(ask, Some(reference_ground(false)));
    assert_eq!(reference.colors, Colors::True(DARK));
    let changed = detect(ask, Some((0x3030, 0x3030, 0x3030)));
    let Colors::True(palette) = changed.colors else {
        panic!("truecolour")
    };
    assert_ne!(palette.faint(), DARK.faint());
    assert_ne!(palette.add_tint(), DARK.add_tint());
    assert_eq!(palette.ember(), DARK.ember());
    with(changed, || {
        assert_eq!(text().fg, Some(Color::Reset));
        assert_eq!(dim().fg, Some(palette.dim()));
    });
    let indexed = Ask {
        truecolor: false,
        indexed: true,
        ..ask
    };
    assert!(follows(indexed));
    with(detect(indexed, Some(reference_ground(true))), || {});
}

#[test]
fn chrome_has_light_pairs_and_fixed_xterm_nearest_colours() {
    for (dark, light) in [
        (DARK.faint(), LIGHT.faint()),
        (DARK.muted(), LIGHT.muted()),
        (DARK.code(), LIGHT.code()),
        (DARK.well(), LIGHT.well()),
        (DARK.pick(), LIGHT.pick()),
    ] {
        assert_ne!(dark, light);
    }
    assert_eq!(nearest(Color::Rgb(95, 135, 175)), Color::Indexed(67));
    assert_eq!(nearest(Color::Rgb(128, 128, 128)), Color::Indexed(244));
    for palette in [DARK, LIGHT] {
        with(
            Theme {
                colors: Colors::Indexed(palette),
                glyphs: &UNICODE,
            },
            || {
                assert!(matches!(presence().fg, Some(Color::Indexed(_))));
                assert_eq!(text().fg, Some(Color::Reset));
                assert_eq!(dim().fg, Some(nearest(palette.dim())));
            },
        );
    }
}

#[test]
fn no_view_names_a_colour_of_its_own() {
    let leaked: Vec<String> = sources()
        .into_iter()
        .flat_map(|(name, code)| {
            code.lines()
                .filter(|line| line.contains("Color::") || line.contains("Modifier::"))
                .map(|line| format!("{name}: {}", line.trim()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "a colour outside the token table: {leaked:#?}"
    );
}

/// The table of §4, and the only places each token is spent. A new call
/// site is a taste decision, so it is written down here before it is made.
#[test]
fn every_token_is_spent_where_the_design_says() {
    let allowed: &[(&str, &[&str])] = &[
        (
            "text",
            &[
                "activity.rs",
                "browse/reader.rs",
                "composer/drawing.rs",
                "dialog.rs",
                "form.rs",
                "highlight.rs",
                "markdown.rs",
                "pill.rs",
                "preview.rs",
                "search.rs",
                "transcript.rs",
                "transcript/band.rs",
                "transcript/notice.rs",
                "transcript/post.rs",
                "transcript/room.rs",
                "transcript/said.rs",
                "transcript/slots.rs",
                "views/actions.rs",
                "views/diff.rs",
                "views/input.rs",
                "views/keyvalue.rs",
                "views/list.rs",
                "views/panel.rs",
                "views/progress.rs",
                "views/select.rs",
                "views/text.rs",
                "views/tree.rs",
                "welcome/drawing.rs",
            ],
        ),
        (
            "dim",
            &[
                "activity.rs",
                "ask/body.rs",
                "ask/receipts.rs",
                "component/live.rs",
                "component/retry.rs",
                "dialog.rs",
                "form.rs",
                "highlight.rs",
                "layers.rs",
                "layout/grid.rs",
                "markdown.rs",
                "pane/draw.rs",
                "preview.rs",
                "queue_rows.rs",
                "search.rs",
                "transcript.rs",
                "transcript/answer.rs",
                "transcript/band.rs",
                "transcript/output.rs",
                "transcript/pictured.rs",
                "transcript/post.rs",
                "transcript/said.rs",
                "transcript/slots.rs",
                "transcript/steps.rs",
                "transcript/thinking.rs",
                "views/actions.rs",
                "views/badge.rs",
                "views/button.rs",
                "views/code.rs",
                "views/diff.rs",
                "views/flex.rs",
                "views/input.rs",
                "views/keyvalue.rs",
                "views/link.rs",
                "views/list.rs",
                "views/markdown.rs",
                "views/progress.rs",
                "views/text.rs",
                "views/tree.rs",
                "welcome/drawing.rs",
                "well.rs",
                "window.rs",
            ],
        ),
        (
            "raised",
            &[
                "composer/drawing.rs",
                "layers.rs",
                "select.rs",
                "views/code.rs",
            ],
        ),
        (
            "presence",
            &[
                "activity.rs",
                "composer/drawing.rs",
                "dialog.rs",
                "form.rs",
                "select.rs",
                "transcript.rs",
                "views/actions.rs",
                "views/badge.rs",
            ],
        ),
        ("glow", &["transcript/band.rs", "transcript/ran.rs"]),
        (
            "good",
            &[
                "activity.rs",
                "transcript.rs",
                "views/badge.rs",
                "views/diff.rs",
            ],
        ),
        (
            "bad",
            &[
                "activity.rs",
                "dock/edge.rs",
                "transcript.rs",
                "transcript/ran.rs",
                "transcript/steps.rs",
                "views/badge.rs",
                "views/button.rs",
                "views/diff.rs",
                "well.rs",
            ],
        ),
        ("mode", &["markdown/mentions.rs"]),
        ("breath", &[]),
        ("pulse", &["transcript.rs", "views/progress.rs"]),
        ("comet", &["embers.rs"]),
        ("landing", &["transcript.rs"]),
        ("cooling", &["transcript.rs"]),
        ("ink", &["highlight.rs"]),
    ];
    for (token, files) in allowed {
        let mut seen: Vec<String> = sources()
            .into_iter()
            .filter(|(_, code)| code.contains(&format!("theme::{token}(")))
            .map(|(name, _)| name)
            .collect();
        seen.sort();
        assert_eq!(&seen, files, "where `{token}` is spent");
    }
}

/// Twenty-four bits and nothing else asked for.
fn asking() -> Ask {
    Ask {
        truecolor: true,
        ..Ask::default()
    }
}

#[test]
fn the_environment_picks_the_look_and_no_colour_wins() {
    assert_eq!(choose(Ask::default(), None).colors, Colors::Ansi);
    assert_eq!(choose(asking(), None).colors, Colors::True(DARK));
    assert_eq!(
        choose(
            Ask {
                no_color: true,
                ..asking()
            },
            None
        )
        .colors,
        Colors::Plain,
    );
    assert_eq!(
        choose(
            Ask {
                ascii: true,
                ..Ask::default()
            },
            None
        )
        .glyphs,
        &ASCII,
    );
    assert_eq!(choose(Ask::default(), None).glyphs, &UNICODE);
}

#[test]
fn a_terminal_that_answers_light_gets_the_light_palette() {
    let terminal = |light| choose(asking(), light).colors;
    assert_eq!(terminal(Some(true)), Colors::True(LIGHT));
    assert_eq!(terminal(Some(false)), Colors::True(DARK));
    assert_eq!(terminal(None), Colors::True(DARK), "and silence is dark");
}

#[test]
fn a_named_look_outranks_what_the_terminal_answered() {
    let named = |look, light| choose(Ask { look, ..asking() }, light).colors;
    assert_eq!(named(Look::Dark, Some(true)), Colors::True(DARK));
    assert_eq!(named(Look::Light, Some(false)), Colors::True(LIGHT));
    assert_eq!(look(Some("light")), Look::Light);
    assert_eq!(look(Some("dark")), Look::Dark);
    assert_eq!(look(Some("terminal")), Look::Terminal);
    assert_eq!(look(Some("sepia")), Look::Terminal, "and so is nonsense");
}

#[test]
fn no_colour_beats_a_terminal_that_answered_light() {
    assert_eq!(
        choose(
            Ask {
                no_color: true,
                look: Look::Light,
                ..asking()
            },
            Some(true)
        )
        .colors,
        Colors::Plain,
    );
}

/// An `OSC 11` reply, read the way `xparsecolor` reads the colour strings
/// terminals answer with — both terminators, every channel width, and the
/// older `#` form (M71).
#[test]
fn an_osc_eleven_reply_says_which_ground_the_terminal_has() {
    for (reply, ground) in [
        (b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\".as_slice(), Some(false)),
        (b"\x1b]11;rgb:1e/1e/2e\x07".as_slice(), Some(false)),
        (b"\x1b]11;rgb:0000/0000/0000\x1b\\".as_slice(), Some(false)),
        (b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\".as_slice(), Some(true)),
        (b"\x1b]11;rgb:f/f/f\x1b\\".as_slice(), Some(true)),
        (b"\x1b]11;rgb:fdfd/f6f6/e3e3\x07".as_slice(), Some(true)),
        (b"\x1b]11;#fdf6e3\x1b\\".as_slice(), Some(true)),
        (b"\x1b]11;#1e1e2e\x1b\\".as_slice(), Some(false)),
    ] {
        assert_eq!(
            answered(reply),
            ground,
            "{:?}",
            String::from_utf8_lossy(reply)
        );
    }
}

/// And nothing else is one. A reply to another question, a reply cut short
/// and a spec in a shape nobody writes all leave the look standing rather
/// than guessing at a palette.
#[test]
fn what_is_not_an_answer_to_that_question_changes_no_look() {
    for reply in [
        b"\x1b]10;rgb:cdcd/d6d6/f4f4\x1b\\".as_slice(),
        b"\x1b_Gi=31;OK\x1b\\".as_slice(),
        b"\x1bP>|ghostty 1.3.1\x1b\\".as_slice(),
        b"\x1b]11;rgb:1e1e/1e1e/2e2e".as_slice(),
        b"\x1b]11;\x1b\\".as_slice(),
        b"\x1b]11;rgb:1e1e/2e2e\x1b\\".as_slice(),
        b"\x1b]11;rgb:1e1e/1e1e/2e2e/ffff\x1b\\".as_slice(),
        b"\x1b]11;rgb:zzzz/1e1e/2e2e\x1b\\".as_slice(),
        b"\x1b]11;rgb:11111/1e1e/2e2e\x1b\\".as_slice(),
        b"\x1b]11;#ff\x1b\\".as_slice(),
        b"\x1b]11;teal\x1b\\".as_slice(),
    ] {
        assert_eq!(
            answered(reply),
            None,
            "{:?}",
            String::from_utf8_lossy(reply)
        );
    }
}

/// The threshold is the perceptual middle grey `terminal_colorsaurus`
/// falls back to, so the probe's own answer and a later reply about the
/// same colour cannot disagree. The pivot is a grey either side of `L* 50`.
#[test]
fn the_ground_is_light_from_the_middle_grey_up() {
    let grey = |v: u16| background_is_light(v * 0x101, v * 0x101, v * 0x101);
    assert!(!grey(0x75), "just under the middle");
    assert!(grey(0x79), "and just over");
    assert!(!grey(0x00));
    assert!(grey(0xff));
}

/// One row of §4's table: its name and the value it takes in a palette.
type Token = (&'static str, fn(Palette) -> Color);

/// Every token of §4's table, in the order the table lists them.
const TOKENS: &[Token] = &[
    ("raised", |p| p.you()),
    ("presence", |p| p.ember()),
    ("glow", |p| p.glow()),
    ("good", |p| p.good()),
    ("bad", |p| p.bad()),
    ("mode", |p| p.plan()),
    ("good_tint", |p| p.add_tint()),
    ("bad_tint", |p| p.del_tint()),
];

fn hex(colour: Color) -> String {
    match colour {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        other => format!("{other:?}"),
    }
}

/// The two palettes side by side: the same tokens, the same meanings, and
/// nothing between them but what each is worth.
#[test]
fn light_and_dark_differ_only_in_what_each_token_is_worth() {
    let rows = TOKENS
        .iter()
        .map(|(name, of)| {
            format!(
                "{name:<10} dark {}   light {}",
                hex(of(DARK)),
                hex(of(LIGHT))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::with_settings!({snapshot_path => "../snapshots"}, { insta::assert_snapshot!("palettes", rows); });
    for (name, of) in TOKENS {
        assert_ne!(of(DARK), of(LIGHT), "`{name}` is the same in both");
    }
}

/// How bright a colour reads, near enough for "is this the light one".
fn luma(colour: Color) -> f32 {
    let Color::Rgb(r, g, b) = colour else {
        return 0.0;
    };
    0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)
}

/// Every accent reads against the ground its own palette is drawn on. The
/// ink is not among them any more (M73): what a terminal's own foreground
/// reads against is the terminal's business, as it is for every other
/// program.
#[test]
fn each_palette_reads_against_its_own_ground() {
    for (name, of) in TOKENS {
        if *name == "raised" || name.ends_with("_tint") {
            continue;
        }
        assert!(
            luma(of(LIGHT)) < luma(LIGHT.you()),
            "`{name}` must read on the light ground"
        );
        assert!(
            luma(of(DARK)) > luma(DARK.you()),
            "`{name}` must read on the dark one"
        );
    }
    assert!(
        luma(DARK.you()) < luma(LIGHT.you()),
        "and each tint is at its own ground's end of the scale"
    );
}

#[test]
fn no_colour_keeps_the_weights_and_drops_the_hues() {
    with(
        Theme {
            colors: Colors::Plain,
            glyphs: &UNICODE,
        },
        || {
            for token in [presence(), glow(), good(), bad(), mode(), raised()] {
                assert_eq!(token, Style::new(), "NO_COLOR spends no colour");
            }
            assert_eq!(
                as_drawn(text()),
                as_drawn(Style::new()),
                "and the ink it does spell is the terminal's own"
            );
            assert_eq!(dim(), Style::new().add_modifier(Modifier::DIM));
            assert_eq!(bold(), Style::new().add_modifier(Modifier::BOLD));
        },
    );
}

#[test]
fn the_ascii_table_spells_every_glyph_in_one_cell() {
    with(ascii(), || {
        assert_eq!(bullet(), "*");
        assert_eq!(connector(), "-");
        assert_eq!(cursor(), ">");
        assert_eq!(todo(false), "-");
        assert_eq!(todo(true), "x");
        assert_eq!(thinking(), "|");
        assert_eq!(border().top_left, "+");
        assert_eq!(ellipsis(), "...");
        assert_eq!(
            bingo_sdk::look::mark(Some('❖'), bullet().is_ascii()).to_string(),
            "-",
            "and a skill says so in words instead"
        );
        assert_eq!(caret(), "|");
        assert_eq!(rule(), "-", "the rule is the box's own stroke");
    });
}

/// A glyph drawn in the bullet's place takes the bullet's room: the text
/// under a row is indented past the mark, not past whichever glyph the row
/// happened to draw.
#[test]
fn a_mark_in_the_bullets_place_is_the_bullets_width() {
    for look in [
        Theme {
            colors: Colors::Ansi,
            glyphs: &UNICODE,
        },
        ascii(),
    ] {
        with(look, || {
            assert_eq!(
                bingo_sdk::look::mark(Some('❖'), bullet().is_ascii())
                    .to_string()
                    .width(),
                bullet().width()
            )
        });
    }
}

#[test]
fn a_thought_has_the_narrower_caret() {
    assert_eq!(thinking(), "▏");
}

fn dark() -> Theme {
    Theme {
        colors: Colors::True(DARK),
        glyphs: &UNICODE,
    }
}

#[test]
fn a_ramp_the_eight_colours_cannot_draw_takes_its_two_ends() {
    assert_eq!(pulse(0.0), presence());
    assert_eq!(pulse(1.0), glow());
    assert_eq!(comet(0.0), glow());
    assert_eq!(comet(1.0), text());
}

#[test]
fn a_ramp_on_twenty_four_bits_passes_between_its_ends() {
    with(dark(), || {
        assert_eq!(pulse(0.0).fg, Some(DARK.ember()));
        assert_eq!(pulse(1.0).fg, Some(DARK.glow()));
        let middle = pulse(0.5).fg.expect("a colour between the two");
        assert_ne!(middle, DARK.ember());
        assert_ne!(middle, DARK.glow());
    });
}

/// A ramp whose far end is the resting ink stops at colour's own edge: the
/// tail is drawn while it is warm and handed to the terminal's foreground
/// where it has cooled, in every look (M73).
#[test]
fn a_ramp_towards_the_ink_ends_on_the_terminals_own() {
    with(dark(), || {
        assert_eq!(comet(0.0).fg, Some(DARK.glow()));
        let cooling = comet(SETTLES / 2.0).fg.expect("a colour on the way");
        assert_ne!(cooling, DARK.glow());
        assert_ne!(cooling, DARK.ember());
        assert_eq!(comet(SETTLES), text(), "and the ink takes the rest");
        assert_eq!(comet(1.0), text());
        assert_eq!(self::cooling(1.0), text());
        assert_eq!(landing(1.0), good().patch(bold()));
        assert_eq!(landing(0.1), bold());
    });
}

/// The two tokens the terminal owns: no look, and no answer about the
/// ground, puts a colour of ours in prose or in what stands behind it
/// (M73). This is the whole of why a scheme flip is instant.
#[test]
fn body_ink_is_terminal_owned_and_dim_uses_ember_or_a_modifier() {
    for look in [
        crate::painted::no_colour(),
        crate::painted::ascii(),
        crate::painted::truecolor(),
        crate::painted::daylight(),
    ] {
        with(look, || {
            assert_eq!(text(), Style::new().fg(Color::Reset), "{:?}", look.colors);
            match look.colors {
                Colors::True(palette) => assert_eq!(dim().fg, Some(palette.dim())),
                _ => assert!(dim().add_modifier.contains(Modifier::DIM)),
            }
        });
    }
}

#[test]
fn nothing_that_moves_spends_a_colour_under_no_colour() {
    with(no_colour(), || {
        for style in [pulse(1.0), comet(0.0)] {
            assert_eq!(style, Style::new());
        }
    });
}

fn no_colour() -> Theme {
    Theme {
        colors: Colors::Plain,
        glyphs: &UNICODE,
    }
}

#[test]
fn a_look_is_only_switched_for_the_thread_that_asked() {
    assert_eq!(glyphs(), &UNICODE);
    with(ascii(), || assert_eq!(glyphs(), &ASCII));
    assert_eq!(glyphs(), &UNICODE, "the override is given back");
}
