//! Pull an image straight from an OCI/Docker registry into a rootfs — no Docker
//! daemon, no `skopeo`, no committed tar. A small [OCI distribution] client: resolve
//! the reference, acquire an anonymous bearer token if the registry demands one,
//! fetch the manifest (picking the `amd64/linux` entry from a multi-arch index),
//! then download the config + layer blobs and apply them via the shared
//! [`oci::apply_layer`]. The result is the same [`ImageConfig`] the `docker save`
//! reader produces, so the runner is identical from there on.
//!
//! Like [`oci`], this has **no dependency on `x86jit-core`** — pulling bytes over
//! HTTP has nothing to do with the recompiler (spec §1/§4.1).
//!
//! [OCI distribution]: https://github.com/opencontainers/distribution-spec
//! [`oci`]: crate::oci

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::oci::{self, ImageConfig, OciError};

/// Docker Hub's registry host (the default when a reference names no registry).
const DOCKER_HUB: &str = "registry-1.docker.io";

/// Media types we accept for a manifest request — multi-arch index first, then a
/// concrete image manifest, in both Docker and OCI spellings.
const MANIFEST_ACCEPT: &str = "application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json, \
     application/vnd.oci.image.manifest.v1+json";

#[derive(Debug)]
pub enum RegistryError {
    /// Transport / HTTP status failure (with a short context string).
    Http(String),
    /// A manifest / token response we couldn't parse or didn't expect.
    Protocol(String),
    /// Applying a downloaded blob failed (tar/gzip/config).
    Oci(OciError),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Http(m) => write!(f, "registry http: {m}"),
            RegistryError::Protocol(m) => write!(f, "registry protocol: {m}"),
            RegistryError::Oci(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for RegistryError {}
impl From<OciError> for RegistryError {
    fn from(e: OciError) -> Self {
        RegistryError::Oci(e)
    }
}

// --- manifest / token JSON shapes (only the fields we use) ---

#[derive(Deserialize)]
struct Index {
    manifests: Vec<Descriptor>,
}

#[derive(Deserialize)]
struct Descriptor {
    digest: String,
    #[serde(default)]
    platform: Platform,
}

#[derive(Deserialize, Default)]
struct Platform {
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    os: String,
}

#[derive(Deserialize)]
struct Manifest {
    config: Blob,
    layers: Vec<Blob>,
}

#[derive(Deserialize)]
struct Blob {
    digest: String,
}

#[derive(Deserialize)]
struct Token {
    #[serde(default)]
    token: String,
    #[serde(default)]
    access_token: String,
}

/// A parsed image reference: `registry`, `repository`, and the tag-or-digest `reference`.
struct Reference {
    registry: String,
    repository: String,
    reference: String,
}

/// Parse `[registry[:port]/]repository[:tag|@digest]` the way Docker does: the first
/// path component is the registry only if it looks like a host (contains `.`/`:`, or
/// is `localhost`); otherwise the image is on Docker Hub and a single-word name gets
/// the implicit `library/` prefix. Tag defaults to `latest`.
fn parse_reference(input: &str) -> Reference {
    // Split off the tag or digest first (a `:` for the tag must come *after* the last
    // `/`, so a `registry:port` host isn't mistaken for a tag).
    let (name, reference) = if let Some(at) = input.rfind('@') {
        (&input[..at], input[at + 1..].to_string())
    } else {
        let slash = input.rfind('/').map(|i| i as isize).unwrap_or(-1);
        match input.rfind(':') {
            Some(colon) if (colon as isize) > slash => {
                (&input[..colon], input[colon + 1..].to_string())
            }
            _ => (input, "latest".to_string()),
        }
    };

    let (registry, repository) = match name.split_once('/') {
        Some((host, rest)) if host.contains('.') || host.contains(':') || host == "localhost" => {
            (host.to_string(), rest.to_string())
        }
        // No registry component → Docker Hub; a bare `busybox` is `library/busybox`.
        _ => {
            let repo = if name.contains('/') {
                name.to_string()
            } else {
                format!("library/{name}")
            };
            (DOCKER_HUB.to_string(), repo)
        }
    };
    Reference {
        registry,
        repository,
        reference,
    }
}

/// Pull `reference` into `rootfs` (which must exist), returning its run config. Uses
/// HTTPS unless `plain_http` (for an insecure `registry:port`). Selects the
/// `amd64/linux` image from a multi-arch index.
pub fn pull(
    reference: &str,
    rootfs: &Path,
    plain_http: bool,
) -> Result<ImageConfig, RegistryError> {
    let r = parse_reference(reference);
    let scheme = if plain_http { "http" } else { "https" };
    let base = format!("{scheme}://{}/v2/{}", r.registry, r.repository);
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();

    let mut client = Client {
        agent,
        base,
        repository: r.repository,
        registry: r.registry,
        scheme,
        token: None,
        cache: cache_dir(),
    };

    // Resolve the reference to a concrete image manifest, following one index hop.
    let raw = client.get_manifest(&r.reference)?;
    let manifest: Manifest = match parse_manifest(&raw)? {
        Resolved::Manifest(m) => m,
        Resolved::Index(idx) => {
            let digest = pick_amd64(&idx)?;
            let raw = client.get_manifest(&digest)?;
            match parse_manifest(&raw)? {
                Resolved::Manifest(m) => m,
                Resolved::Index(_) => {
                    return Err(RegistryError::Protocol("nested manifest index".into()))
                }
            }
        }
    };

    // Config first (cheap), then layers in order.
    let config_raw = client.get_blob(&manifest.config.digest)?;
    for layer in &manifest.layers {
        let raw = client.get_blob(&layer.digest)?;
        oci::apply_layer(&raw, rootfs)?;
    }
    Ok(oci::config_from_blob(&config_raw)?)
}

enum Resolved {
    Manifest(Manifest),
    Index(Index),
}

/// A manifest response is either a multi-arch index (has `manifests`) or a concrete
/// image manifest (has `config` + `layers`). Distinguish structurally rather than
/// trusting the `Content-Type`, which some registries set loosely.
fn parse_manifest(raw: &[u8]) -> Result<Resolved, RegistryError> {
    if let Ok(idx) = serde_json::from_slice::<Index>(raw) {
        if !idx.manifests.is_empty() {
            return Ok(Resolved::Index(idx));
        }
    }
    serde_json::from_slice::<Manifest>(raw)
        .map(Resolved::Manifest)
        .map_err(|e| RegistryError::Protocol(format!("manifest: {e}")))
}

fn pick_amd64(idx: &Index) -> Result<String, RegistryError> {
    idx.manifests
        .iter()
        .find(|m| m.platform.architecture == "amd64" && m.platform.os == "linux")
        .map(|m| m.digest.clone())
        .ok_or_else(|| RegistryError::Protocol("no amd64/linux image in the manifest index".into()))
}

/// Content-addressed blob cache directory: `$X86JIT_OCI_CACHE` if set (CI points it at
/// an `actions/cache` dir), else a stable temp dir so repeated pulls of the same image
/// don't re-fetch. Blobs and digest-pinned manifests are keyed by their sha256 digest,
/// so the cache is immutable — a registry (or Docker Hub) is hit at most once per digest.
fn cache_dir() -> Option<PathBuf> {
    Some(
        std::env::var_os("X86JIT_OCI_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("x86jit-oci-cache")),
    )
}

struct Client {
    agent: ureq::Agent,
    base: String,
    repository: String,
    registry: String,
    scheme: &'static str,
    token: Option<String>,
    cache: Option<PathBuf>,
}

impl Client {
    /// Fetch a manifest by `reference`. A digest reference (`sha256:…`) is immutable, so
    /// it is cached; a mutable tag is always fetched fresh.
    fn get_manifest(&mut self, reference: &str) -> Result<Vec<u8>, RegistryError> {
        let cacheable = reference.starts_with("sha256:");
        if cacheable {
            if let Some(bytes) = self.cache_read(reference) {
                return Ok(bytes);
            }
        }
        let url = format!("{}/manifests/{reference}", self.base);
        let bytes = self.get(&url, Some(MANIFEST_ACCEPT))?;
        if cacheable {
            self.cache_write(reference, &bytes);
        }
        Ok(bytes)
    }

    /// Fetch a blob by `digest` (config or layer). Always cacheable — blobs are
    /// content-addressed, so the digest is a sound, immutable cache key.
    fn get_blob(&mut self, digest: &str) -> Result<Vec<u8>, RegistryError> {
        if let Some(bytes) = self.cache_read(digest) {
            return Ok(bytes);
        }
        let url = format!("{}/blobs/{digest}", self.base);
        let bytes = self.get(&url, None)?;
        self.cache_write(digest, &bytes);
        Ok(bytes)
    }

    fn cache_path(&self, digest: &str) -> Option<PathBuf> {
        // `sha256:hex` → `sha256-hex` (portable filename).
        self.cache
            .as_ref()
            .map(|d| d.join(digest.replace(':', "-")))
    }

    fn cache_read(&self, digest: &str) -> Option<Vec<u8>> {
        std::fs::read(self.cache_path(digest)?).ok()
    }

    fn cache_write(&self, digest: &str, bytes: &[u8]) {
        // Best-effort: a cache write failure just means the next pull re-fetches.
        if let Some(path) = self.cache_path(digest) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, bytes);
        }
    }

    /// GET `url`, acquiring a bearer token on a 401 and retrying once. The token is
    /// cached for the repository and reused across manifest + blob requests.
    fn get(&mut self, url: &str, accept: Option<&str>) -> Result<Vec<u8>, RegistryError> {
        match self.try_get(url, accept) {
            Ok(bytes) => Ok(bytes),
            Err(GetError::Unauthorized(challenge)) => {
                self.token = Some(self.acquire_token(&challenge)?);
                self.try_get(url, accept).map_err(|e| e.into_registry())
            }
            Err(e) => Err(e.into_registry()),
        }
    }

    fn try_get(&self, url: &str, accept: Option<&str>) -> Result<Vec<u8>, GetError> {
        let mut req = self.agent.get(url);
        if let Some(a) = accept {
            req = req.set("Accept", a);
        }
        if let Some(t) = &self.token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        match req.call() {
            Ok(resp) => {
                let mut bytes = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut bytes)
                    .map_err(|e| GetError::Http(format!("read body: {e}")))?;
                Ok(bytes)
            }
            Err(ureq::Error::Status(401, resp)) => Err(GetError::Unauthorized(
                resp.header("WWW-Authenticate").unwrap_or("").to_string(),
            )),
            Err(ureq::Error::Status(code, resp)) => Err(GetError::Http(format!(
                "{code} {} for {url}",
                resp.status_text()
            ))),
            Err(e) => Err(GetError::Http(format!("{e} for {url}"))),
        }
    }

