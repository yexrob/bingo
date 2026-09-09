//! What a message says, whatever kind of message it is (ADR-0051 §1).
//!
//! One normaliser, and it never answers "nothing". A sticker, a shared chat,
//! a card and a type this build has never heard of all come out as words,
//! because a chat that silently drops a message is a chat nobody can rely on
//! — and the surface it was dropped in never says so.
//!
//! Nothing here does I/O. What a message carried beside its words leaves as
//! the key the platform serves it under; [`super::attachments`] is what turns a key
//! into bytes.
//!
//! The field names are Feishu's own, read off 接收消息内容
//! (open.feishu.cn, `im-v1/message-content-description/message_content`):
//! `post` is a title and paragraphs of tagged runs, `file`/`audio`/`media`
//! carry a `file_key`, an `interactive` card names itself under
//! `header.title.content`.

use serde_json::{Value, json};

/// The words of one message, and what it carried beside them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spoken {
    pub text: String,
    pub resources: Vec<Resource>,
}

/// One thing a message carried that is not words, by the address it is served
/// under: parsing does no I/O, so what leaves here is the key, not the bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub message: String,
    pub key: String,
    pub kind: Kind,
}

/// What a resource is, which decides both how it is asked for and what
/// becomes of it: a picture joins the ask itself (ADR-0040), everything else
/// lands on disk and becomes a path in the words (ADR-0051 §2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Picture,
    File {
        name: String,
    },
    /// A voice note. Feishu records them as opus and names them nothing.
    Audio,
    Video {
        name: String,
    },
}

/// The placeholder that stands for everyone in a chat.
const EVERYONE: &str = "@_all";

/// The most lines an interactive card is read as. A card can nest a whole
/// dashboard; what a person meant by sending it is at the top of it.
const CARD_LINES: usize = 12;

/// Keys under which a card keeps something a person reads.
const TEXT_KEYS: [&str; 2] = ["text", "content"];

/// Card elements that are something to press rather than something to read.
const ACTIONS: [&str; 3] = ["button", "select_static", "overflow"];

/// Keys of an action that are never its label.
const UNREAD: [&str; 2] = ["tag", "value"];

/// What one message amounts to. `id` is the message the resources will be
/// fetched from, `message_type` and `content` are the platform's own two
/// fields, `mentions` its list of who was named, and `me` this bot's open id
/// so that its own mention can be taken back out of the words.
pub fn spoken(id: &str, message_type: &str, content: &str, mentions: &Value, me: &str) -> Spoken {
    // Content that will not parse is still content: a person typed it.
    let body: Value = serde_json::from_str(content).unwrap_or_else(|_| json!({ "text": content }));
    let envelope = Envelope { id, mentions, me };
    let spoken = match message_type {
        "text" => words(envelope.resolve(body["text"].as_str().unwrap_or_default())),
        "post" => post(&body, &envelope),
        "image" => envelope.carried(&body["image_key"], Kind::Picture),
        "file" => envelope.carried(&body["file_key"], Kind::File { name: named(&body) }),
        "audio" => envelope.carried(&body["file_key"], Kind::Audio),
        "media" => envelope.carried(&body["file_key"], Kind::Video { name: named(&body) }),
        "interactive" => words(card(&body)),
        "share_chat" => words(share_chat(&body)),
        "merge_forward" => words(forwarded(&body)),
        _ => Spoken::default(),
    };
    heard_as(spoken, message_type)
}

/// A fenced block, with a fence long enough that nothing inside it can end
/// the block early — a file full of markdown is exactly the case that breaks
/// three backticks.
pub fn fenced(language: &str, text: &str) -> String {
    let fence = "`".repeat(backticks(text).max(2) + 1);
    format!("{fence}{language}\n{text}\n{fence}")
}

