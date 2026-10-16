use super::*;
/// `vouched`: the kernel says browse's own built-in drew the keys and gave
/// this answer, so a line only the person may write may run from it.
pub fn result(ui: &mut Ui, tree: &Tree, action: &Action, vouched: bool, now: Now) -> Vec<Effect> {
    if action.name != "surface:browse" || action.args["handler"].as_str() != request(ui) {
        return vec![];
    }
    let value = &action.args["value"];
    if let Some(message) = value["notice"].as_str() {
        ui.notify(Level::Warn, message, now.instant);
    }
    if value["close"] == true {
        ui.focus.close(now.instant);
    }
    if let Some(line) = value["run"].as_str() {
        if tree.viewed().busy() {
            ui.notify(Level::Warn, BUSY, now.instant);
            return vec![];
        }
        ui.focus.close(now.instant);
        if !vouched && crate::commands::privileged(&ui.catalogs.specs(), line) {
            ui.notify(Level::Warn, crate::commands::unvouched(line), now.instant);
            return vec![];
        }
        return crate::input::command_line(ui, tree, now, line.into());
    }
    for name in ["pager", "copy"] {
        if let Some(value) = value.get(name)
            && let Some(item) = selected_item(ui, tree, value)
        {
            if name == "copy" {
                return vec![Effect::Copy(
                    crate::pager::whole(
                        tree.viewed(),
                        item,
                        usize::from(ui.shown.canvas.area().width),
                    )
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
                )];
            }
            let page = crate::pager::page::Page::item(item.id.clone());
            crate::pager::page::open(ui, tree, page, now);
        }
    }
    Vec::new()
}
fn selected_item<'a>(ui: &Ui, tree: &'a Tree, value: &serde_json::Value) -> Option<&'a Item> {
    let state = tree.viewed();
    if let Some(id) = value.as_str() {
        return state.transcript.get(&ItemId::from_raw(id));
    }
    let props = current(ui)?;
    let turn = props
        .turns
        .iter()
        .find(|t| Some(&t.id) == props.selected.as_ref())?;
    state.transcript.items().get(turn.first).map(AsRef::as_ref)
}