    /// Turn a `Bearer realm="…",service="…",scope="…"` challenge into a token via the
    /// realm's token endpoint. Falls back to `repository:<repo>:pull` scope.
    fn acquire_token(&self, challenge: &str) -> Result<String, RegistryError> {
        let params = parse_bearer_challenge(challenge);
        let realm = params
            .iter()
            .find(|(k, _)| k == "realm")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| format!("{}://{}/token", self.scheme, self.registry));
        let service = params
            .iter()
            .find(|(k, _)| k == "service")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let scope = params
            .iter()
            .find(|(k, _)| k == "scope")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| format!("repository:{}:pull", self.repository));

        let mut req = self.agent.get(&realm);
        if !service.is_empty() {
            req = req.query("service", &service);
        }
        req = req.query("scope", &scope);
        let resp = req
            .call()
            .map_err(|e| RegistryError::Http(format!("token endpoint: {e}")))?;
        let mut body = Vec::new();
        resp.into_reader()
            .read_to_end(&mut body)
            .map_err(|e| RegistryError::Http(format!("token body: {e}")))?;
        let token: Token = serde_json::from_slice(&body)
            .map_err(|e| RegistryError::Protocol(format!("token json: {e}")))?;
        // Registries return either `token` or `access_token` (both mean the same).
        let t = if !token.token.is_empty() {
            token.token
        } else {
            token.access_token
        };
        if t.is_empty() {
            return Err(RegistryError::Protocol("empty bearer token".into()));
        }
        Ok(t)
    }
}

