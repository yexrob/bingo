use bingo_sdk::{CatalogEntry, Effort};
#[derive(Clone, Debug, Default)]
pub struct Chat {
    pub effort: Option<Effort>,
    pub current: String,
    pub used: u64,
    pub pictures: bool,
}
pub fn tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{}M", value as f64 / 1_000_000.0)
    } else if value >= 1000 {
        format!("{}k", value / 1000)
    } else {
        value.to_string()
    }
}
pub fn efforts(model: &CatalogEntry) -> Vec<Effort> {
    levels(model).into_iter().flatten().collect()
}
pub fn levels(model: &CatalogEntry) -> Vec<Option<Effort>> {
    let levels = model.meta["efforts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| {
            if value.is_null() {
                return Some(None);
            }
            serde_json::from_value::<Effort>(value.clone())
                .ok()
                .map(Some)
                .or_else(|| value.as_str().and_then(Effort::spoken))
        })
        .collect::<Vec<_>>();
    if levels.is_empty() {
        vec![None]
    } else {
        levels
    }
}
pub fn clamp(model: &CatalogEntry, effort: Option<Effort>) -> Option<Effort> {
    let levels = levels(model);
    if levels.contains(&effort) {
        effort
    } else if levels.contains(&Some(Effort::High)) {
        Some(Effort::High)
    } else {
        levels.last().copied().flatten()
    }
}
pub fn card(model: &CatalogEntry, chat: &Chat, effort: Option<Effort>, keep: bool) -> Vec<String> {
    let meta = &model.meta;
    let mut lines = vec![
        model
            .id
            .split_once('/')
            .map_or(model.id.as_str(), |(_, m)| m)
            .into(),
        String::new(),
        context(known(&meta["context"]), chat.used),
        format!(
            "output    {}",
            known(&meta["output"])
                .map(|v| format!("{} per answer", tokens(v)))
                .unwrap_or_else(|| "unknown".into())
        ),
        format!("price     {}", price(meta)),
        format!(
            "images    {}{}",
            if meta["images"] == true {
                "reads them"
            } else if meta["images"] == false {
                "can't see them"
            } else {
                "unknown"
            },
            if chat.pictures {
                " · this chat has pictures"
            } else {
                ""
            }
        ),
        format!(
            "source    {}",
            match meta["limitsSource"]
                .as_str()
                .or_else(|| meta["source"].as_str())
            {
                Some("catalogue") => "models.dev",
                Some("endpoint") => "the server",
                Some("configured") => "your settings",
                Some(source) => source,
                None => "unknown",
            }
        ),
        String::new(),
    ];
    append_controls(&mut lines, model, chat, effort, keep);
    lines
}
/// A limit an endpoint reports as zero is a limit nobody knows.
pub fn known(limit: &serde_json::Value) -> Option<u64> {
    limit.as_u64().filter(|v| *v > 0)
}
fn context(window: Option<u64>, used: u64) -> String {
    let Some(window) = window else {
        return "context   unknown until it answers".into();
    };
    let fill = ((used as f64 / window as f64 * 10.0).round() as usize).min(10);
    format!(
        "context   {}{} {} · this chat {}",
        "▰".repeat(fill),
        "▱".repeat(10 - fill),
        tokens(window),
        tokens(used)
    )
}
fn price(meta: &serde_json::Value) -> String {
    match (
        meta["price"]["input"].as_f64(),
        meta["price"]["output"].as_f64(),
    ) {
        (Some(input), Some(output)) => format!("${input} in · ${output} out per M tokens"),
        _ => "unknown · not in the catalogue".into(),
    }
}
fn append_controls(
    lines: &mut Vec<String>,
    model: &CatalogEntry,
    chat: &Chat,
    effort: Option<Effort>,
    keep: bool,
) {
    let meta = &model.meta;
    let levels = efforts(model);
    lines.push(if levels.is_empty() {
        "thinking  no effort dial".into()
    } else {
        format!(
            "thinking  {}",
            levels
                .iter()
                .map(|e| if Some(*e) == clamp(model, effort) {
                    format!("[{}]", e.name())
                } else {
                    e.name().into()
                })
                .collect::<Vec<_>>()
                .join(" ")
        )
    });
    lines.push(
        if keep {
            "settings  ○ unchanged   ● keep choice"
        } else {
            "settings  ● unchanged   ○ keep choice"
        }
        .into(),
    );
    lines.push("model     next-start default in main sessions".into());
    if crate::label::compacts(model) {
        lines.push("▲ compacts the chat first, then switches".into());
    }
    if chat.pictures && meta["images"] == false {
        lines.push("▲ pictures stay in the log; this model won't see them".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn absent_facts_are_unknown_and_warnings_come_from_the_kernel_and_chat() {
        let mut model = CatalogEntry {
            id: "p/m".into(),
            label: "m".into(),
            meta: json!({}),
        };
        let chat = Chat {
            used: 182000,
            pictures: true,
            ..Default::default()
        };
        assert!(
            card(&model, &chat, None, false)
                .join("\n")
                .contains("unknown · not in the catalogue")
        );
        model.meta =
            json!({"context":128000,"switchAt":115200,"images":false,"efforts":["low","high"]});
        let lines = card(&model, &chat, Some(Effort::High), true).join("\n");
        assert!(
            !lines.contains("compacts"),
            "the kernel decides, not the window"
        );
        model.meta["compacts"] = json!(true);
        let lines = card(&model, &chat, Some(Effort::High), true).join("\n");
        assert!(
            lines.contains("compacts")
                && lines.contains("pictures stay")
                && lines.contains("[high]")
        );
        assert!(!lines.contains("medium"));
        assert!(lines.contains("settings  ○ unchanged   ● keep choice"));
        let ordinary = card(&model, &chat, Some(Effort::High), false).join("\n");
        assert!(ordinary.contains("settings  ● unchanged   ○ keep choice"));
        assert!(!ordinary.contains("here and new sessions"));
        assert!(ordinary.contains("model     next-start default in main sessions"));
    }
}