/// What only the envelope of a message knows, carried down to the runs that
/// need it.
struct Envelope<'a> {
    id: &'a str,
    /// The platform's list of who was named: the only place a `@_user_N`
    /// placeholder has a name on it.
    mentions: &'a Value,
    me: &'a str,
}

impl Envelope<'_> {
    /// A message that is one attachment and no words at all.
    fn carried(&self, key: &Value, kind: Kind) -> Spoken {
        Spoken {
            text: String::new(),
            resources: self.resource(key, kind).into_iter().collect(),
        }
    }

    fn resource(&self, key: &Value, kind: Kind) -> Option<Resource> {
        Some(Resource {
            message: self.id.to_string(),
            key: text_of(key)?.to_string(),
            kind,
        })
    }

    /// What a mention reads as: `@all` for everyone, nothing at all for us —
    /// somebody who writes "@bingo run the tests" asked for the tests — and
    /// otherwise the name that was on their screen.
    fn reads(&self, who: &str, fallback: &str) -> String {
        if who == EVERYONE {
            return "@all".into();
        }
        let listed = self.listed(who);
        let open_id = listed.and_then(|m| m["id"]["open_id"].as_str());
        if open_id.unwrap_or(who) == self.me {
            return String::new();
        }
        let name = listed.and_then(|m| text_of(&m["name"])).or(some(fallback));
        format!("@{}", name.unwrap_or("someone"))
    }

    fn listed(&self, who: &str) -> Option<&Value> {
        array(self.mentions)
            .iter()
            .find(|m| m["key"].as_str() == Some(who) || m["id"]["open_id"].as_str() == Some(who))
    }

    /// Every `@_user_N` placeholder in a text body replaced by what it reads
    /// as. The longest placeholder goes first, because `@_user_1` is a prefix
    /// of `@_user_10` and a chat with ten mentions is an ordinary chat.
    fn resolve(&self, text: &str) -> String {
        let mut keys: Vec<&str> = array(self.mentions)
            .iter()
            .filter_map(|m| m["key"].as_str())
            .collect();
        keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
        let mut text = text.to_string();
        for key in keys {
            text = text.replace(key, &self.reads(key, ""));
        }
        // Feishu leaves `@_all` out of the mentions list, so the placeholder
        // in the words is the only trace of it there is.
        text.replace(EVERYONE, "@all")
    }
}

/// A message that is words and nothing else.
fn words(text: String) -> Spoken {
    Spoken {
        text,
        resources: Vec::new(),
    }
}

/// The words trimmed, and the last word on a message that came to nothing: a
/// type this build has no reading for still arrives as the name of its type,
/// so a sticker in a direct chat is answered rather than ignored.
fn heard_as(mut spoken: Spoken, message_type: &str) -> Spoken {
    spoken.text = spoken.text.trim().to_string();
    if spoken.text.is_empty() && spoken.resources.is_empty() {
        spoken.text = format!("[{message_type}]");
    }
    spoken
}

/// A rich-text message flattened: its title on a line of its own, then one
/// line per paragraph of runs.
fn post(body: &Value, envelope: &Envelope<'_>) -> Spoken {
    let body = localised(body);
    let mut spoken = Spoken::default();
    let mut lines: Vec<String> = text_of(&body["title"])
        .map(str::to_string)
        .into_iter()
        .collect();
    for paragraph in array(&body["content"]) {
        lines.push(
            runs(paragraph, envelope, &mut spoken.resources)
                .trim()
                .to_string(),
        );
    }
    spoken.text = lines.join("\n");
    spoken
}

/// A post arrives as `{title, content}`, but one that has been through a
/// client library or a forward arrives keyed by locale. Either way the body
/// is the object with a `content` array in it.
fn localised(body: &Value) -> &Value {
    if body["content"].is_array() {
        return body;
    }
    body.as_object()
        .into_iter()
        .flatten()
        .map(|(_, value)| value)
        .find(|value| value["content"].is_array())
        .unwrap_or(body)
}