enum GetError {
    Unauthorized(String),
    Http(String),
}
impl GetError {
    fn into_registry(self) -> RegistryError {
        match self {
            GetError::Unauthorized(_) => {
                RegistryError::Http("401 Unauthorized after acquiring a token".into())
            }
            GetError::Http(m) => RegistryError::Http(m),
        }
    }
}

/// Parse the comma-separated `key="value"` pairs of a `Bearer …` WWW-Authenticate
/// challenge (the leading `Bearer ` scheme word is dropped).
fn parse_bearer_challenge(challenge: &str) -> Vec<(String, String)> {
    let body = challenge
        .trim()
        .strip_prefix("Bearer ")
        .unwrap_or(challenge);
    body.split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_defaults_to_docker_hub_library() {
        let r = parse_reference("busybox");
        assert_eq!(r.registry, DOCKER_HUB);
        assert_eq!(r.repository, "library/busybox");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn reference_keeps_explicit_registry_port_and_tag() {
        let r = parse_reference("localhost:5000/team/app:1.2");
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repository, "team/app");
        assert_eq!(r.reference, "1.2");
    }

    #[test]
    fn reference_digest_pins() {
        let r = parse_reference("public.ecr.aws/docker/library/busybox@sha256:abc");
        assert_eq!(r.registry, "public.ecr.aws");
        assert_eq!(r.repository, "docker/library/busybox");
        assert_eq!(r.reference, "sha256:abc");
    }

    #[test]
    fn bearer_challenge_parses() {
        let p = parse_bearer_challenge(
            "Bearer realm=\"https://auth.docker.io/token\",service=\"registry.docker.io\",scope=\"repository:library/busybox:pull\"",
        );
        assert_eq!(
            p.iter().find(|(k, _)| k == "realm").unwrap().1,
            "https://auth.docker.io/token"
        );
        assert_eq!(
            p.iter().find(|(k, _)| k == "scope").unwrap().1,
            "repository:library/busybox:pull"
        );
    }
}
