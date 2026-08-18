//! The published contract: a deterministic Draft 7 schema bundle generated from
//! the Rust types.
//!
//! A GUI generates its TypeScript from this rather than maintaining a
//! handwritten copy of the same interfaces, so the bundle is committed and CI
//! fails on unreviewed drift. Generation is a pure function of the types: no
//! timestamps, no paths, no map iteration order that could differ between runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::schema::RootSchema;
use serde::Serialize;

use crate::app_server::AppServerError;
use crate::app_server::protocol::envelope::{
    ClientNotificationFrame, NotificationFrame, PROTOCOL_MAJOR, PROTOCOL_MINOR, RequestFrame,
    ResponseFrame,
};
use crate::app_server::protocol::error::{ProtocolErrorKind, RpcError};
use crate::app_server::protocol::notifications::notification_schemas;
use crate::app_server::protocol::requests::method_schemas;

/// The bundle's own version. Bumped when the layout changes, not when a schema
/// inside it does.
pub const BUNDLE_VERSION: u32 = 1;

const MANIFEST: &str = "manifest.json";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SchemaRef {
    id: String,
    file: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MethodEntry {
    method: &'static str,
    direction: &'static str,
    params: SchemaRef,
    result: SchemaRef,
    errors: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotificationEntry {
    method: &'static str,
    direction: &'static str,
    params: SchemaRef,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvelopeEntry {
    name: &'static str,
    direction: &'static str,
    schema: SchemaRef,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEntry {
    bingo_code: &'static str,
    code: i32,
    scope: String,
    recoverable: bool,
    message: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    bundle_version: u32,
    protocol: ProtocolEntry,
    schema_dialect: &'static str,
    /// The shared definitions every schema below refers to.
    definitions: SchemaRef,
    transport: TransportEntry,
    envelopes: Vec<EnvelopeEntry>,
    methods: Vec<MethodEntry>,
    notifications: Vec<NotificationEntry>,
    errors: Vec<ErrorEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolEntry {
    major: u32,
    minor: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransportEntry {
    framing: &'static str,
    /// Failures that belong to the connection rather than to any one method.
    connection_errors: Vec<&'static str>,
}

const DIALECT: &str = "http://json-schema.org/draft-07/schema#";

fn slug(method: &str) -> String {
    method.replace('/', ".")
}

fn schema_id(name: &str) -> String {
    format!("urn:bingo:app-server:v{PROTOCOL_MAJOR}:{name}")
}

fn reference(directory: &str, name: &str) -> SchemaRef {
    SchemaRef {
        id: schema_id(name),
        file: format!("{directory}/{name}.json"),
    }
}

/// The scope's wire form, read from its own serialization so the manifest
/// cannot describe a scope the protocol does not emit.
fn scope_name(kind: ProtocolErrorKind) -> Result<String, AppServerError> {
    Ok(serde_json::to_value(kind.scope())?
        .as_str()
        .unwrap_or_default()
        .to_string())
}

/// Build the whole bundle in memory: relative file path to file contents, plus
/// the manifest. Writing is a separate step so the drift guard can compare
/// without touching the working tree.
pub fn bundle() -> Result<BTreeMap<PathBuf, String>, AppServerError> {
    let mut files: BTreeMap<PathBuf, String> = BTreeMap::new();

    let envelopes = vec![
        (
            "request",
            "clientToServer",
            "envelope.request",
            schemars::schema_for!(RequestFrame),
        ),
        (
            "clientNotification",
            "clientToServer",
            "envelope.clientNotification",
            schemars::schema_for!(ClientNotificationFrame),
        ),
        (
            "response",
            "serverToClient",
            "envelope.response",
            schemars::schema_for!(ResponseFrame),
        ),
        (
            "notification",
            "serverToClient",
            "envelope.notification",
            schemars::schema_for!(NotificationFrame),
        ),
        (
            "error",
            "serverToClient",
            "envelope.error",
            schemars::schema_for!(RpcError),
        ),
    ];

    let mut shared: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut envelope_entries = Vec::new();
    for (name, direction, file, schema) in envelopes {
        let reference = reference("envelopes", file);
        files.insert(
            PathBuf::from(&reference.file),
            render(schema, &reference.id, &mut shared)?,
        );
        envelope_entries.push(EnvelopeEntry {
            name,
            direction,
            schema: reference,
        });
    }

    let mut method_entries = Vec::new();
    for schemas in method_schemas() {
        let stem = slug(schemas.method.as_str());
        let params = reference("methods", &format!("{stem}.params"));
        let result = reference("methods", &format!("{stem}.result"));
        files.insert(
            PathBuf::from(&params.file),
            render(schemas.params, &params.id, &mut shared)?,
        );
        files.insert(
            PathBuf::from(&result.file),
            render(schemas.result, &result.id, &mut shared)?,
        );
        method_entries.push(MethodEntry {
            method: schemas.method.as_str(),
            direction: "clientToServer",
            params,
            result,
            errors: schemas
                .method
                .declared_errors()
                .iter()
                .map(|kind| kind.bingo_code())
                .collect(),
        });
    }

    let mut notification_entries = Vec::new();
    for (method, schema) in notification_schemas() {
        let params = reference("notifications", &format!("{}.params", slug(method)));
        files.insert(
            PathBuf::from(&params.file),
            render(schema, &params.id, &mut shared)?,
        );
        notification_entries.push(NotificationEntry {
            method,
            direction: "serverToClient",
            params,
        });
    }

    files.insert(
        PathBuf::from(SHARED),
        to_json(&serde_json::json!({
            "$schema": DIALECT,
            "$id": schema_id("definitions"),
            DEFINITIONS: shared,
        }))?,
    );

    let manifest = Manifest {
        bundle_version: BUNDLE_VERSION,
        protocol: ProtocolEntry {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
        },
        schema_dialect: DIALECT,
        definitions: SchemaRef {
            id: schema_id("definitions"),
            file: SHARED.to_string(),
        },
        transport: TransportEntry {
            framing: "ndjson",
            connection_errors: vec![
                ProtocolErrorKind::FrameTooLarge.bingo_code(),
                ProtocolErrorKind::ClientTooSlow.bingo_code(),
            ],
        },
        envelopes: envelope_entries,
        methods: method_entries,
        notifications: notification_entries,
        errors: ProtocolErrorKind::ALL
            .iter()
            .map(|kind| {
                Ok(ErrorEntry {
                    bingo_code: kind.bingo_code(),
                    code: kind.code(),
                    scope: scope_name(*kind)?,
                    recoverable: kind.recoverable(),
                    message: kind.message(),
                })
            })
            .collect::<Result<Vec<_>, AppServerError>>()?,
    };
    files.insert(PathBuf::from(MANIFEST), to_json(&manifest)?);
    Ok(files)
}

/// Serialize one schema, hoisting its definitions into the bundle's shared file.
///
/// Inlining them per file would repeat a session snapshot in a dozen places, so
/// a renamed field would land as a dozen diffs of the same change. Names come
/// from the type, so two schemas that define the same name define it the same
/// way; the drift guard checks that rather than assuming it.
fn render(
    schema: RootSchema,
    id: &str,
    shared: &mut BTreeMap<String, serde_json::Value>,
) -> Result<String, AppServerError> {
    let mut value = serde_json::to_value(&schema)?;
    if let Some(map) = value.as_object_mut() {
        if let Some(serde_json::Value::Object(definitions)) = map.remove(DEFINITIONS) {
            for (name, definition) in definitions {
                if let Some(existing) = shared.get(&name)
                    && *existing != definition
                {
                    return Err(AppServerError::SchemaConflict { name });
                }
                shared.insert(name, definition);
            }
        }
        map.insert("$id".to_string(), serde_json::Value::String(id.to_string()));
    }
    point_refs_at_the_shared_file(&mut value);
    to_json(&value)
}

const DEFINITIONS: &str = "definitions";
const SHARED: &str = "definitions.json";
const LOCAL_REF: &str = "#/definitions/";

/// Every schema file sits one directory below the bundle root, so one relative
/// prefix serves them all.
fn point_refs_at_the_shared_file(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "$ref"
                    && let Some(target) = child.as_str()
                    && let Some(name) = target.strip_prefix(LOCAL_REF)
                {
                    *child = serde_json::Value::String(format!("../{SHARED}{LOCAL_REF}{name}"));
                    continue;
                }
                point_refs_at_the_shared_file(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                point_refs_at_the_shared_file(item);
            }
        }
        _ => {}
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<String, AppServerError> {
    let mut rendered = serde_json::to_string_pretty(value)?;
    rendered.push('\n');
    Ok(rendered)
}

/// Write the bundle under `out`, replacing what is there. Returns the files
/// written, in sorted order.
pub fn generate(out: &Path) -> Result<Vec<PathBuf>, AppServerError> {
    let files = bundle()?;
    let mut written = Vec::new();
    for (relative, contents) in files {
        let path = out.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AppServerError::Output {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(&path, contents).map_err(|source| AppServerError::Output {
            path: path.clone(),
            source,
        })?;
        written.push(relative);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::protocol::notifications::ServerNotification;
    use crate::app_server::protocol::requests::RequestMethod;

    /// The committed bundle, as the repository holds it.
    fn committed() -> BTreeMap<PathBuf, String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/app-server");
        let mut files = BTreeMap::new();
        collect(&root, &root, &mut files);
        files
    }

    fn collect(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => panic!("{}: {error}", dir.display()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, files);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                let contents = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                files.insert(relative.to_path_buf(), contents);
            }
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let first = bundle().unwrap_or_else(|error| panic!("{error}"));
        let second = bundle().unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(first, second, "two generations disagree");
        assert!(first.contains_key(Path::new(MANIFEST)));
    }

    /// The committed bundle is the contract. A type change that is not
    /// regenerated turns this red, which is the whole point of committing it.
    #[test]
    fn the_committed_bundle_matches_the_types() {
        let generated = bundle().unwrap_or_else(|error| panic!("{error}"));
        let committed = committed();
        let missing: Vec<&PathBuf> = generated
            .keys()
            .filter(|path| !committed.contains_key(*path))
            .collect();
        let extra: Vec<&PathBuf> = committed
            .keys()
            .filter(|path| !generated.contains_key(*path))
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "schema bundle drifted: missing {missing:?}, unexpected {extra:?}; \
             run `cargo run -- app-server generate-schema --out schema/app-server`"
        );
        for (path, contents) in &generated {
            assert_eq!(
                committed.get(path).map(String::as_str),
                Some(contents.as_str()),
                "{} drifted; run `cargo run -- app-server generate-schema --out schema/app-server`",
                path.display()
            );
        }
    }

    #[test]
    fn the_manifest_covers_every_method_and_notification() {
        let files = bundle().unwrap_or_else(|error| panic!("{error}"));
        let manifest = files
            .get(Path::new(MANIFEST))
            .unwrap_or_else(|| panic!("the bundle has no manifest"));
        let value: serde_json::Value =
            serde_json::from_str(manifest).unwrap_or_else(|error| panic!("{error}"));
        let methods = value["methods"].as_array().map(Vec::len).unwrap_or(0);
        let notifications = value["notifications"].as_array().map(Vec::len).unwrap_or(0);
        assert_eq!(methods, RequestMethod::ALL.len());
        assert_eq!(notifications, ServerNotification::METHODS.len());
        for method in RequestMethod::ALL {
            let stem = slug(method.as_str());
            assert!(
                files.contains_key(&PathBuf::from(format!("methods/{stem}.params.json"))),
                "{} has no params schema",
                method.as_str()
            );
            assert!(
                files.contains_key(&PathBuf::from(format!("methods/{stem}.result.json"))),
                "{} has no result schema",
                method.as_str()
            );
        }
    }

    /// Every property the bundle publishes is camelCase.
    ///
    /// This is the guard for a real trap: serde's container-level
    /// `rename_all_fields` renames an enum variant's fields while schemars
    /// ignores it, so the published schema would name a field the server never
    /// writes. A snake_case property name is that bug, whatever caused it.
    #[test]
    fn published_property_names_are_camel_case() {
        fn walk(value: &serde_json::Value, path: &str, offenders: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, child) in map {
                        if key == "properties"
                            && let Some(properties) = child.as_object()
                        {
                            for name in properties.keys() {
                                if name.contains('_') {
                                    offenders.push(format!("{path}: {name}"));
                                }
                            }
                        }
                        if key == "required"
                            && let Some(names) = child.as_array()
                        {
                            for name in names.iter().filter_map(serde_json::Value::as_str) {
                                if name.contains('_') {
                                    offenders.push(format!("{path}: required {name}"));
                                }
                            }
                        }
                        walk(child, path, offenders);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        walk(item, path, offenders);
                    }
                }
                _ => {}
            }
        }

        let files = bundle().unwrap_or_else(|error| panic!("{error}"));
        let mut offenders = Vec::new();
        for (path, contents) in &files {
            let value: serde_json::Value =
                serde_json::from_str(contents).unwrap_or_else(|error| panic!("{error}"));
            walk(&value, &path.display().to_string(), &mut offenders);
        }
        assert!(
            offenders.is_empty(),
            "these published fields are not camelCase: {offenders:?}"
        );
    }

    /// Every `$ref` in the bundle resolves. A schema pointing at a definition
    /// the shared file does not carry is a bundle a client cannot compile.
    #[test]
    fn every_reference_resolves_to_a_shared_definition() {
        fn collect(value: &serde_json::Value, refs: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, child) in map {
                        if key == "$ref"
                            && let Some(target) = child.as_str()
                        {
                            refs.push(target.to_string());
                        }
                        collect(child, refs);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        collect(item, refs);
                    }
                }
                _ => {}
            }
        }

        let files = bundle().unwrap_or_else(|error| panic!("{error}"));
        let shared: serde_json::Value = serde_json::from_str(
            files
                .get(Path::new(SHARED))
                .unwrap_or_else(|| panic!("the bundle has no shared definitions")),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        for (path, contents) in &files {
            if path == Path::new(MANIFEST) {
                continue;
            }
            let value: serde_json::Value =
                serde_json::from_str(contents).unwrap_or_else(|error| panic!("{error}"));
            let mut refs = Vec::new();
            collect(&value, &mut refs);
            let local = path == Path::new(SHARED);
            for target in refs {
                let name = if local {
                    target.strip_prefix(LOCAL_REF)
                } else {
                    target.strip_prefix(&format!("../{SHARED}{LOCAL_REF}"))
                };
                let name = name.unwrap_or_else(|| {
                    panic!(
                        "{}: {target} does not point at the shared file",
                        path.display()
                    )
                });
                assert!(
                    shared[DEFINITIONS].get(name).is_some(),
                    "{}: {name} is referenced and not defined",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn every_schema_declares_draft_seven_and_its_own_id() {
        let files = bundle().unwrap_or_else(|error| panic!("{error}"));
        for (path, contents) in &files {
            if path == Path::new(MANIFEST) {
                continue;
            }
            let value: serde_json::Value =
                serde_json::from_str(contents).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(
                value["$schema"].as_str(),
                Some(DIALECT),
                "{} is not draft 7",
                path.display()
            );
            assert!(
                value["$id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("urn:bingo:app-server:v1:")),
                "{} has no stable $id",
                path.display()
            );
        }
    }
}