/// One paragraph, its runs laid end to end.
fn runs(paragraph: &Value, envelope: &Envelope<'_>, resources: &mut Vec<Resource>) -> String {
    let mut line = String::new();
    for value in array(paragraph) {
        let (text, resource) = run(value, envelope);
        line.push_str(&text);
        resources.extend(resource);
    }
    line
}

/// One run of a post: what it reads as, and what it leaves to be fetched.
fn run(value: &Value, envelope: &Envelope<'_>) -> (String, Option<Resource>) {
    let text = value["text"].as_str().unwrap_or_default();
    let file_key = &value["file_key"];
    match value["tag"].as_str().unwrap_or_default() {
        "a" => (link(value, text), None),
        "at" => (
            envelope.reads(
                value["user_id"].as_str().unwrap_or_default(),
                value["user_name"].as_str().unwrap_or_default(),
            ),
            None,
        ),
        "img" | "image" => (
            String::new(),
            envelope.resource(&value["image_key"], Kind::Picture),
        ),
        "media" | "video" => (
            String::new(),
            envelope.resource(file_key, Kind::Video { name: named(value) }),
        ),
        "file" => (
            String::new(),
            envelope.resource(file_key, Kind::File { name: named(value) }),
        ),
        "audio" => (String::new(), envelope.resource(file_key, Kind::Audio)),
        "code_block" | "pre" => (
            fenced(value["language"].as_str().unwrap_or_default(), text),
            None,
        ),
        "hr" | "divider" => ("---".into(), None),
        "emotion" | "emoji" => (emotion(value, text), None),
        // `text`, and every tag a later Feishu adds: its words, if it has any.
        _ => (text.to_string(), None),
    }
}

/// `[label](href)`, with the address for a label where there is none — a link
/// nobody can read is worse than a long one.
fn link(value: &Value, text: &str) -> String {
    let href = value["href"].as_str().unwrap_or_default();
    match (text, href) {
        ("", "") => String::new(),
        (label, "") => label.to_string(),
        ("", href) => format!("[{href}]({href})"),
        (label, href) => format!("[{label}]({href})"),
    }
}

fn emotion(value: &Value, text: &str) -> String {
    match text_of(&value["emoji_type"]).or(some(text)) {
        Some(name) => format!(":{name}:"),
        None => String::new(),
    }
}

/// An interactive card read as words: what it calls itself, the text of its
/// elements, and the labels of whatever a person could press.
fn card(body: &Value) -> String {
    let card = body
        .get("card")
        .filter(|card| card.is_object())
        .unwrap_or(body);
    let mut lines: Vec<String> = text_of(&card["header"]["title"]["content"])
        .map(str::to_string)
        .into_iter()
        .collect();
    let mut actions = Vec::new();
    collect(card, &mut lines, &mut actions);
    lines.truncate(CARD_LINES);
    if !actions.is_empty() {
        lines.push(format!("Actions: {}", actions.join(", ")));
    }
    lines.join("\n")
}

/// Every string a card shows, in the order it shows them, and separately the
/// labels of its actions: a button's words are what to press, not what to
/// read. A line already collected is not collected twice — a card repeats its
/// own title in half a dozen places.
fn collect(value: &Value, lines: &mut Vec<String>, actions: &mut Vec<String>) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| collect(item, lines, actions)),
        Value::Object(fields) => {
            if ACTIONS.contains(
                &fields
                    .get("tag")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ) {
                return kept(actions, &label(value));
            }
            for (key, item) in fields {
                match (TEXT_KEYS.contains(&key.as_str()), item.as_str()) {
                    (true, Some(text)) => kept(lines, text),
                    _ => collect(item, lines, actions),
                }
            }
        }
        _ => {}
    }
}

