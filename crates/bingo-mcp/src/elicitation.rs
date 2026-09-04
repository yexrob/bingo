//! MCP elicitation, the client half: a server asking the person a question
//! through the one door there is (ADR-0039 §1, §3).
//!
//! The mapping ADR-0039 §3 recorded and did not build, built. A form-mode
//! `elicitation/create` (spec 2025-06-18 `client/elicitation`, `mode` added in
//! 2025-11-25) is a flat object of primitive properties, and that is exactly a
//! set of questions asked together: an enum property is a choice, a boolean a
//! two-option question, a string or a number one answered in words. The card's
//! title names the server, because a person must be able to see who is asking.
//!
//! The schema is read as the JSON it arrived as rather than through a typed
//! tree: the spec writes this contract in JSON, the fixtures beside this file
//! quote it, and the shapes a revision adds are properties to read, not types
//! to grow.
//!
//! Nothing nested is answered. The spec keeps the schema flat, so an object,
//! an array or a property of no type this file knows declines the whole
//! request — never a half-answer the server would read as the person's.

use bingo_sdk::{Answer, AnswerSpec, InteractionKind, Question, QuestionOption};
use rmcp::model::{ElicitResult, ElicitationAction};
use serde_json::{Map, Value};

/// What one property wants back, which is what a person's answer is turned
/// into on the way to the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wants {
    Text,
    Number,
    Integer,
    /// One of two: `true` or `false`.
    Bool,
    /// One of the values the property listed.
    Choice,
}

/// One property of a `requestedSchema`: the question a person is asked, and
/// the key and shape the answer goes back under.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub question: Question,
    wants: Wants,
    required: bool,
}

/// A form-mode request, mapped: what the card says it is, and one field per
/// property, in the order the properties come off the wire.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub title: String,
    pub fields: Vec<Field>,
}

/// The answers the kernel will accept for one: the whole form, or nothing.
pub fn answers() -> Vec<AnswerSpec> {
    vec![AnswerSpec::Form, AnswerSpec::Cancel]
}

/// Read a form-mode request. `Err` is why it cannot be put to anybody at all,
/// which the server hears as `decline`.
pub fn form(server: &str, message: &str, schema: &Value) -> Result<Form, String> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| "the requested schema has no properties".to_string())?;
    let required = required_names(schema);
    let fields = properties
        .iter()
        .map(|(name, property)| field(name, property, required.iter().any(|it| it == name)))
        .collect::<Result<Vec<_>, _>>()?;
    if fields.is_empty() {
        return Err("the requested schema asks for nothing".into());
    }
    Ok(Form {
        title: format!("{server}: {message}"),
        fields,
    })
}

fn required_names(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// One property as a question. The words are the property's own: its
/// description where it has one, else its title, else the key the server
/// named it by.
fn field(name: &str, property: &Value, required: bool) -> Result<Field, String> {
    let title = property.get("title").and_then(Value::as_str);
    let words = property
        .get("description")
        .and_then(Value::as_str)
        .or(title)
        .unwrap_or(name)
        .to_string();
    let (wants, options) = shape(name, property)?;
    Ok(Field {
        name: name.to_string(),
        question: Question {
            question: words,
            header: Some(title.unwrap_or(name).to_string()),
            // Words of one's own are the only way to answer a string or a
            // number, and no way at all to answer a set of named values.
            free_text: options.is_empty(),
            options,
            multi: false,
        },
        wants,
        required,
    })
}

/// What kind of property this is, and the options it offers. Anything the
/// spec's flat subset does not cover is refused by name.
fn shape(name: &str, property: &Value) -> Result<(Wants, Vec<QuestionOption>), String> {
    if let Some(options) = choices(property) {
        return Ok((Wants::Choice, options));
    }
    match property.get("type").and_then(Value::as_str) {
        Some("string") => Ok((Wants::Text, Vec::new())),
        Some("number") => Ok((Wants::Number, Vec::new())),
        Some("integer") => Ok((Wants::Integer, Vec::new())),
        Some("boolean") => Ok((Wants::Bool, yes_no(property))),
        Some(other) => Err(format!("{name} is a {other}, which is not a flat property")),
        None => Err(format!("{name} names no type")),
    }
}

/// The values a property lists, as options: `enum` with the optional
/// `enumNames` beside it (2025-06-18), or `oneOf` of `const`/`title` pairs
/// (2025-11-25). The value itself is the option's id, so an answer maps back
/// to what the server must receive.
fn choices(property: &Value) -> Option<Vec<QuestionOption>> {
    if let Some(one_of) = property.get("oneOf").and_then(Value::as_array) {
        let options: Vec<QuestionOption> = one_of
            .iter()
            .filter_map(|choice| {
                let value = choice.get("const")?.as_str()?;
                Some(option(value, choice.get("title").and_then(Value::as_str)))
            })
            .collect();
        return (!options.is_empty()).then_some(options);
    }
    let values = property.get("enum").and_then(Value::as_array)?;
    let names = property.get("enumNames").and_then(Value::as_array);
    let options: Vec<QuestionOption> = values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let value = value.as_str()?;
            let label = names
                .and_then(|names| names.get(index))
                .and_then(Value::as_str);
            Some(option(value, label))
        })
        .collect();
    (!options.is_empty()).then_some(options)
}

