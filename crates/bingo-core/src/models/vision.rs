//! A model without vision never receives an image part. The projection is
//! request-time only: the journal and the items keep every image, so a later
//! switch to a model that sees brings them back.

use bingo_sdk::{ContentPart, Message};

/// The model is named because this line is the whole explanation the model
/// gets for a gap the person can still see in the transcript.
pub fn omitted_note(model: &str) -> String {
    format!("[image omitted: {model} has no vision]")
}

/// `messages` with every image — a pasted one, or one inside a tool result —
/// replaced by `note`; `None` when nothing had to change, so the common case
/// pays a scan and never a clone.
pub fn project_images_out(messages: &[Message], note: &str) -> Option<Vec<Message>> {
    if !messages.iter().any(|m| m.parts.iter().any(has_image)) {
        return None;
    }
    Some(
        messages
            .iter()
            .map(|m| Message {
                role: m.role,
                parts: m.parts.iter().map(|p| project(p, note)).collect(),
                provider_options: m.provider_options.clone(),
            })
            .collect(),
    )
}

fn has_image(part: &ContentPart) -> bool {
    match part {
        ContentPart::Image(_) => true,
        ContentPart::ToolResult { parts, .. } => parts.iter().any(has_image),
        _ => false,
    }
}

fn project(part: &ContentPart, note: &str) -> ContentPart {
    match part {
        // A picture that is a file is still a file: the model cannot see
        // it, but it can hand the path on (ADR-0052 §3).
        ContentPart::Image(image) => ContentPart::text(match image.whereabouts() {
            Some(words) => format!("{note} {words}"),
            None => note.to_string(),
        }),
        ContentPart::ToolResult {
            tool_use_id,
            parts,
            is_error,
        } => ContentPart::ToolResult {
            tool_use_id: tool_use_id.clone(),
            parts: parts.iter().map(|p| project(p, note)).collect(),
            is_error: *is_error,
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{Image, Role};

    fn image() -> ContentPart {
        ContentPart::Image(Image {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
            path: None,
        })
    }

    #[test]
    fn a_conversation_without_images_is_left_untouched() {
        let messages = vec![Message::text(Role::User, "hello")];
        assert_eq!(project_images_out(&messages, "note"), None);
    }

    #[test]
    fn images_at_the_top_and_inside_tool_results_become_the_note() {
        let messages = vec![
            Message::user(vec![ContentPart::text("look"), image()]),
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "call_1".into(),
                parts: vec![image(), ContentPart::text("done")],
                is_error: false,
            }]),
        ];
        let projected = project_images_out(&messages, "[gone]").expect("changed");
        assert_eq!(
            projected[0].parts,
            vec![ContentPart::text("look"), ContentPart::text("[gone]")]
        );
        assert_eq!(
            projected[1].parts,
            vec![ContentPart::ToolResult {
                tool_use_id: "call_1".into(),
                parts: vec![ContentPart::text("[gone]"), ContentPart::text("done")],
                is_error: false,
            }]
        );
        assert!(
            messages[0].parts.contains(&image()),
            "the source is not modified"
        );
        assert_eq!(omitted_note("m"), "[image omitted: m has no vision]");
    }

    /// A model that cannot see is still told where the file is, so it can
    /// hand the path to a tool (ADR-0052 §3).
    #[test]
    fn a_picture_with_a_path_leaves_its_path_behind() {
        let image = Image::from_bytes("image/png", b"png")
            .expect("small")
            .at("/shots/a.png");
        let messages = vec![Message::user(vec![ContentPart::Image(image)])];
        let projected = project_images_out(&messages, "note").expect("changed");
        assert_eq!(
            projected[0].parts,
            vec![ContentPart::text("note [picture: /shots/a.png]")]
        );
    }
}
