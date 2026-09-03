//! What of bingo's tools an agent is offered, derived rather than listed.
//!
//! ADR-0036 §1: the offer is the request's own tool list — the set the kernel
//! already assembles for whatever model this session runs, child-filtered for
//! a sub-agent seat and whole for a top-level one. Nothing here selects a tool
//! *in*; the only list in this crate is the one that keeps tools *out*, and it
//! is spelled below.
//!
//! Two subtractions, and both are about a tool being served twice rather than
//! about it being unfit: the machine's hands the agent already brought, and
//! the servers a person's own rows hand it directly (§4). A third case is not
//! a subtraction but a replacement: a row that names its own `tools` gets
//! those, and the derivation stands aside.
//!
//! Pure, and tested against invented tools: what crosses is decided by shape
//! and by name, never by asking a tool anything.

use std::collections::BTreeSet;

use bingo_sdk::{CatalogEntry, ToolSpec};
use serde_json::Value;

/// The tools that do not cross, whatever the request offered.
///
/// The fs, bash and web tools are the machine's hands, which the agent brought
/// with it under its own permission words (ADR-0035 §5) — a second pair over a
/// socket is the same hands, slower. `SpawnAgent` and `AskUserQuestion` are the
/// two a child seat is never given either, for the reasons `bingo-agents`
/// spells out where it keeps them back.
///
/// This is the whole of what this crate knows about tool names.
pub const NOT_THE_AGENTS: [&str; 12] = [
    "AskUserQuestion",
    "Bash",
    "BashOutput",
    "Edit",
    "Glob",
    "Grep",
    "KillShell",
    "Read",
    "SpawnAgent",
    "WebFetch",
    "WebSearch",
    "Write",
];

/// What a catalogue shows beside a sourced tool: which server it came from.
/// The same word `bingo-mcp` files it under, read rather than parsed out of
/// the tool's name, so a server or a tool whose own name holds the separator
/// is still matched.
const SERVER: &str = "server";

/// The catalogue's three additions to a tool's own `meta`. They are facts
/// about the tool that the spec already carries in its own fields, so they do
/// not travel back into one.
const CATALOGUED: [&str; 3] = ["description", "inputSchema", "traits"];

/// The offer, derived: everything the turn was given, less the hands the agent
/// brought and less whatever its own rows already serve it.
pub fn derived(specs: &[ToolSpec], forwarded: &BTreeSet<String>) -> Vec<ToolSpec> {
    specs
        .iter()
        .filter(|spec| !NOT_THE_AGENTS.contains(&spec.name.as_str()))
        .filter(|spec| !served_by(spec, forwarded))
        .cloned()
        .collect()
}

/// The offer a row chose for itself, and the names it asked for that nothing
/// answers to. An explicit list replaces the derivation whole — including the
/// exclusion, because it is the person's own word on their own machine
/// (ADR-0036 §6) — and is checked for nothing but existence.
pub fn chosen(specs: &[ToolSpec], names: &[String]) -> (Vec<ToolSpec>, Vec<String>) {
    let asked: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let kept: Vec<ToolSpec> = specs
        .iter()
        .filter(|spec| asked.contains(spec.name.as_str()))
        .cloned()
        .collect();
    let held: BTreeSet<&str> = kept.iter().map(|spec| spec.name.as_str()).collect();
    let missing = names
        .iter()
        .filter(|name| !held.contains(name.as_str()))
        .cloned()
        .collect();
    (kept, missing)
}

/// Whether a forwarded server already serves this tool.
fn served_by(spec: &ToolSpec, forwarded: &BTreeSet<String>) -> bool {
    spec.meta
        .get(SERVER)
        .and_then(Value::as_str)
        .is_some_and(|server| forwarded.contains(server))
}

/// The tools catalogue read back as specs — the bootstrap before a first
/// request has said what this session's offer is (ADR-0036 §1). The three
/// facts the catalogue adds are lifted back into the fields they came from;
/// what is left is the tool's own `meta`, which is where `server` lives.
pub fn from_catalog(entries: Vec<CatalogEntry>) -> Vec<ToolSpec> {
    entries.into_iter().map(spec).collect()
}

