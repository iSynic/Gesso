//! File format v2: a `Name.canvas/` folder holding `document.json` and content-addressed
//! `assets/<sha256>.<ext>`. `document.json` is pretty-printed JSON with a `formatVersion`
//! header, stable key order and defaults omitted, so files diff cleanly in git.

mod bundle;
mod migrate;

use std::collections::BTreeMap;

use doc::model::{AssetId, AssetMeta};
use doc::{DocError, Document, DocumentData, NodeTree, Variables};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use bundle::{ASSETS_DIR, Bundle, DOCUMENT_FILE, Opened, identify_asset};
pub use migrate::migrate;

/// The version this build writes. Older files are migrated on load.
pub const FORMAT_VERSION: u32 = 2;
pub const GENERATOR: &str = concat!("gesso ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("{path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot migrate from format version {from}: {msg}")]
    Migration { from: u32, msg: String },
    #[error("file has format version {0}, newer than this build supports ({FORMAT_VERSION})")]
    TooNew(u32),
    #[error("invalid document: {0}")]
    Doc(#[from] DocError),
    #[error("unsupported asset type (expected PNG, JPEG, WebP, GIF or SVG)")]
    UnknownAssetType,
}

pub(crate) fn io_err(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> FormatError + '_ {
    move |source| FormatError::Io { path: path.display().to_string(), source }
}

/// The top level of `document.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFile {
    /// Always written first so readers can check it cheaply.
    pub format_version: u32,
    #[serde(default)]
    pub generator: String,
    pub pages: Vec<NodeTree>,
    #[serde(default, skip_serializing_if = "Variables::is_empty")]
    pub variables: Variables,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<AssetId, AssetMeta>,
}

/// Borrowed view for writing without cloning the document data.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentFileRef<'a> {
    format_version: u32,
    generator: &'a str,
    pages: &'a [NodeTree],
    #[serde(skip_serializing_if = "Variables::is_empty")]
    variables: &'a Variables,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    assets: &'a BTreeMap<AssetId, AssetMeta>,
}

/// Serialises a document to `document.json` text (pretty, 2-space indent, trailing newline).
pub fn to_json(doc: &Document) -> String {
    data_to_json(&doc.to_data())
}

pub fn data_to_json(data: &DocumentData) -> String {
    let file = DocumentFileRef { format_version: FORMAT_VERSION, generator: GENERATOR, pages: &data.pages, variables: &data.variables, assets: &data.assets };
    // Serialising plain data to a String cannot fail.
    let mut s = serde_json::to_string_pretty(&file).unwrap_or_default();
    s.push('\n');
    s
}

/// Reads the format version from the start of the file without parsing all of it.
fn peek_version(bytes: &[u8]) -> Result<Option<u32>, FormatError> {
    let head = &bytes[..bytes.len().min(512)];
    let head = String::from_utf8_lossy(head);
    if let Some(i) = head.find("\"formatVersion\"") {
        let rest = head[i + 15..].trim_start().trim_start_matches(':').trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(v) = digits.parse() {
            return Ok(Some(v));
        }
    }
    // Not near the start (hand-edited file?): fall back to a full read of just this field.
    #[derive(Deserialize)]
    struct Peek {
        #[serde(rename = "formatVersion")]
        v: Option<u32>,
    }
    Ok(serde_json::from_slice::<Peek>(bytes)?.v)
}

/// Parses `document.json` bytes, migrating older versions, and builds a validated document.
pub fn from_json(bytes: &[u8]) -> Result<Document, FormatError> {
    Ok(Document::from_data(data_from_json(bytes)?)?)
}

/// Like [`from_json`] but stops at plain data (no validation).
pub fn data_from_json(bytes: &[u8]) -> Result<DocumentData, FormatError> {
    let version = peek_version(bytes)?.unwrap_or(0);
    let file: DocumentFile = match version {
        FORMAT_VERSION => serde_json::from_slice(bytes)?,
        v if v > FORMAT_VERSION => return Err(FormatError::TooNew(v)),
        _ => serde_json::from_value(migrate(serde_json::from_slice(bytes)?)?)?,
    };
    Ok(DocumentData { pages: file.pages, variables: file.variables, assets: file.assets })
}

/// JSON Schema for `document.json`.
pub fn schema() -> serde_json::Value {
    let mut s = serde_json::to_value(schemars::schema_for!(DocumentFile)).unwrap_or_default();
    if let Some(o) = s.as_object_mut() {
        o.insert("$id".into(), "https://example.invalid/ai-design-engine/document.v2.schema.json".into());
        o.insert("title".into(), "Gesso document, format version 2".into());
    }
    s
}
