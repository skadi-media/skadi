//! Cardigann definition sync + store (SKADI-T-0260): keep a local directory of
//! tracker definitions populated from the community Jackett/Prowlarr `Indexers`
//! repo (a pinned tarball), with a **bundled fallback** so first-run works
//! offline, and expose a parsed [`Catalog`] for the add-tracker picker + the
//! registry. The network fetch lives here (it needs the shared `HttpClient`); the
//! parsing/summarizing is the pure `skadi_cardigann::catalog`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use skadi_cardigann::Catalog;
use skadi_core::{AppError, Result};
use skadi_http::HttpClient;
use tar::Archive;

/// Embedded fallback definitions, written to the store on an empty first run so
/// search works before the first online sync.
static BUNDLED: &[(&str, &str)] = &[
    (
        "thepiratebay.yml",
        include_str!("../bundled/thepiratebay.yml"),
    ),
    ("yts.yml", include_str!("../bundled/yts.yml")),
    ("1337x.yml", include_str!("../bundled/1337x.yml")),
    (
        "limetorrents.yml",
        include_str!("../bundled/limetorrents.yml"),
    ),
    ("nyaasi.yml", include_str!("../bundled/nyaasi.yml")),
    ("eztv.yml", include_str!("../bundled/eztv.yml")),
    (
        "torrentleech.yml",
        include_str!("../bundled/torrentleech.yml"),
    ),
    // The operator's custom audiobook tracker (SKADI-T-0178) — not in upstream.
    (
        "audiobookbay.yml",
        include_str!("../bundled/audiobookbay.yml"),
    ),
];

/// The default pin — `master` tracks the daily-synced upstream; an operator can
/// pin a specific commit/tag via config for reproducibility.
pub const DEFAULT_REVISION: &str = "master";

/// Outcome of a [`DefinitionStore::refresh`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RefreshReport {
    pub revision: String,
    /// Definitions written to the store.
    pub written: usize,
    /// Definitions that failed to parse after the sync (warn-skipped).
    pub unparseable: usize,
}

/// A directory of Cardigann definitions, syncable from upstream.
pub struct DefinitionStore {
    dir: PathBuf,
    http: HttpClient,
}

impl DefinitionStore {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, http: HttpClient) -> Self {
        Self {
            dir: dir.into(),
            http,
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Build the catalog from the store, **seeding the bundled set** if the store
    /// is empty (offline first-run). Parse failures are warn-skipped.
    ///
    /// # Errors
    /// Filesystem errors creating/reading the store directory.
    pub fn load(&self) -> Result<Catalog> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| AppError::Internal(format!("creating definitions dir: {e}")))?;
        let empty = std::fs::read_dir(&self.dir)
            .map_err(|e| AppError::Internal(format!("reading definitions dir: {e}")))?
            .next()
            .is_none();
        if empty {
            self.seed_bundled()?;
        }
        let (catalog, errors) = Catalog::load_dir(&self.dir)
            .map_err(|e| AppError::Internal(format!("loading definitions: {e}")))?;
        if !errors.is_empty() {
            tracing::warn!(
                skipped = errors.len(),
                "skipped unparseable cardigann definitions"
            );
        }
        tracing::info!(count = catalog.len(), "loaded cardigann definition catalog");
        Ok(catalog)
    }

    fn seed_bundled(&self) -> Result<()> {
        for (name, content) in BUNDLED {
            std::fs::write(self.dir.join(name), content)
                .map_err(|e| AppError::Internal(format!("writing bundled def {name}: {e}")))?;
        }
        tracing::info!(
            count = BUNDLED.len(),
            "seeded bundled cardigann definitions"
        );
        Ok(())
    }

    /// Fetch the pinned `Prowlarr/Indexers` tarball and extract the latest-schema
    /// definitions (v11, falling back to v10) into the store, then rebuild the
    /// catalog. Returns counts; never partially-applies a corrupt archive.
    ///
    /// # Errors
    /// Network errors fetching the tarball, or archive-decode errors.
    pub async fn refresh(&self, revision: &str) -> Result<RefreshReport> {
        let url = format!("https://github.com/Prowlarr/Indexers/archive/{revision}.tar.gz");
        let bytes = self.http.get_bytes(&url).await?;
        let defs = extract_definitions(&bytes)?;

        std::fs::create_dir_all(&self.dir)
            .map_err(|e| AppError::Internal(format!("creating definitions dir: {e}")))?;
        for (name, content) in &defs {
            std::fs::write(self.dir.join(name), content)
                .map_err(|e| AppError::Internal(format!("writing def {name}: {e}")))?;
        }

        let (_, errors) = Catalog::load_dir(&self.dir)
            .map_err(|e| AppError::Internal(format!("reloading definitions: {e}")))?;
        Ok(RefreshReport {
            revision: revision.to_string(),
            written: defs.len(),
            unparseable: errors.len(),
        })
    }
}