fn spec(entry: CatalogEntry) -> ToolSpec {
    let mut meta = match entry.meta {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let description = meta.remove("description");
    let input_schema = meta.remove("inputSchema");
    for catalogued in CATALOGUED {
        meta.remove(catalogued);
    }
    ToolSpec {
        name: entry.id,
        description: description
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input_schema: input_schema.unwrap_or(Value::Null),
        meta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("what {name} does"),
            input_schema: json!({ "type": "object" }),
            meta: serde_json::Map::new(),
        }
    }

    fn sourced(server: &str, tool: &str) -> ToolSpec {
        let mut spec = spec(&format!("mcp__{server}__{tool}"));
        spec.meta
            .insert(SERVER.into(), Value::String(server.into()));
        spec
    }

    fn names(specs: &[ToolSpec]) -> Vec<&str> {
        specs.iter().map(|spec| spec.name.as_str()).collect()
    }

    /// The offer is what the turn was given, and the derivation adds nothing:
    /// a tool nobody in this crate has heard of crosses on the strength of the
    /// request alone.
    #[test]
    fn a_tool_this_crate_never_heard_of_is_offered() {
        let offered = derived(
            &[spec("SendMessage"), spec("Sing"), spec("TaskCreate")],
            &BTreeSet::new(),
        );
        assert_eq!(names(&offered), ["SendMessage", "Sing", "TaskCreate"]);
    }

    /// The hands the agent brought with it do not cross a second time.
    #[test]
    fn the_machines_own_hands_stay_on_the_agents_side() {
        let given: Vec<ToolSpec> = NOT_THE_AGENTS
            .iter()
            .map(|name| spec(name))
            .chain([spec("SendMessage")])
            .collect();
        assert_eq!(names(&derived(&given, &BTreeSet::new())), ["SendMessage"]);
    }

    /// A server the agent dials itself must not also be served over the
    /// bridge: nothing is offered twice (ADR-0036 §4).
    #[test]
    fn a_forwarded_servers_tools_leave_the_offer() {
        let given = [
            spec("SendMessage"),
            sourced("files", "read"),
            sourced("weather", "today"),
        ];
        let forwarded = BTreeSet::from(["files".to_string()]);
        assert_eq!(
            names(&derived(&given, &forwarded)),
            ["SendMessage", "mcp__weather__today"],
            "only the forwarded server's tools go"
        );
        assert_eq!(
            names(&derived(&given, &BTreeSet::new())).len(),
            3,
            "and with nothing forwarded they are all on the bridge"
        );
    }

    /// Which server a tool came from is read from the fact the catalogue
    /// holds, not guessed from the name: a server whose own name carries the
    /// separator would defeat any parse of it.
    #[test]
    fn a_servers_tools_are_found_by_the_fact_not_by_the_name() {
        let odd = sourced("a__b", "c__d");
        assert_eq!(odd.name, "mcp__a__b__c__d");
        let forwarded = BTreeSet::from(["a__b".to_string()]);
        assert!(derived(&[odd], &forwarded).is_empty());
    }

    /// A row that names its tools gets those and only those — including one
    /// the derivation would have kept back, because the person said so.
    #[test]
    fn an_explicit_list_replaces_the_derivation_whole() {
        let given = [spec("SendMessage"), spec("Read"), spec("TaskCreate")];
        let (kept, missing) = chosen(&given, &["Read".into(), "SendMessage".into()]);
        assert_eq!(names(&kept), ["SendMessage", "Read"]);
        assert!(missing.is_empty());
    }

    /// Checked for existence and nothing else: what is there is offered, what
    /// is not is named back so somebody can say it.
    #[test]
    fn a_name_nothing_answers_to_is_reported_not_refused() {
        let (kept, missing) = chosen(
            &[spec("SendMessage")],
            &["SendMessage".into(), "Yodel".into()],
        );
        assert_eq!(names(&kept), ["SendMessage"]);
        assert_eq!(missing, ["Yodel"]);
    }

    /// The catalogue's entry, read back as the spec it was made from
    /// (ADR-0036 §1) — the schema whole, the description in its own field, and
    /// the tool's own `meta` still carrying which server it came from.
    #[test]
    fn a_catalogue_entry_reads_back_as_the_spec_it_was_made_from() {
        let entry = CatalogEntry {
            id: "mcp__files__read".into(),
            label: "mcp__files__read".into(),
            meta: json!({
                "server": "files",
                "description": "Read a file.",
                "inputSchema": { "type": "object", "properties": { "path": { "type": "string" } } },
                "traits": { "readOnly": true, "trusted": false }
            }),
        };
        let read = from_catalog(vec![entry]);
        assert_eq!(read[0].name, "mcp__files__read");
        assert_eq!(read[0].description, "Read a file.");
        assert_eq!(read[0].input_schema["properties"]["path"]["type"], "string");
        assert_eq!(read[0].meta[SERVER], json!("files"));
        assert!(
            !read[0].meta.contains_key("traits"),
            "what the catalogue added is not the tool's own meta"
        );
    }

    /// A catalogue that says nothing about a tool still yields a tool: an
    /// offer with no schema is better than a tool the agent never sees.
    #[test]
    fn an_entry_with_nothing_beside_it_is_still_a_tool() {
        let read = from_catalog(vec![CatalogEntry {
            id: "Sing".into(),
            label: "Sing".into(),
            meta: Value::Null,
        }]);
        assert_eq!(read[0].name, "Sing");
        assert_eq!(read[0].description, "");
        assert!(read[0].meta.is_empty());
    }
}
