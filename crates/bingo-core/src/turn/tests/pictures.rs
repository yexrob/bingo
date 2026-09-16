//! A picture that ages off the wire (ADR-0061): the items keep it, the
//! requests stop carrying it.

use serde_json::Value;

use super::*;

/// A tool whose result is a picture, as `Read` returns one.
struct PictureTool {
    image: Image,
}

#[async_trait]
impl Tool for PictureTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Picture".into(),
            description: "returns a picture".into(),
            input_schema: json!({"type": "object"}),
            meta: Default::default(),
        }
    }
    fn traits(&self, _: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }
    async fn call(&self, _input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            parts: vec![ContentPart::Image(self.image.clone())],
            is_error: false,
            display: None,
        })
    }
}

/// The scripted model with eyes; every other test here runs the blind one.
fn seeing_config(provider: Arc<ScriptedProvider>, tools: Vec<Arc<dyn Tool>>) -> TurnConfig {
    let mut cfg = config(provider, tools);
    if let Some(model) = cfg.model.as_mut() {
        model.capabilities = seeing();
    }
    cfg
}

fn png(bytes: usize) -> Image {
    Image::from_bytes("image/png", &vec![0u8; bytes]).expect("within the cap")
}

/// Every part a request carries, tool results opened up: where a picture sits
/// is the fold's business, and the projection's business is only whether it is
/// still there.
fn wire_parts(request: &ModelRequest) -> Vec<ContentPart> {
    fn walk(part: &ContentPart, out: &mut Vec<ContentPart>) {
        match part {
            ContentPart::ToolResult { parts, .. } => parts.iter().for_each(|p| walk(p, out)),
            other => out.push(other.clone()),
        }
    }
    let mut out = vec![];
    for part in request.messages.iter().flat_map(|m| m.parts.iter()) {
        walk(part, &mut out);
    }
    out
}

fn carried(provider: &ScriptedProvider, part: &ContentPart) -> Vec<bool> {
    provider
        .requests()
        .iter()
        .map(|request| wire_parts(request).contains(part))
        .collect()
}

/// ADR-0061 §1: the picture is whole for three answers and a note after them,
/// while the items keep it for the surfaces and for a reopen.
#[tokio::test]
async fn a_picture_a_tool_returned_leaves_the_wire_after_three_answers() {
    let image = png(3_000).at("/shots/a.png");
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Picture", json!({}))),
        Script::Events(tool_call("Echo", json!({"v": 1}))),
        Script::Events(tool_call("Echo", json!({"v": 2}))),
        Script::Events(tool_call("Echo", json!({"v": 3}))),
        Script::Events(text("done")),
    ]);
    let cfg = seeing_config(
        provider.clone(),
        vec![
            Arc::new(PictureTool {
                image: image.clone(),
            }),
            Arc::new(EchoTool { read_only: true }),
        ],
    );
    let host = RecordingHost::new();
    let out = run(&cfg, &host, CancellationToken::new()).await;
    assert_eq!(out.status, TurnStatus::Completed, "{:?}", host.kinds());
    let whole = ContentPart::Image(image.clone());
    let note = ContentPart::text("[image elided: image/png 3.0 KB] [picture: /shots/a.png]");
    assert_eq!(
        carried(&provider, &whole),
        vec![false, true, true, true, false],
        "the first request is before the call; the last is after the third answer"
    );
    assert_eq!(
        carried(&provider, &note),
        vec![false, false, false, false, true]
    );
    assert!(
        out.items.iter().any(|item| matches!(&item.body,
            ItemBody::ToolCall { output: Some(output), .. } if output.parts.contains(&whole))),
        "the items keep the picture the wire gave up"
    );
}

/// A picture the person pasted ages by the same count, and one that is
/// nowhere on this machine is named without a path.
#[tokio::test]
async fn a_picture_at_the_top_of_what_the_person_said_ages_the_same_way() {
    let image = png(3_000);
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"v": 1}))),
        Script::Events(tool_call("Echo", json!({"v": 2}))),
        Script::Events(tool_call("Echo", json!({"v": 3}))),
        Script::Events(text("done")),
    ]);
    let cfg = seeing_config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: true })],
    );
    let host = RecordingHost::new();
    let mut frames = history("look at this");
    if let Event::ItemCompleted { item } = &mut frames[0].event
        && let ItemBody::User { parts, .. } = &mut item.body
    {
        parts.push(ContentPart::Image(image.clone()));
    }
    let out = run_turn(
        &cfg,
        TurnRun {
            turn: TurnId::from_raw("trn_1"),
            history: frames,
            generation: 0,
            cancel: CancellationToken::new(),
            kind: TurnKind::Respond,
        },
        &host,
    )
    .await;
    assert_eq!(out.status, TurnStatus::Completed, "{:?}", host.kinds());
    assert_eq!(
        carried(&provider, &ContentPart::Image(image)),
        vec![true, true, true, false]
    );
    assert_eq!(
        carried(
            &provider,
            &ContentPart::text("[image elided: image/png 3.0 KB]")
        ),
        vec![false, false, false, true]
    );
}
