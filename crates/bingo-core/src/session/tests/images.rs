//! A picture beside the words (ADR-0040): what `validate` and `journal_prose`
//! do with `Input::Text.images`, exercised through a `Log` session — nothing
//! answers there, so a submit journals at once and the assertion is about the
//! parts alone.

use super::*;

fn log_session() -> Mailbox {
    let provider = ScriptedProvider::new(vec![]);
    let mut summary = summary("ses_log");
    summary.driver = Driver::Log;
    spawn(summary, None, Services::none(), |_| {
        let mut cfg = config(provider, vec![], Arc::new(NoHost));
        cfg.model = None;
        Arc::new(cfg)
    })
}

fn acked(frame: &Frame) -> bool {
    matches!(frame.event, Event::IntentAck { .. })
}

fn image(media_type: &str, bytes: &[u8]) -> Image {
    Image::from_bytes(media_type, bytes).expect("a small image encodes")
}

/// `decoded_len` reads the base64 length alone, so an over-cap image needs no
/// real encoding: enough filler characters and no padding lands the
/// arithmetic one byte past `Image::MAX_BYTES`.
fn oversized_image() -> Image {
    let base64_len = (Image::MAX_BYTES / 3 + 1) * 4;
    Image {
        media_type: "image/png".into(),
        data: "A".repeat(base64_len),
    }
}

fn first_parts(state: &SessionState) -> &[ContentPart] {
    let ItemBody::User { parts, .. } = &state.items[0].body else {
        panic!("a user item first");
    };
    parts
}

#[tokio::test]
async fn an_image_with_no_words_journals_as_one_image_part() {
    let mailbox = log_session();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(
        IntentId::mint(),
        Input::Text {
            text: String::new(),
            images: vec![image("image/png", b"abc")],
            origin: Origin::surface("tui"),
            delivery: Delivery::Wake,
        },
    );
    frames_until(&mut events, &mut state, acked).await;
    let parts = first_parts(&state);
    assert_eq!(parts.len(), 1);
    assert!(matches!(&parts[0], ContentPart::Image(_)));
}

#[tokio::test]
async fn text_and_two_images_journal_in_order() {
    let mailbox = log_session();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(
        IntentId::mint(),
        Input::Text {
            text: "look at these".into(),
            images: vec![image("image/png", b"one"), image("image/gif", b"two")],
            origin: Origin::surface("tui"),
            delivery: Delivery::Wake,
        },
    );
    frames_until(&mut events, &mut state, acked).await;
    let parts = first_parts(&state);
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].as_text(), Some("look at these"));
    assert!(matches!(&parts[1], ContentPart::Image(i) if i.media_type == "image/png"));
    assert!(matches!(&parts[2], ContentPart::Image(i) if i.media_type == "image/gif"));
}

#[tokio::test]
async fn an_unknown_media_type_is_invalid_input() {
    let mailbox = log_session();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let bogus = Image {
        media_type: "image/tiff".into(),
        data: "AAAA".into(),
    };
    mailbox.submit(
        IntentId::mint(),
        Input::Text {
            text: String::new(),
            images: vec![bogus],
            origin: Origin::surface("tui"),
            delivery: Delivery::Wake,
        },
    );
    let frames = frames_until(&mut events, &mut state, acked).await;
    assert!(matches!(
        &frames[0].event,
        Event::IntentAck { outcome: IntentOutcome::Rejected { error }, .. }
            if error.code == ErrorCode::InvalidInput && error.message.contains("image/tiff")
    ));
    assert!(state.items.is_empty());
}

#[tokio::test]
async fn an_oversize_image_is_invalid_input() {
    let mailbox = log_session();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(
        IntentId::mint(),
        Input::Text {
            text: String::new(),
            images: vec![oversized_image()],
            origin: Origin::surface("tui"),
            delivery: Delivery::Wake,
        },
    );
    let frames = frames_until(&mut events, &mut state, acked).await;
    assert!(matches!(
        &frames[0].event,
        Event::IntentAck { outcome: IntentOutcome::Rejected { error }, .. }
            if error.code == ErrorCode::InvalidInput && error.message.contains("too large")
    ));
    assert!(state.items.is_empty());
}