/// Extract `definitions/v11` (then `v10`) `*.yml` from the gzipped repo tarball,
/// keeping the highest schema version per definition filename.
fn extract_definitions(targz: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut archive = Archive::new(GzDecoder::new(targz));
    // filename → (version, contents); a higher version overwrites a lower one.
    let mut best: BTreeMap<String, (u8, String)> = BTreeMap::new();
    let entries = archive
        .entries()
        .map_err(|e| AppError::Internal(format!("reading tarball: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| AppError::Internal(format!("tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| AppError::Internal(format!("tar path: {e}")))?
            .to_path_buf();
        let s = path.to_string_lossy();
        // ".../definitions/v11/foo.yml" → rel = "v11/foo.yml"
        let Some(rel) = s.split("/definitions/").nth(1) else {
            continue;
        };
        let version = if rel.starts_with("v11/") {
            11
        } else if rel.starts_with("v10/") {
            10
        } else {
            continue;
        };
        if !rel.ends_with(".yml") {
            continue;
        }
        let Some(fname) = Path::new(rel).file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        let fname = fname.to_string();
        if best.get(&fname).is_some_and(|(v, _)| *v >= version) {
            continue;
        }
        let mut content = String::new();
        if entry.read_to_string(&mut content).is_ok() {
            best.insert(fname, (version, content));
        }
    }
    Ok(best.into_iter().map(|(k, (_, v))| (k, v)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use flate2::{Compression, write::GzEncoder};

    /// Build a gzipped tar of `definitions/<ver>/<name>` entries.
    fn make_targz(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, body) in entries {
            let bytes = body.as_bytes();
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, format!("Indexers-master/{path}"), bytes)
                .unwrap();
        }
        let tar_bytes = tar.into_inner().unwrap();
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut enc, &tar_bytes).unwrap();
        enc.finish().unwrap()
    }

    const MINI_DEF: &str = "id: minitracker\nname: Mini\ntype: public\ncaps:\n  modes: {search: [q]}\nsearch:\n  rows:\n    selector: tr\n";

    #[test]
    fn extract_prefers_v11_over_v10() {
        let v10 = "id: dup\nname: V10\ncaps: {}\nsearch: {rows: {selector: a}}\n";
        let v11 = "id: dup\nname: V11\ncaps: {}\nsearch: {rows: {selector: b}}\n";
        let targz = make_targz(&[
            ("definitions/v10/dup.yml", v10),
            ("definitions/v11/dup.yml", v11),
            ("definitions/v11/minitracker.yml", MINI_DEF),
            ("definitions/v3/ancient.yml", "id: old\nname: Old\n"), // ignored (too old)
            ("README.md", "not a def"),
        ]);
        let defs = extract_definitions(&targz).unwrap();
        assert_eq!(
            defs.len(),
            2,
            "dup (v11) + minitracker; ancient/readme excluded"
        );
        assert!(defs["dup.yml"].contains("V11"));
        assert!(defs.contains_key("minitracker.yml"));
    }

    #[test]
    fn load_seeds_bundled_when_empty() {
        let tmp = skadi_core::unique_temp_path("defs");
        let _ = std::fs::remove_dir_all(&tmp);
        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let store = DefinitionStore::new(&tmp, http);

        let catalog = store.load().unwrap();
        assert!(
            catalog.len() >= BUNDLED.len(),
            "bundled seeded: {}",
            catalog.len()
        );
        assert!(catalog.get("thepiratebay").is_some());
        // Files actually landed on disk.
        assert!(tmp.join("thepiratebay.yml").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
