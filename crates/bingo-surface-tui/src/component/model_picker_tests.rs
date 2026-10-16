use crate::test_support::*;
use bingo_mod_models::{card::Chat, sheet::Sheet, state::Picker};
use bingo_sdk::*;
use serde_json::json;

fn entries() -> (Vec<CatalogEntry>, Vec<CatalogEntry>) {
    let providers=[("anthropic","ready","ok"),("openai","ready","ok"),("work","ready","down"),("codex","missing","unchecked")].into_iter().map(|(id,auth,reach)|CatalogEntry{id:id.into(),label:id.into(),meta:json!({"auth":{"kind":auth,"from":"store"},"reach":{"kind":reach,"ms":84,"error":"connection refused"},"address":"http://localhost:9/v1","instance":id=="work"})}).collect();
    let models=[("anthropic/claude-sonnet-5-5",true,true), ("openai/gpt-6",true,true),("anthropic/claude-opus-5-5",true,true),("work/qwen3-coder-30b",false,false),("codex/gpt-6",false,true),("anthropic/claude-sonnet-5-5-20261015",false,true)].into_iter().map(|(id,pinned,images)|CatalogEntry{id:id.into(),label:id.split_once('/').unwrap().1.into(),meta:json!({"provider":id.split_once('/').unwrap().0,"context":128000,"switchAt":115200,"output":32000,"images":images,"reasoning":images,"efforts":if images{vec!["low","medium","high"]}else{vec![]},"pinned":pinned,"chat":true,"source":"catalogue","price":{"input":3,"output":15},"aliasOf":if id.ends_with("20261015"){Some("claude-sonnet-5-5")}else{None}})}).collect();
    (models, providers)
}
#[test]
fn prototype_scenes_at_both_terminal_sizes() {
    let (models, providers) = entries();
    for (width, height) in [(108, 30), (80, 24)] {
        for scene in [
            "open", "direct", "long", "down", "codex", "paste", "all", "quick", "manage",
        ] {
            if scene == "quick" {
                insta::assert_snapshot!(
                    format!("model_{scene}_{width}x{height}"),
                    quick_screen(width, height)
                );
                continue;
            }
            let mut providers = providers.clone();
            if scene == "long" {
                providers.iter_mut().find(|p| p.id == "work").unwrap().meta["reach"]["kind"] =
                    json!("ok");
            }
            let (picker, chat) = scenario(scene, &models);
            let mut models = models.clone();
            if scene == "long" {
                // The kernel's switch decision for a chat this long.
                for model in &mut models {
                    model.meta["compacts"] = json!(model.id != chat.current);
                }
            }
            let view = bingo_mod_models::draw::tree(&picker, &models, &providers, &chat, width, 18);
            let text = serde_json::to_string(&view).unwrap();
            assert!(!text.contains("sk-dummy-never-draw-this"));
            assert!(
                view::answer_ok(
                    render::Component::ModelPicker,
                    &json!({}),
                    bingo_mod_models::Models.manifest().id,
                    None,
                    &view
                )
                .is_ok()
            );
            let state = mounted(view);
            let (mut ui, now) = scene_ui();
            let screen = draw_sized(width, height, &state, &mut ui, now);
            assert!(!screen.contains("sk-dummy-never-draw-this"));
            insta::assert_snapshot!(format!("model_{scene}_{width}x{height}"), screen);
        }
    }
}
fn scenario(scene: &str, models: &[CatalogEntry]) -> (Picker, Chat) {
    let mut picker = Picker {
        id: "picker".into(),
        props: component::ModelPickerProps {
            effort: Some(Effort::Medium),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut chat = Chat {
        current: models[0].id.clone(),
        used: 14000,
        ..Default::default()
    };
    match scene {
        "direct" => {
            picker.props.filter = "opus".into();
        }
        "long" => {
            chat.used = 182000;
            chat.pictures = true;
            picker.props.filter = "qwen".into();
        }
        "all" => picker.all = true,
        "quick" => picker.props.effort = Some(Effort::High),
        _ => {}
    }
    picker.sheet = scenario_sheet(scene);
    (picker, chat)
}
fn mounted(view: View) -> SessionState {
    let mut state = state();
    let mut size = slot::PaneSize::from_rows(18);
    size.columns.ideal = slot::Length::Percent(100);
    state.apply(&frame(
        1,
        Event::Signal {
            plugin: bingo_mod_models::Models.manifest().id.into(),
            kind: "picker".into(),
            payload: slot::with(
                json!({}),
                [slot::Slot::Pane {
                    hints: None,
                    id: "picker".into(),
                    title: "model".into(),
                    focus: true,
                    close_on_escape: true,
                    hold_toasts: true,
                    asked: true,
                    place: bingo_mod_models::state::placement(),
                    size,
                    view: view.into(),
                }],
            ),
            generation: None,
        },
    ));
    state
}
fn scene_ui() -> (crate::ui::Ui, crate::clock::Now) {
    scene()
}

#[test]
fn model_chords_submit_their_bound_lines_without_opening() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mut ui, now) = scene_ui();
    let state = state();
    for (name, chord) in [("model", "alt+m"), ("think", "alt+t")] {
        let mut spec = CommandSpec::ui("fixture");
        spec.name = name.into();
        spec.hidden = false;
        spec.keys = vec![bingo_sdk::KeyBinding::WithArgs {
            chord: chord.into(),
            args: "next".into(),
        }];
        ui.catalogs.commands.push(crate::commands::entry(spec));
    }
    for (letter, line) in [('m', "/model next"), ('t', "/think next")] {
        let effects = crate::input::on_key(
            &mut ui,
            &solo(&state),
            KeyEvent::new(KeyCode::Char(letter), KeyModifiers::ALT),
            now,
        );
        assert!(matches!(&effects[..], [crate::effect::Effect::Run(text)] if text == line));
        assert!(!ui.focus.showing());
        assert!(!draw_sized(80, 24, &state, &mut ui, now).contains("pinned ·"));
    }
}

