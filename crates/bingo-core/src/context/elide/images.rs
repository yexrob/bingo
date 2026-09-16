//! A picture the model has already answered leaves the wire (ADR-0061): the
//! note says what it was and where to read it again. A projection like the
//! result elision beside it — the items, the journal and every surface keep
//! the picture.

use bingo_sdk::{ContentPart, Image, Message, Role, bytes::words};

/// `messages` with every image the model has answered `rounds_kept` times or
/// more replaced by [`note`]; `None` when no image is that old, so the common
/// case pays a scan and never a clone.
pub fn elide_old_images(messages: &[Message], rounds_kept: usize) -> Option<Vec<Message>> {
    let answered = answered_prefix(messages, rounds_kept);
    if !messages[..answered]
        .iter()
        .any(|m| m.parts.iter().any(has_image))
    {
        return None;
    }
    Some(
        messages
            .iter()
            .enumerate()
            .map(|(n, message)| match n < answered {
                true => elided(message),
                false => message.clone(),
            })
            .collect(),
    )
}

/// How many of the leading messages the model has answered `rounds_kept`
/// times. Counted from the end, because a message only grows older toward the
/// front: the answered ones are always a prefix.
fn answered_prefix(messages: &[Message], rounds_kept: usize) -> usize {
    let mut answers = 0usize;
    for (n, message) in messages.iter().enumerate().rev() {
        if answers >= rounds_kept {
            return n + 1;
        }
        if message.role == Role::Assistant {
            answers += 1;
        }
    }
    0
}

fn has_image(part: &ContentPart) -> bool {
    match part {
        ContentPart::Image(_) => true,
        ContentPart::ToolResult { parts, .. } => parts.iter().any(has_image),
        _ => false,
    }
}

fn elided(message: &Message) -> Message {
    Message {
        role: message.role,
        parts: message.parts.iter().map(project).collect(),
        provider_options: message.provider_options.clone(),
    }
}

fn project(part: &ContentPart) -> ContentPart {
    match part {
        ContentPart::Image(image) => ContentPart::text(note(image)),
        ContentPart::ToolResult {
            tool_use_id,
            parts,
            is_error,
        } => ContentPart::ToolResult {
            tool_use_id: tool_use_id.clone(),
            parts: parts.iter().map(project).collect(),
            is_error: *is_error,
        },
        other => other.clone(),
    }
}

/// What stands in for the picture: what it was, what it weighed, and — when
/// the bytes are somewhere on this machine — where to `Read` it again
/// (ADR-0052 §3).
pub fn note(image: &Image) -> String {
    let what = format!(
        "[image elided: {} {}]",
        image.media_type,
        words(image.decoded_len())
    );
    match image.whereabouts() {
        Some(whereabouts) => format!("{what} {whereabouts}"),
        None => what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn png(bytes: usize) -> Image {
        Image::from_bytes("image/png", &vec![0u8; bytes]).expect("within the cap")
    }

    fn shown(image: Image) -> Message {
        Message::user(vec![ContentPart::text("look"), ContentPart::Image(image)])
    }

    fn answer() -> Message {
        Message::text(Role::Assistant, "ok")
    }

    #[test]
    fn a_conversation_without_images_is_left_untouched() {
        let messages = vec![Message::text(Role::User, "hello"), answer(), answer()];
        assert_eq!(elide_old_images(&messages, 0), None);
        assert_eq!(elide_old_images(&[], 3), None);
    }

    #[test]
    fn a_picture_leaves_on_the_answer_after_the_third_and_not_before() {
        let messages = vec![shown(png(3_000)), answer(), answer(), answer()];
        assert_eq!(
            elide_old_images(&messages[..3], 3),
            None,
            "two answers is not three"
        );
        let projected = elide_old_images(&messages, 3).expect("changed");
        assert_eq!(
            projected[0].parts,
            vec![
                ContentPart::text("look"),
                ContentPart::text("[image elided: image/png 3.0 KB]"),
            ]
        );
        assert_eq!(projected[1..], messages[1..], "the answers are untouched");
        assert!(
            messages[0].parts.contains(&ContentPart::Image(png(3_000))),
            "the source is not modified"
        );
    }

    /// The picture a `Read` returned: the result it belongs to is still the
    /// same result, so the model can still tell which call answered it.
    #[test]
    fn inside_a_tool_result_the_id_the_flag_and_the_other_parts_survive() {
        let result = |parts| {
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "c1".into(),
                parts,
                is_error: true,
            }])
        };
        let messages = vec![
            result(vec![
                ContentPart::text("read 1 image"),
                ContentPart::Image(png(1_000).at("/a/b.png")),
            ]),
            answer(),
            answer(),
            answer(),
        ];
        let projected = elide_old_images(&messages, 3).expect("changed");
        assert_eq!(
            projected[0],
            result(vec![
                ContentPart::text("read 1 image"),
                ContentPart::text("[image elided: image/png 1.0 KB] [picture: /a/b.png]"),
            ])
        );
    }

    /// ADR-0061 Consequences: bytes from a wire client can never be read back,
    /// and the note is all the model gets to know about them.
    #[test]
    fn a_pathless_picture_gets_the_note_without_whereabouts() {
        assert_eq!(note(&png(1_000)), "[image elided: image/png 1.0 KB]");
    }

    /// The note the `blender_demo` turn would have sent instead of its 2.2 MB
    /// PNG on every round after the third (ADR-0061 Context).
    #[test]
    fn the_note_names_the_type_the_size_and_where_to_read_it_again() {
        let image = png(2_212_534).at("/a/b.png");
        assert_eq!(
            note(&image),
            "[image elided: image/png 2.2 MB] [picture: /a/b.png]"
        );
    }

    /// What a message looks like on the wire, as the projection must leave it.
    fn shape(messages: &[Message]) -> Vec<(Role, usize)> {
        messages.iter().map(|m| (m.role, m.parts.len())).collect()
    }

    fn keeps_a_picture(message: &Message) -> bool {
        message.parts.iter().any(has_image)
    }

    proptest! {
        #[test]
        fn only_answered_pictures_move_and_the_projection_is_idempotent(
            spoken in proptest::collection::vec(any::<bool>(), 0..12),
            rounds_kept in 0usize..5,
        ) {
            let messages: Vec<Message> = spoken
                .iter()
                .map(|shows| match shows {
                    true => shown(png(64)),
                    false => answer(),
                })
                .collect();
            let once = elide_old_images(&messages, rounds_kept)
                .unwrap_or_else(|| messages.clone());
            prop_assert_eq!(shape(&once), shape(&messages));
            for (n, message) in messages.iter().enumerate() {
                let answers = messages[n + 1..].iter().filter(|m| m.role == Role::Assistant).count();
                let stays = answers < rounds_kept || !keeps_a_picture(message);
                prop_assert_eq!(once[n].parts == message.parts, stays, "message {}", n);
            }
            prop_assert_eq!(elide_old_images(&once, rounds_kept), None);
        }
    }
}
