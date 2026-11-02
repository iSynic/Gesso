//! The on-disk bundle: `Name.canvas/document.json` + `Name.canvas/assets/<sha256>.<ext>`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use doc::Document;
use doc::model::{AssetId, AssetMeta};
use sha2::{Digest, Sha256};

use crate::{FormatError, from_json, io_err, to_json};

pub const DOCUMENT_FILE: &str = "document.json";
pub const ASSETS_DIR: &str = "assets";

/// A document folder on disk.
#[derive(Debug, Clone)]
pub struct Bundle {
    root: PathBuf,
}

/// Result of opening a bundle.
#[derive(Debug)]
pub struct Opened {
    pub bundle: Bundle,
    pub doc: Document,
    /// Assets listed in the document whose files are missing (the document still opens).
    pub missing_assets: Vec<AssetId>,
}

fn ext_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

/// Writes `bytes` to `path` via a temporary file and a rename, so readers never see a partial
/// file and a crash leaves the previous version intact.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), FormatError> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp).map_err(io_err(&tmp))?;
    f.write_all(bytes).map_err(io_err(&tmp))?;
    f.sync_all().map_err(io_err(&tmp))?;
    drop(f);
    fs::rename(&tmp, path).map_err(io_err(path))
}

impl Bundle {
    /// Creates (or reuses) a bundle folder.
    pub fn create(root: impl Into<PathBuf>) -> Result<Bundle, FormatError> {
        let root = root.into();
        let assets = root.join(ASSETS_DIR);
        fs::create_dir_all(&assets).map_err(io_err(&assets))?;
        Ok(Bundle { root })
    }

    pub fn open(root: impl Into<PathBuf>) -> Result<Opened, FormatError> {
        let root = root.into();
        let path = root.join(DOCUMENT_FILE);
        let bytes = fs::read(&path).map_err(io_err(&path))?;
        let doc = from_json(&bytes)?;
        let bundle = Bundle { root };
        let missing_assets = doc.assets().iter().filter(|(id, m)| !bundle.asset_path(id, m).exists()).map(|(id, _)| id.clone()).collect();
        Ok(Opened { bundle, doc, missing_assets })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn document_path(&self) -> PathBuf {
        self.root.join(DOCUMENT_FILE)
    }

    pub fn asset_path(&self, id: &AssetId, meta: &AssetMeta) -> PathBuf {
        self.root.join(ASSETS_DIR).join(format!("{}.{}", id.0, ext_for(&meta.mime)))
    }

    /// Saves `document.json` atomically. Asset files are written when added, never deleted here.
    pub fn save(&self, doc: &Document) -> Result<(), FormatError> {
        write_atomic(&self.document_path(), to_json(doc).as_bytes())
    }

    /// Stores an image in the bundle under its SHA-256 and returns the id and metadata to
    /// register in the document (via `Tx::put_asset`). Identical bytes are stored once.
    pub fn add_asset(&self, bytes: &[u8]) -> Result<(AssetId, AssetMeta), FormatError> {
        let (id, meta) = identify_asset(bytes)?;
        let path = self.asset_path(&id, &meta);
        if !path.exists() {
            write_atomic(&path, bytes)?;
        }
        Ok((id, meta))
    }

    pub fn read_asset(&self, id: &AssetId, meta: &AssetMeta) -> Result<Vec<u8>, FormatError> {
        let path = self.asset_path(id, meta);
        fs::read(&path).map_err(io_err(&path))
    }
}

/// The content-addressed id (SHA-256) and metadata of an image, as [`Bundle::add_asset`] stores
/// it. Fails for bytes that aren't a supported image.
pub fn identify_asset(bytes: &[u8]) -> Result<(AssetId, AssetMeta), FormatError> {
    let meta = sniff(bytes)?;
    Ok((AssetId(Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()), meta))
}

/// Detects the image type and pixel size.
fn sniff(bytes: &[u8]) -> Result<AssetMeta, FormatError> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
    let head = head.trim_start();
    if head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")) {
        return Ok(AssetMeta { mime: "image/svg+xml".into(), width: 0, height: 0 });
    }
    let mime = match imagesize::image_type(bytes).map_err(|_| FormatError::UnknownAssetType)? {
        imagesize::ImageType::Png => "image/png",
        imagesize::ImageType::Jpeg => "image/jpeg",
        imagesize::ImageType::Webp => "image/webp",
        imagesize::ImageType::Gif => "image/gif",
        _ => return Err(FormatError::UnknownAssetType),
    };
    let size = imagesize::blob_size(bytes).map_err(|_| FormatError::UnknownAssetType)?;
    Ok(AssetMeta { mime: mime.into(), width: size.width as u32, height: size.height as u32 })
}