/// A boolean is a question of two options, in words rather than in `true` and
/// `false`; the `default` the server named is put first, so the row under the
/// cursor is the one it suggested.
fn yes_no(property: &Value) -> Vec<QuestionOption> {
    let (yes, no) = (option("true", Some("Yes")), option("false", Some("No")));
    match property.get("default").and_then(Value::as_bool) {
        Some(false) => vec![no, yes],
        _ => vec![yes, no],
    }
}

fn option(value: &str, label: Option<&str>) -> QuestionOption {
    QuestionOption {
        id: value.to_string(),
        label: label.unwrap_or(value).to_string(),
        description: None,
        // A server's question is a person's alone to answer: no stance speaks
        // for them here (ADR-0039 §2).
        role: None,
        preview: None,
    }
}

impl Form {
    /// The interaction the door opens for it.
    pub fn kind(&self) -> InteractionKind {
        InteractionKind::Form {
            title: Some(self.title.clone()),
            questions: self.fields.iter().map(|f| f.question.clone()).collect(),
        }
    }

    /// What the person's answer means to the server: the content where every
    /// property the server called for came back, `decline` where one did not
    /// or where the answer was not this form's, `cancel` where the card was
    /// left.
    pub fn result(&self, answer: &Answer) -> ElicitResult {
        let Answer::Form { answers } = answer else {
            return match answer {
                Answer::Cancel => acted(ElicitationAction::Cancel),
                _ => acted(ElicitationAction::Decline),
            };
        };
        let mut content = Map::new();
        for (field, given) in self.fields.iter().zip(answers) {
            if let Some(value) = field.value(given) {
                content.insert(field.name.clone(), value);
            }
        }
        let missing = self
            .fields
            .iter()
            .any(|field| field.required && !content.contains_key(&field.name));
        match missing {
            true => acted(ElicitationAction::Decline),
            false => acted(ElicitationAction::Accept).with_content(Value::Object(content)),
        }
    }
}

impl Field {
    /// What this field's slot of the answer is worth to the server, or nothing
    /// when the person left it — or wrote something the property will not
    /// hold, which is the same thing to a server that asked for a number.
    fn value(&self, given: &Answer) -> Option<Value> {
        match given {
            Answer::Choice { ids } => self.chosen(ids.first()?),
            Answer::Text { text } => self.written(text.trim()),
            _ => None,
        }
    }

    fn chosen(&self, id: &str) -> Option<Value> {
        match self.wants {
            Wants::Bool => Some(Value::Bool(id == "true")),
            Wants::Choice => Some(Value::String(id.to_string())),
            _ => None,
        }
    }

    fn written(&self, text: &str) -> Option<Value> {
        if text.is_empty() {
            return None;
        }
        match self.wants {
            Wants::Text => Some(Value::String(text.to_string())),
            Wants::Integer => text.parse::<i64>().ok().map(Value::from),
            Wants::Number => text
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number),
            // A named value is chosen, never typed: the option's id is what
            // the server must receive and a person's words are not it.
            Wants::Bool | Wants::Choice => None,
        }
    }
}

/// A URL-mode request. The person is shown the full address and consents to
/// opening it; this client never opens it for them and never fetches it, so
/// nothing about the page reaches the model (spec, Safe URL Handling).
pub fn url_kind(server: &str, message: &str, url: &str) -> InteractionKind {
    InteractionKind::Confirm {
        title: format!("{server} wants a page opened"),
        detail: format!("{message}\n{url}\nOpen it yourself to go on."),
    }
}

/// What a URL-mode card was answered with. Consent is `accept` — which says
/// the person agreed to the interaction, not that it is done.
pub fn url_answers() -> Vec<AnswerSpec> {
    vec![AnswerSpec::Confirm, AnswerSpec::Cancel]
}