fn quick_screen(width: u16, height: u16) -> String {
    let text = "⇄ gpt-6 · openai · thinking high · this session";
    let completed_item = |item| folded(vec![frame(1, Event::ItemCompleted { item })]);
    let mut state = completed_item(item(
        "itm_choice",
        ItemStatus::Completed,
        ItemBody::Notice {
            level: Level::Info,
            code: "model.choice".into(),
            text: text.into(),
        },
    ));
    state.config.kernel["render"] = json!({"InfoNotice":[{}]});
    let (mut ui, now) = scene_ui();
    draw_sized(width, height, &state, &mut ui, now);
    let key = ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.component == render::Component::InfoNotice)
        .unwrap()
        .clone();
    ui.placed.pulled.request(&key);
    ui.placed
        .pulled
        .answer(key, bingo_mod_models::draw::outcome(text));
    let screen = draw_sized(width, height, &state, &mut ui, now);
    assert!(!screen.contains("pinned ·"));
    screen
}

fn scenario_sheet(scene: &str) -> Option<Sheet> {
    match scene {
        "down" | "manage" => {
            return Some(Sheet::Manage {
                provider: "work".into(),
                selected: 0,
                pending: Some("work/qwen3-coder-30b".into()),
                confirm: false,
            });
        }
        "codex" => {
            return Some(Sheet::SignIn {
                provider: "codex".into(),
                selected: 0,
                pending: Some("codex/gpt-6".into()),
            });
        }
        "paste" => {
            return Some(Sheet::Add {
                guess: bingo_mod_models::guess::Guess {
                    name: "work".into(),
                    shape: "openai".into(),
                    address: "http://localhost:11434/v1".into(),
                    key: Some("sk-dummy-never-draw-this".into()),
                },
                selected: 0,
            });
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod receipt {
    use super::*;
    #[test]
    fn choice_receipt_uses_the_attention_tone_and_dim_text() {
        use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};
        let view = bingo_mod_models::draw::outcome("⇄ small · here");
        let lines = crate::views::render(&view, 80);
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new(lines.clone()), frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let attention = crate::views::render(
            &View::Badge {
                text: "x".into(),
                tone: Tone::Attention,
            },
            80,
        );
        assert_eq!(buffer[(0, 0)].fg, attention[0].spans[0].style.fg.unwrap());
        assert_eq!(
            buffer[(1, 0)].fg,
            crate::theme::dim()
                .fg
                .unwrap_or(ratatui::style::Color::Reset)
        );
        assert!(
            !buffer[(0, 0)]
                .modifier
                .contains(ratatui::style::Modifier::DIM)
        );
        assert!(
            buffer[(1, 0)]
                .modifier
                .contains(ratatui::style::Modifier::DIM)
        );
        assert_eq!(buffer[(0, 0)].symbol(), "⇄");
    }
}

#[test]
fn a_filtered_picker_reflows_its_cursor_when_the_pane_shrinks() {
    let (picker, models, providers) = filtered_last_model();
    let state = state();
    let request = render::RenderRequest {
        component: render::Component::ModelPicker,
        request_id: "filtered".into(),
        surface: render::SurfaceName::Terminal,
        props: json!(picker.props),
        viewport: render::Viewport {
            columns: 108,
            rows: 18,
            place: None,
        },
    };
    let mut memo = crate::pulled::Memo::default();
    let tall = crate::pulled::Key::new(&state, &request, 108, 18);
    assert!(memo.request(&tall));
    memo.answer(
        tall,
        bingo_mod_models::draw::tree(&picker, &models, &providers, &Chat::default(), 108, 18),
    );
    let short = crate::pulled::Key::new(&state, &request, 108, 8);
    assert!(memo.request(&short), "a short pane needs a new row window");
    memo.answer(
        short.clone(),
        bingo_mod_models::draw::tree(&picker, &models, &providers, &Chat::default(), 108, 8),
    );
    let view = memo.get(&short).unwrap();
    let lines = crate::views::render(view, 108);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(108, 8)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(
                ratatui::widgets::Paragraph::new(lines.clone()),
                frame.area(),
            )
        })
        .unwrap();
    let screen = format!("{:?}", terminal.backend().buffer());
    assert!(
        screen
            .lines()
            .any(|line| line.contains('❯') && line.contains("m29")),
        "{screen}"
    );
}

fn filtered_last_model() -> (Picker, Vec<CatalogEntry>, Vec<CatalogEntry>) {
    let (_, providers) = entries();
    let models: Vec<_> = (0..30)
        .map(|i| CatalogEntry {
            id: format!("openai/m{i}"),
            label: format!("m{i}"),
            meta: json!({"provider":"openai","context":128000}),
        })
        .collect();
    let picker = Picker {
        props: component::ModelPickerProps {
            filter: "openai/".into(),
            selected: Some("model:openai/m29".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    (picker, models, providers)
}
