//! OCI/Docker image reader (OCI-1.T2).
//!
//! Parses a `docker save` tarball — the `[{Config, Layers}]` `manifest.json` layout
//! every `docker save` emits — into a rootfs directory plus the run [`ImageConfig`]
//! (`Env`/`Cmd`/`Entrypoint`/`WorkingDir`). No kernel, no container runtime: an
//! image is just layers + config; isolation (namespaces/cgroups) is orthogonal and
//! unneeded to *execute* the payload.
//!
//! Deliberately has **no dependency on `x86jit-core`** — this crate physically
//! cannot leak into the recompiler (spec §1/§4.1). The runner (`x86jit-run`) glues
//! this rootfs+config to the engine.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// What to run and how, extracted from the image config blob.
#[derive(Debug, Clone, Default)]
pub struct ImageConfig {
    pub env: Vec<String>,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub working_dir: String,
    pub architecture: String,
    pub os: String,
}

impl ImageConfig {
    /// The process argv: `Entrypoint` followed by `Cmd` (Docker semantics). If both
    /// are empty the image has no default command.
    pub fn argv(&self) -> Vec<String> {
        self.entrypoint.iter().chain(&self.cmd).cloned().collect()
    }
}

#[derive(Debug)]
pub enum OciError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// The tarball didn't contain the expected `manifest.json` / a referenced blob.
    Malformed(String),
}

impl std::fmt::Display for OciError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OciError::Io(e) => write!(f, "io: {e}"),
            OciError::Json(e) => write!(f, "json: {e}"),
            OciError::Malformed(m) => write!(f, "malformed image: {m}"),
        }
    }
}
impl std::error::Error for OciError {}
impl From<std::io::Error> for OciError {
    fn from(e: std::io::Error) -> Self {
        OciError::Io(e)
    }
}
impl From<serde_json::Error> for OciError {
    fn from(e: serde_json::Error) -> Self {
        OciError::Json(e)
    }
}

// --- image JSON shapes (only the fields we use) ---

#[derive(Deserialize)]
struct ManifestEntry {
    #[serde(rename = "Config")]
    config: String,
    #[serde(rename = "Layers")]
    layers: Vec<String>,
}

#[derive(Deserialize)]
struct ConfigBlob {
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    config: ConfigInner,
}

#[derive(Deserialize, Default)]
struct ConfigInner {
    #[serde(rename = "Env", default)]
    env: Vec<String>,
    #[serde(rename = "Entrypoint", default)]
    entrypoint: Vec<String>,
    #[serde(rename = "Cmd", default)]
    cmd: Vec<String>,
    #[serde(rename = "WorkingDir", default)]
    working_dir: String,
}

/// Extract `image_tar` into `rootfs` (which must exist and should be empty) and
/// return its run config. The outer tar is read into memory (image tars are small
/// relative to RAM); layers are gunzipped as needed and applied in order.
pub fn load_image(image_tar: &Path, rootfs: &Path) -> Result<ImageConfig, OciError> {
    let blobs = read_outer_tar(image_tar)?;

    let manifest_raw = blobs
        .get("manifest.json")
        .ok_or_else(|| OciError::Malformed("no manifest.json (not a `docker save` tar?)".into()))?;
    let manifest: Vec<ManifestEntry> = serde_json::from_slice(manifest_raw)?;
    let entry = manifest
        .first()
        .ok_or_else(|| OciError::Malformed("empty manifest.json".into()))?;

    let config_raw = blobs
        .get(entry.config.as_str())
        .ok_or_else(|| OciError::Malformed(format!("missing config blob {}", entry.config)))?;
    let cfg: ConfigBlob = serde_json::from_slice(config_raw)?;

    for layer in &entry.layers {
        let raw = blobs
            .get(layer.as_str())
            .ok_or_else(|| OciError::Malformed(format!("missing layer blob {layer}")))?;
        apply_layer(raw, rootfs)?;
    }

    Ok(ImageConfig {
        env: cfg.config.env,
        entrypoint: cfg.config.entrypoint,
        cmd: cfg.config.cmd,
        working_dir: if cfg.config.working_dir.is_empty() {
            "/".into()
        } else {
            cfg.config.working_dir
        },
        architecture: cfg.architecture,
        os: cfg.os,
    })
}

/// Read every regular-file entry of the outer `docker save` tar into `path ->
/// bytes` (manifest.json, config + layer blobs).
fn read_outer_tar(image_tar: &Path) -> Result<HashMap<String, Vec<u8>>, OciError> {
    let file = std::fs::File::open(image_tar)?;
    let mut archive = tar::Archive::new(file);
    let mut out = HashMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let path = entry.path()?.to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        out.insert(path, bytes);
    }
    Ok(out)
}

/// Apply one filesystem layer (a tar, gzip-compressed iff it starts with the gzip
/// magic) into `rootfs`, honoring OverlayFS whiteouts.
fn apply_layer(raw: &[u8], rootfs: &Path) -> Result<(), OciError> {
    let is_gzip = raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b;
    if is_gzip {
        let dec = flate2::read::GzDecoder::new(raw);
        unpack_tar(tar::Archive::new(dec), rootfs)
    } else {
        unpack_tar(tar::Archive::new(raw), rootfs)
    }
}

fn unpack_tar<R: Read>(mut archive: tar::Archive<R>, rootfs: &Path) -> Result<(), OciError> {
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();

        // Whiteout files (OverlayFS): `.wh.<name>` removes <name>; `.wh..wh..opq`
        // makes the containing dir opaque (clear its current contents).
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name == ".wh..wh..opq" {
                if let Some(dir) = safe_join(rootfs, path.parent().unwrap_or(Path::new(""))) {
                    clear_dir(&dir);
                }
                continue;
            }
            if let Some(target) = name.strip_prefix(".wh.") {
                let victim = path.with_file_name(target);
                if let Some(p) = safe_join(rootfs, &victim) {
                    let _ = std::fs::remove_dir_all(&p).or_else(|_| std::fs::remove_file(&p));
                }
                continue;
            }
        }

        // `unpack_in` sanitizes against `..`/absolute-path traversal and returns
        // Ok(false) if the entry would escape the rootfs (untrusted tars).
        entry.unpack_in(rootfs)?;
    }
    Ok(())
}

/// Join a guest-relative path onto `rootfs`, rejecting any component that would
/// escape it (`..`, absolute prefixes). Returns `None` on an unsafe path.
fn safe_join(rootfs: &Path, rel: &Path) -> Option<PathBuf> {
    let mut out = rootfs.to_path_buf();
    for comp in rel.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            // Escapes or absolute roots are rejected outright.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

fn clear_dir(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            let _ = std::fs::remove_dir_all(&p).or_else(|_| std::fs::remove_file(&p));
        }
    }
}