/// What a person reads on the thing they would press. An overflow keeps its
/// words down in its options, so the search goes as deep as it has to; a
/// button's `value` is what the click carries back, which is nobody's label.
fn label(action: &Value) -> String {
    match action {
        Value::String(text) => some(text).unwrap_or_default().to_string(),
        Value::Object(fields) => fields
            .iter()
            .filter(|(key, _)| !UNREAD.contains(&key.as_str()))
            .map(|(_, item)| label(item))
            .find(|label| !label.is_empty())
            .unwrap_or_default(),
        Value::Array(items) => items
            .iter()
            .map(label)
            .find(|label| !label.is_empty())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn kept(lines: &mut Vec<String>, line: &str) {
    let line = line.trim();
    if !line.is_empty() && !lines.iter().any(|already| already == line) {
        lines.push(line.to_string());
    }
}

fn share_chat(body: &Value) -> String {
    let name = text_of(&body["chat_name"]).or(text_of(&body["name"]));
    match (name, text_of(&body["chat_id"])) {
        (Some(name), Some(chat)) => format!("Shared chat: {name} ({chat})"),
        (Some(name), None) => format!("Shared chat: {name}"),
        (None, Some(chat)) => format!("Shared chat: {chat}"),
        (None, None) => String::new(),
    }
}

/// A merged bundle is its title here. The messages inside it are a fetch, and
/// [`super::merged`] is what appends them.
fn forwarded(body: &Value) -> String {
    text_of(&body["title"])
        .or(text_of(&body["content"]))
        .unwrap_or_default()
        .to_string()
}

/// What a file run or a file message calls itself. Feishu spells it three
/// ways, depending on which client uploaded it.
fn named(value: &Value) -> String {
    ["file_name", "title", "text"]
        .iter()
        .find_map(|key| text_of(&value[*key]))
        .unwrap_or_default()
        .to_string()
}

/// The longest run of backticks in `text`, which is what a fence has to beat.
fn backticks(text: &str) -> usize {
    let (mut longest, mut run) = (0, 0);
    for character in text.chars() {
        run = match character == '`' {
            true => run + 1,
            false => 0,
        };
        longest = longest.max(run);
    }
    longest
}

/// A string field with something in it, trimmed.
fn text_of(value: &Value) -> Option<&str> {
    some(value.as_str()?)
}

/// A string with something in it, trimmed.
fn some(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|text| !text.is_empty())
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "ou_bot";

    fn spoke(message_type: &str, content: Value, mentions: Value) -> Spoken {
        spoken("om_1", message_type, &content.to_string(), &mentions, ME)
    }

    fn alone(message_type: &str, content: Value) -> Spoken {
        spoke(message_type, content, json!([]))
    }

    fn resource(key: &str, kind: Kind) -> Resource {
        Resource {
            message: "om_1".into(),
            key: key.into(),
            kind,
        }
    }

    fn file(name: &str) -> Kind {
        Kind::File { name: name.into() }
    }

    fn video(name: &str) -> Kind {
        Kind::Video { name: name.into() }
    }

    fn two() -> Value {
        json!([
            { "key": "@_user_1", "id": { "open_id": ME }, "name": "bingo" },
            { "key": "@_user_2", "id": { "open_id": "ou_wei" }, "name": "Wei" },
        ])
    }

    #[test]
    fn a_text_message_is_its_words_with_the_mentions_read_as_a_person_read_them() {
        let spoken = spoke("text", json!({ "text": "@_user_1 ask @_user_2" }), two());
        assert_eq!(spoken.text, "ask @Wei");
        assert!(spoken.resources.is_empty());
    }

    /// `@_user_1` is a prefix of `@_user_10`, so the order the placeholders are
    /// replaced in is the difference between eleven names and one mangled one.
    #[test]
    fn a_chat_with_ten_mentions_reads_every_one_of_them() {
        let mentions: Value = (1..=11)
            .map(|n| json!({ "key": format!("@_user_{n}"), "id": { "open_id": format!("ou_{n}") }, "name": format!("p{n}") }))
            .collect();
        let text = "@_user_1 @_user_10 @_user_11 @_user_2";
        let spoken = spoke("text", json!({ "text": text }), mentions);
        assert_eq!(spoken.text, "@p1 @p10 @p11 @p2");
    }

    #[test]
    fn everyone_is_named_even_though_the_mentions_never_list_them() {
        let spoken = alone("text", json!({ "text": "@_all stand-up" }));
        assert_eq!(spoken.text, "@all stand-up");
    }

    #[test]
    fn a_post_carries_every_run_a_post_can_hold() {
        let spoken = spoke(
            "post",
            json!({
                "title": "Release notes",
                "content": [
                    [
                        { "tag": "text", "text": "see " },
                        { "tag": "a", "text": "the plan", "href": "https://x/y" },
                        { "tag": "text", "text": " and " },
                        { "tag": "a", "href": "https://z" },
                    ],
                    [
                        { "tag": "at", "user_id": "@_user_1" },
                        { "tag": "text", "text": " ask " },
                        { "tag": "at", "user_id": "@_user_2" },
                        { "tag": "text", "text": " and " },
                        { "tag": "at", "user_id": "@_all" },
                    ],
                    [{ "tag": "code_block", "language": "rust", "text": "fn main() {}" }],
                    [{ "tag": "hr" }],
                    [
                        { "tag": "emotion", "emoji_type": "SMILE" },
                        { "tag": "emotion", "text": "OK" },
                    ],
                    [
                        { "tag": "img", "image_key": "img_a" },
                        { "tag": "file", "file_key": "file_a", "file_name": "notes.md" },
                    ],
                    [
                        { "tag": "audio", "file_key": "audio_a", "duration": 1200 },
                        { "tag": "media", "file_key": "media_a", "file_name": "clip.mp4" },
                    ],
                    [{ "tag": "a_tag_a_later_feishu_adds", "text": "and its words" }],
                ],
            }),
            two(),
        );
        assert_eq!(
            spoken.text,
            "Release notes\n\
             see [the plan](https://x/y) and [https://z](https://z)\n\
             ask @Wei and @all\n\
             ```rust\nfn main() {}\n```\n\
             ---\n\
             :SMILE::OK:\n\
             \n\
             \n\
             and its words"
        );
        assert_eq!(
            spoken.resources,
            vec![
                resource("img_a", Kind::Picture),
                resource("file_a", file("notes.md")),
                resource("audio_a", Kind::Audio),
                resource("media_a", video("clip.mp4")),
            ]
        );
    }

    /// A fence that the file inside it could close is a file that arrives
    /// half-read.
    #[test]
    fn a_code_block_full_of_fences_is_fenced_wider() {
        let spoken = alone(
            "post",
            json!({ "content": [[{ "tag": "code_block", "text": "```\nnested\n```" }]] }),
        );
        assert_eq!(spoken.text, "````\n```\nnested\n```\n````");
    }

    #[test]
    fn a_post_keyed_by_locale_is_still_a_post() {
        let spoken = alone(
            "post",
            json!({ "zh_cn": { "title": "标题", "content": [[{ "tag": "text", "text": "hello" }]] } }),
        );
        assert_eq!(spoken.text, "标题\nhello");
    }

    #[test]
    fn a_picture_is_a_message_with_no_words_and_one_key_to_fetch() {
        let spoken = alone("image", json!({ "image_key": "img_1" }));
        assert_eq!(spoken.text, "");
        assert_eq!(spoken.resources, vec![resource("img_1", Kind::Picture)]);
    }

    #[test]
    fn a_file_an_audio_and_a_media_message_are_each_one_resource() {
        let file_message = alone(
            "file",
            json!({ "file_key": "file_1", "file_name": "report.pdf" }),
        );
        assert_eq!(file_message.text, "");
        assert_eq!(
            file_message.resources,
            vec![resource("file_1", file("report.pdf"))]
        );

        let voice = alone("audio", json!({ "file_key": "audio_1", "duration": 4200 }));
        assert_eq!(voice.text, "");
        assert_eq!(voice.resources, vec![resource("audio_1", Kind::Audio)]);

        let clip = alone(
            "media",
            json!({
                "file_key": "media_1", "file_name": "demo.mp4",
                "duration": 9000, "image_key": "img_cover",
            }),
        );
        assert_eq!(clip.text, "");
        assert_eq!(
            clip.resources,
            vec![resource("media_1", video("demo.mp4"))],
            "the cover is a thumbnail of the video, not a picture somebody sent"
        );
    }

    #[test]
    fn a_card_is_what_it_calls_itself_what_it_says_and_what_can_be_pressed() {
        let spoken = alone(
            "interactive",
            json!({
                "card": {
                    "header": { "title": { "tag": "plain_text", "content": "Deploy" } },
                    "elements": [
                        { "tag": "markdown", "content": "**staging** is ready" },
                        { "tag": "div", "text": { "tag": "lark_md", "content": "**staging** is ready" } },
                        { "tag": "action", "actions": [
                            { "tag": "button", "text": { "content": "Ship it" } },
                            { "tag": "overflow", "options": [{ "text": "Later" }] },
                        ] },
                    ],
                },
            }),
        );
        assert_eq!(
            spoken.text, "Deploy\n**staging** is ready\nActions: Ship it, Later",
            "a line a card repeats is read once"
        );
    }

    #[test]
    fn a_card_with_nothing_readable_in_it_still_says_that_a_card_arrived() {
        assert_eq!(
            alone("interactive", json!({ "config": {} })).text,
            "[interactive]"
        );
    }

    #[test]
    fn a_card_that_is_a_whole_dashboard_is_read_down_to_its_top() {
        let elements: Value = (0..40)
            .map(|n| json!({ "tag": "markdown", "content": format!("line {n}") }))
            .collect();
        let spoken = alone("interactive", json!({ "elements": elements }));
        assert_eq!(spoken.text.lines().count(), CARD_LINES);
        assert!(spoken.text.starts_with("line 0\n"), "{}", spoken.text);
    }

    #[test]
    fn a_shared_chat_names_the_chat_by_whichever_half_it_carries() {
        assert_eq!(
            alone(
                "share_chat",
                json!({ "chat_name": "Ops", "chat_id": "oc_9" })
            )
            .text,
            "Shared chat: Ops (oc_9)"
        );
        assert_eq!(
            alone("share_chat", json!({ "chat_name": "Ops" })).text,
            "Shared chat: Ops"
        );
        assert_eq!(
            alone("share_chat", json!({ "chat_id": "oc_9" })).text,
            "Shared chat: oc_9"
        );
        assert_eq!(alone("share_chat", json!({})).text, "[share_chat]");
    }

    #[test]
    fn a_merged_forward_is_its_title_until_the_bundle_is_fetched() {
        assert_eq!(
            alone("merge_forward", json!({ "title": "Tuesday's thread" })).text,
            "Tuesday's thread"
        );
        assert_eq!(alone("merge_forward", json!({})).text, "[merge_forward]");
    }

    /// The M13 non-goal closed: a sticker in a direct chat used to be dropped
    /// on the floor, and the person who sent it was answered with silence.
    #[test]
    fn a_sticker_and_a_type_this_build_has_never_heard_of_both_arrive() {
        assert_eq!(
            alone("sticker", json!({ "file_key": "sticker_1" })).text,
            "[sticker]"
        );
        assert_eq!(alone("system", json!({ "template": "x" })).text, "[system]");
    }

    #[test]
    fn content_that_is_not_json_is_still_what_somebody_typed() {
        assert_eq!(
            spoken("om_1", "text", "not json at all", &json!([]), ME).text,
            "not json at all"
        );
        assert_eq!(
            spoken("om_1", "image", "not json at all", &json!([]), ME).text,
            "[image]"
        );
    }
}