pub fn url_result(answer: &Answer) -> ElicitResult {
    match answer {
        Answer::Confirm => acted(ElicitationAction::Accept),
        Answer::Cancel => acted(ElicitationAction::Cancel),
        _ => acted(ElicitationAction::Decline),
    }
}

/// Nobody could be asked at all: the fail-closed answer to a question that
/// never reached a person (ADR-0039 §2).
pub fn declined() -> ElicitResult {
    acted(ElicitationAction::Decline)
}

fn acted(action: ElicitationAction) -> ElicitResult {
    ElicitResult::new(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One request as a server sends it, read off the fixture that quotes the
    /// specification.
    fn params(name: &str) -> Value {
        let path = format!("{}/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).expect("a fixture");
        let request: Value = serde_json::from_str(&text).expect("json");
        request["params"].clone()
    }

    fn asked(name: &str) -> Form {
        let params = params(name);
        form(
            "files",
            params["message"].as_str().expect("a message"),
            &params["requestedSchema"],
        )
        .expect("a form")
    }

    /// The words of every question, by the property they belong to. The card
    /// asks them in the order the schema's properties come off the wire, which
    /// is the server's own where the JSON reader keeps it; nothing here leans
    /// on which, so neither does a test.
    fn words(form: &Form) -> std::collections::BTreeMap<String, String> {
        form.fields
            .iter()
            .map(|field| (field.name.clone(), field.question.question.clone()))
            .collect()
    }

    /// One field by the property it belongs to.
    fn at(form: &Form, name: &str) -> Field {
        form.fields
            .iter()
            .find(|field| field.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("a {name} field"))
    }

    /// Every question answered by name, in the order the card asks them.
    fn answered(form: &Form, given: &[(&str, Answer)]) -> Answer {
        Answer::Form {
            answers: form
                .fields
                .iter()
                .map(|field| {
                    given
                        .iter()
                        .find(|(name, _)| *name == field.name)
                        .map(|(_, answer)| answer.clone())
                        .unwrap_or(Answer::Cancel)
                })
                .collect(),
        }
    }

    #[test]
    fn one_string_property_is_one_question_answered_in_words() {
        let form = asked("elicit-username");
        assert_eq!(form.title, "files: Please provide your GitHub username");
        assert_eq!(
            words(&form),
            std::collections::BTreeMap::from([("name".to_string(), "name".to_string())])
        );
        assert!(form.fields[0].question.free_text);
        assert!(form.fields[0].question.options.is_empty());
        assert!(form.fields[0].required);
        assert_eq!(
            form.kind(),
            InteractionKind::Form {
                title: Some("files: Please provide your GitHub username".into()),
                questions: vec![form.fields[0].question.clone()],
            }
        );
    }

    #[test]
    fn the_card_names_the_server_that_is_asking() {
        assert_eq!(
            asked("elicit-contact").title,
            "files: Please provide your contact information"
        );
    }

    #[test]
    fn every_property_is_a_question_wearing_its_own_words() {
        let form = asked("elicit-contact");
        assert_eq!(
            words(&form),
            std::collections::BTreeMap::from([
                ("age".to_string(), "Your age".to_string()),
                ("email".to_string(), "Your email address".to_string()),
                ("name".to_string(), "Your full name".to_string()),
            ])
        );
        let mut required: Vec<&str> = form
            .fields
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name.as_str())
            .collect();
        required.sort_unstable();
        assert_eq!(required, vec!["email", "name"]);
    }

    #[test]
    fn an_enum_is_a_choice_and_a_boolean_is_two_options() {
        let form = asked("elicit-choices");
        let channel = at(&form, "channel");
        assert_eq!(channel.question.header.as_deref(), Some("Channel"));
        assert_eq!(
            channel
                .question
                .options
                .iter()
                .map(|o| (o.id.as_str(), o.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("stable", "Stable"),
                ("beta", "Beta"),
                ("nightly", "Nightly")
            ],
            "the value is the id and enumNames are what a person reads"
        );
        assert!(!channel.question.free_text, "a named value is chosen");

        let colour = at(&form, "colour");
        assert_eq!(
            colour
                .question
                .options
                .iter()
                .map(|o| (o.id.as_str(), o.label.as_str()))
                .collect::<Vec<_>>(),
            vec![("#FF0000", "Red"), ("#00FF00", "Green")],
            "oneOf const/title says the same thing the other way"
        );

        let sign = at(&form, "sign");
        assert_eq!(
            sign.question
                .options
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>(),
            vec!["false", "true"],
            "the default the server named is the row under the cursor"
        );
    }

    #[test]
    fn a_shape_the_spec_does_not_allow_declines_the_whole_request() {
        let params = params("elicit-nested");
        let refused = form("files", "why", &params["requestedSchema"]).expect_err("refused");
        assert!(
            refused.contains("target") || refused.contains("colours"),
            "{refused}"
        );
        assert_eq!(declined().action, ElicitationAction::Decline);
        assert!(
            form("files", "why", &serde_json::json!({ "type": "object" })).is_err(),
            "a schema with no properties asks for nothing"
        );
    }

    #[test]
    fn what_the_person_answered_is_the_content_the_server_receives() {
        let form = asked("elicit-choices");
        let chose = |id: &str| Answer::Choice {
            ids: vec![id.into()],
        };
        let result = form.result(&answered(
            &form,
            &[
                ("channel", chose("beta")),
                ("colour", chose("#00FF00")),
                ("jobs", Answer::Text { text: "8".into() }),
                ("sign", chose("true")),
            ],
        ));
        assert_eq!(result.action, ElicitationAction::Accept);
        assert_eq!(
            result.content,
            Some(serde_json::json!({
                "channel": "beta",
                "colour": "#00FF00",
                "jobs": 8,
                "sign": true
            }))
        );
    }

    #[test]
    fn a_question_the_person_left_is_left_out_and_a_required_one_declines() {
        let form = asked("elicit-choices");
        let chose = |id: &str| Answer::Choice {
            ids: vec![id.into()],
        };
        // `colour` and `sign` are optional; leaving them out is still an answer.
        let kept = form.result(&answered(
            &form,
            &[
                ("channel", chose("stable")),
                ("jobs", Answer::Text { text: "1".into() }),
            ],
        ));
        assert_eq!(kept.action, ElicitationAction::Accept);
        assert_eq!(
            kept.content,
            Some(serde_json::json!({ "channel": "stable", "jobs": 1 }))
        );
        // `jobs` is not: a server that asked for it hears a decline.
        let short = form.result(&answered(&form, &[("channel", chose("stable"))]));
        assert_eq!(short.action, ElicitationAction::Decline);
        assert_eq!(short.content, None);
    }

    #[test]
    fn a_number_is_checked_before_it_is_sent_back() {
        let form = asked("elicit-choices");
        let chose = |id: &str| Answer::Choice {
            ids: vec![id.into()],
        };
        let typed = |text: &str| {
            form.result(&answered(
                &form,
                &[
                    ("channel", chose("stable")),
                    ("jobs", Answer::Text { text: text.into() }),
                ],
            ))
        };
        assert_eq!(
            typed("many").action,
            ElicitationAction::Decline,
            "words are not an integer, and the property was required"
        );
        assert_eq!(
            typed("2.5").action,
            ElicitationAction::Decline,
            "and neither is a fraction"
        );
        assert_eq!(typed("64").action, ElicitationAction::Accept);
    }

    #[test]
    fn leaving_the_card_cancels_and_anything_else_declines() {
        let form = asked("elicit-username");
        assert_eq!(
            form.result(&Answer::Cancel).action,
            ElicitationAction::Cancel
        );
        assert_eq!(
            form.result(&Answer::Deny { feedback: None }).action,
            ElicitationAction::Decline
        );
        assert_eq!(answers(), vec![AnswerSpec::Form, AnswerSpec::Cancel]);
    }

    #[test]
    fn a_url_request_shows_the_whole_address_and_asks_before_anything_opens() {
        let params = params("elicit-url");
        let kind = url_kind(
            "files",
            params["message"].as_str().expect("a message"),
            params["url"].as_str().expect("a url"),
        );
        let InteractionKind::Confirm { title, detail } = &kind else {
            panic!("expected a confirmation, got {kind:?}");
        };
        assert_eq!(title, "files wants a page opened");
        assert!(
            detail.contains("https://mcp.example.com/ui/set_api_key"),
            "{detail}"
        );
        assert!(
            detail.contains("Please provide your API key to continue."),
            "{detail}"
        );
        assert_eq!(
            url_result(&Answer::Confirm).action,
            ElicitationAction::Accept
        );
        assert_eq!(
            url_result(&Answer::Cancel).action,
            ElicitationAction::Cancel
        );
        assert_eq!(
            url_result(&Answer::Text { text: "x".into() }).action,
            ElicitationAction::Decline
        );
        assert_eq!(url_answers(), vec![AnswerSpec::Confirm, AnswerSpec::Cancel]);
    }
}
