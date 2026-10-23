//! Model files for local speech recognition: a small registry, a download
//! cache with checksums, and a lock file recording what is installed.
//!
//! Nothing here runs during a capture or a test: models are fetched only by an
//! explicit `mc models pull`, over `https://`, `http://` or `file://`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("unknown model {0:?} (see `mc models list`; pass --url to add your own)")]
    Unknown(String),
    #[error("model {0:?} is not installed (run `mc models pull {0}`)")]
    NotInstalled(String),
    #[error("invalid model name {0:?}: use letters, digits, '.', '_' and '-'")]
    BadName(String),
    #[error("unsupported URL {0:?} (https://, http:// or file://)")]
    BadUrl(String),
    #[error("checksum mismatch for {name}: expected {expected}, got {actual}")]
    Mismatch {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("download failed: {0}")]
    Download(String),
    #[error("{path}: {message}")]
    Io { path: String, message: String },
}

impl ModelError {
    fn io(path: &Path, error: impl std::fmt::Display) -> Self {
        Self::Io {
            path: path.display().to_string(),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSpec {
    pub name: String,
    pub description: String,
    pub file: String,
    pub url: String,
    /// Official checksum when known. Entries without one are pinned on first
    /// download (the lock file keeps the hash) and verified from then on.
    pub sha256: Option<String>,
}

/// The whisper.cpp model repository at a fixed revision, so the checksums keep matching.
const WHISPER_REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

fn whisper(name: &str, file: &str, sha256: &str, description: &str) -> ModelSpec {
    ModelSpec {
        name: name.to_string(),
        description: description.to_string(),
        file: file.to_string(),
        url: format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/{WHISPER_REVISION}/{file}"
        ),
        sha256: Some(sha256.to_string()),
    }
}

/// The models `mc` knows how to fetch.
pub fn builtin() -> Vec<ModelSpec> {
    vec![
        whisper(
            "whisper-base",
            "ggml-base.bin",
            "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
            "multilingual, fast: live captions on modest hardware",
        ),
        whisper(
            "whisper-small",
            "ggml-small.bin",
            "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
            "multilingual, better live accuracy",
        ),
        whisper(
            "whisper-medium-q5_0",
            "ggml-medium-q5_0.bin",
            "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
            "multilingual, quantised: balanced final pass",
        ),
        whisper(
            "whisper-large-v3-turbo-q5_0",
            "ggml-large-v3-turbo-q5_0.bin",
            "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
            "multilingual, quantised: the default final pass",
        ),
        whisper(
            "whisper-large-v3-turbo",
            "ggml-large-v3-turbo.bin",
            "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
            "multilingual, full precision: best accuracy, needs more memory",
        ),
    ]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockEntry {
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// The checksum came from the registry or `--sha256`, not from this download.
    pub pinned: bool,
    pub pulled_wall_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Lock {
    models: BTreeMap<String, LockEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelStatus {
    pub name: String,
    pub description: String,
    pub builtin: bool,
    pub installed: bool,
    pub path: Option<PathBuf>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub pinned: bool,
}

#[derive(Debug, Default)]
pub struct PullOptions {
    /// Overrides the registry URL, or defines a model of your own.
    pub url: Option<String>,
    /// The checksum the file must have.
    pub sha256: Option<String>,
    /// Download again even if the model is installed and intact.
    pub force: bool,
}

#[derive(Debug, PartialEq)]
pub enum PullOutcome {
    Downloaded {
        path: PathBuf,
        sha256: String,
        size: u64,
        /// The hash was only recorded, not checked against a known one.
        unpinned: bool,
    },
    AlreadyInstalled {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerifyReport {
    pub name: String,
    pub ok: bool,
    pub actual: String,
    /// What it was compared with; none when nothing was recorded.
    pub expected: Option<String>,
}

pub struct ModelStore {
    dir: PathBuf,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn normalise_hash(text: &str) -> String {
    text.trim().to_lowercase()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub fn sha256_file(path: &Path) -> Result<String, ModelError> {
    let mut file = File::open(path).map_err(|e| ModelError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| ModelError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `file:///home/a/m.bin`, `file://localhost/home/a/m.bin`,
/// `file:///C:/m.bin` and the lenient `file://C:\m.bin` (percent-escapes
/// decoded) to a path.
fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = match rest.strip_prefix("localhost") {
        Some(path) if path.starts_with('/') => path,
        Some(_) => return None,
        None => rest,
    };
    let decoded = percent_decode(rest)?;
    if is_drive_path(&decoded) {
        return Some(PathBuf::from(decoded));
    }
    match decoded.strip_prefix('/') {
        Some(drive) if is_drive_path(drive) => Some(PathBuf::from(drive)),
        Some(_) => Some(PathBuf::from(decoded)),
        // A host other than this machine (UNC share): not supported.
        None => None,
    }
}

/// `C:\...` or `C:/...`: what Windows users paste after `file://`.
fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\')
}

impl ModelStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("lock.json")
    }

    fn load_lock(&self) -> Lock {
        let mut lock: Lock = std::fs::read_to_string(self.lock_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // The lock is a plain file anyone can edit: an entry that does not name
        // a file inside the models folder is ignored, never followed.
        lock.models
            .retain(|name, entry| valid_name(name) && valid_name(&entry.file));
        lock
    }

    /// Re-reads the lock under an exclusive marker file, applies `change` and
    /// saves, so two `mc models` processes cannot lose each other's updates.
    fn update_lock(&self, change: impl FnOnce(&mut Lock)) -> Result<(), ModelError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| ModelError::io(&self.dir, e))?;
        let marker = self.dir.join(".lock");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
            {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // A marker older than a minute belongs to a dead process.
                    let stale = std::fs::metadata(&marker)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(60));
                    if stale {
                        let _ = std::fs::remove_file(&marker);
                    } else if std::time::Instant::now() > deadline {
                        return Err(ModelError::io(&marker, "another mc models is running"));
                    } else {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                }
                Err(e) => return Err(ModelError::io(&marker, e)),
            }
        }
        let mut lock = self.load_lock();
        change(&mut lock);
        let result = self.save_lock(&lock);
        let _ = std::fs::remove_file(&marker);
        result
    }

    fn save_lock(&self, lock: &Lock) -> Result<(), ModelError> {
        let path = self.lock_path();
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let text = serde_json::to_string_pretty(lock).map_err(|e| ModelError::io(&path, e))? + "\n";
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| ModelError::io(&path, e))
    }

    fn spec(&self, name: &str, lock: &Lock) -> Option<(ModelSpec, bool)> {
        if let Some(spec) = builtin().into_iter().find(|s| s.name == name) {
            return Some((spec, true));
        }
        lock.models.get(name).map(|entry| {
            (
                ModelSpec {
                    name: name.to_string(),
                    description: "custom model".to_string(),
                    file: entry.file.clone(),
                    url: entry.url.clone(),
                    sha256: entry.pinned.then(|| entry.sha256.clone()),
                },
                false,
            )
        })
    }

    pub fn list(&self) -> Vec<ModelStatus> {
        let lock = self.load_lock();
        let mut names: Vec<String> = builtin().into_iter().map(|s| s.name).collect();
        names.extend(
            lock.models
                .keys()
                .filter(|k| !names.contains(k))
                .cloned()
                .collect::<Vec<_>>(),
        );
        names
            .into_iter()
            .filter_map(|name| {
                let (spec, is_builtin) = self.spec(&name, &lock)?;
                let path = self.dir.join(&spec.file);
                let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file());
                let entry = lock.models.get(&name);
                Some(ModelStatus {
                    name,
                    description: spec.description,
                    builtin: is_builtin,
                    installed: meta.is_some(),
                    path: meta.as_ref().map(|_| path),
                    size: meta.map(|m| m.len()),
                    sha256: entry.map(|e| e.sha256.clone()),
                    pinned: entry.is_some_and(|e| e.pinned) || spec.sha256.is_some(),
                })
            })
            .collect()
    }

    /// Where an installed model lives.
    pub fn path(&self, name: &str) -> Result<PathBuf, ModelError> {
        let lock = self.load_lock();
        let (spec, _) = self
            .spec(name, &lock)
            .ok_or_else(|| ModelError::Unknown(name.to_string()))?;
        let path = self.dir.join(spec.file);
        if path.is_file() {
            Ok(path)
        } else {
            Err(ModelError::NotInstalled(name.to_string()))
        }
    }

    /// Hash of the installed file against what the lock (or registry) recorded.
    pub fn verify(&self, name: &str) -> Result<VerifyReport, ModelError> {
        let path = self.path(name)?;
        let lock = self.load_lock();
        let expected = self
            .spec(name, &lock)
            .and_then(|(s, _)| s.sha256)
            .or_else(|| lock.models.get(name).map(|e| e.sha256.clone()))
            .map(|h| normalise_hash(&h));
        let actual = sha256_file(&path)?;
        Ok(VerifyReport {
            name: name.to_string(),
            ok: expected.as_ref().is_none_or(|e| *e == actual),
            actual,
            expected,
        })
    }

    pub fn remove(&self, name: &str) -> Result<bool, ModelError> {
        let lock = self.load_lock();
        let (spec, _) = self
            .spec(name, &lock)
            .ok_or_else(|| ModelError::Unknown(name.to_string()))?;
        let path = self.dir.join(&spec.file);
        let existed = path.is_file();
        if existed {
            std::fs::remove_file(&path).map_err(|e| ModelError::io(&path, e))?;
        }
        // A custom model disappears from the registry too; a built-in one just
        // forgets its recorded hash.
        if lock.models.contains_key(name) {
            self.update_lock(|lock| {
                lock.models.remove(name);
            })?;
        }
        Ok(existed)
    }

    pub fn pull(
        &self,
        name: &str,
        opts: &PullOptions,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<PullOutcome, ModelError> {
        if !valid_name(name) {
            return Err(ModelError::BadName(name.to_string()));
        }
        let lock = self.load_lock();
        let (mut spec, _) = match (self.spec(name, &lock), &opts.url) {
            (Some(found), _) => found,
            (None, Some(url)) => (
                ModelSpec {
                    name: name.to_string(),
                    description: "custom model".to_string(),
                    file: format!("custom-{name}.bin"),
                    url: url.clone(),
                    sha256: None,
                },
                false,
            ),
            (None, None) => return Err(ModelError::Unknown(name.to_string())),
        };
        if let Some(url) = &opts.url {
            // The registry checksum belongs to the registry URL, not to this one.
            spec.url.clone_from(url);
            spec.sha256 = None;
        }
        self.pull_spec(name, spec, &lock, opts, progress)
    }

    fn pull_spec(
        &self,
        name: &str,
        spec: ModelSpec,
        lock: &Lock,
        opts: &PullOptions,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<PullOutcome, ModelError> {
        let expected = opts
            .sha256
            .as_deref()
            .map(normalise_hash)
            .or_else(|| spec.sha256.as_deref().map(normalise_hash))
            .or_else(|| {
                // Same source as before: keep holding the file to what it was.
                lock.models
                    .get(name)
                    .filter(|e| e.url == spec.url)
                    .map(|e| e.sha256.clone())
            });
        let target = self.dir.join(&spec.file);
        std::fs::create_dir_all(&self.dir).map_err(|e| ModelError::io(&self.dir, e))?;
        let previous = lock.models.get(name);
        let same_source = previous.is_some_and(|e| e.url == spec.url);
        let installed = if !opts.force && target.is_file() {
            sha256_file(&target).ok()
        } else {
            None
        };
        if let Some(actual) = &installed {
            if same_source && expected.as_ref().is_none_or(|e| e == actual) {
                return Ok(PullOutcome::AlreadyInstalled { path: target });
            }
            // The registry URL moved (new pinned revision) but the file is the official one.
            if spec.sha256.as_deref().map(normalise_hash).as_ref() == Some(actual) {
                let entry = LockEntry {
                    file: spec.file.clone(),
                    url: spec.url.clone(),
                    sha256: actual.clone(),
                    size: std::fs::metadata(&target).map_or(0, |m| m.len()),
                    pinned: true,
                    pulled_wall_ms: previous.map_or_else(now_ms, |e| e.pulled_wall_ms),
                };
                if previous != Some(&entry) {
                    self.update_lock(|lock| {
                        lock.models.insert(name.to_string(), entry);
                    })?;
                }
                return Ok(PullOutcome::AlreadyInstalled { path: target });
            }
        }
        // Only a checksum that came from the registry or `--sha256` is official;
        // one merely recorded earlier keeps its unofficial status.
        let official = opts.sha256.is_some()
            || spec.sha256.is_some()
            || (same_source && previous.is_some_and(|e| e.pinned));

        // Unique per process, so two pulls never write the same file.
        let part = self
            .dir
            .join(format!("{}.{}.part", spec.file, std::process::id()));
        let result = download(&spec.url, &part, progress);
        let (actual, size) = match result {
            Ok(done) => done,
            Err(error) => {
                let _ = std::fs::remove_file(&part);
                return Err(error);
            }
        };
        if let Some(expected) = &expected
            && *expected != actual
        {
            let _ = std::fs::remove_file(&part);
            return Err(ModelError::Mismatch {
                name: name.to_string(),
                expected: expected.clone(),
                actual,
            });
        }
        if let Err(error) = std::fs::rename(&part, &target) {
            let _ = std::fs::remove_file(&part);
            return Err(ModelError::io(&target, error));
        }
        let entry = LockEntry {
            file: spec.file,
            url: spec.url,
            sha256: actual.clone(),
            size,
            pinned: official,
            pulled_wall_ms: now_ms(),
        };
        self.update_lock(|lock| {
            lock.models.insert(name.to_string(), entry);
        })?;
        Ok(PullOutcome::Downloaded {
            path: target,
            sha256: actual,
            size,
            unpinned: !official,
        })
    }
}

/// Copies `url` to `part`, returning its sha256 and size.
fn download(
    url: &str,
    part: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(String, u64), ModelError> {
    let (mut reader, total): (Box<dyn Read>, Option<u64>) = if url.starts_with("file://") {
        let path = file_url_path(url).ok_or_else(|| ModelError::BadUrl(url.to_string()))?;
        let file = File::open(&path).map_err(|e| ModelError::io(&path, e))?;
        let total = file.metadata().ok().map(|m| m.len());
        (Box::new(file), total)
    } else if url.starts_with("https://") || url.starts_with("http://") {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            // No overall body limit: the larger models take a while. The
            // response must still start, and Ctrl-C ends the command.
            .timeout_recv_body(None)
            .build()
            .into();
        let response = agent
            .get(url)
            .call()
            .map_err(|e| ModelError::Download(format!("{url}: {e}")))?;
        let total = response.body().content_length();
        (Box::new(response.into_body().into_reader()), total)
    } else {
        return Err(ModelError::BadUrl(url.to_string()));
    };
    let mut out = File::create(part).map_err(|e| ModelError::io(part, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| ModelError::Download(format!("{url}: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        out.write_all(&buffer[..n])
            .map_err(|e| ModelError::io(part, e))?;
        size += n as u64;
        progress(size, total);
    }
    out.sync_all().map_err(|e| ModelError::io(part, e))?;
    if let Some(total) = total
        && total != size
    {
        return Err(ModelError::Download(format!(
            "{url}: received {size} of {total} bytes"
        )));
    }
    Ok((hex(&hasher.finalize()), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_file_urls() {
        assert!(valid_name("whisper-large-v3-turbo-q5_0"));
        assert!(valid_name("mi.modelo_2"));
        for bad in ["", ".hidden", "a/b", "a b", "..", "x\\y"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        let path = |u: &str| file_url_path(u);
        assert_eq!(
            path("file:///home/a/m.bin"),
            Some(PathBuf::from("/home/a/m.bin"))
        );
        assert_eq!(
            path("file://localhost/home/a/m.bin"),
            Some(PathBuf::from("/home/a/m.bin"))
        );
        assert_eq!(
            path("file:///C:/m/x.bin"),
            Some(PathBuf::from("C:/m/x.bin"))
        );
        assert_eq!(
            path("file://C:\\m\\x.bin"),
            Some(PathBuf::from("C:\\m\\x.bin"))
        );
        assert_eq!(
            path("file://localhost/D:/m.bin"),
            Some(PathBuf::from("D:/m.bin"))
        );
        assert_eq!(
            path("file:///home/a%20b/m%C3%A9.bin"),
            Some(PathBuf::from("/home/a b/mé.bin"))
        );
        assert_eq!(path("file://C%3A/m.bin"), Some(PathBuf::from("C:/m.bin")));
        assert_eq!(path("file://localhostC:/m.bin"), None);
        assert_eq!(path("file:///1:/x"), Some(PathBuf::from("/1:/x")));
        assert_eq!(path("file://server/share/m.bin"), None);
        assert_eq!(path("file:///bad%zz"), None);
    }

    fn temp_store(tag: &str) -> ModelStore {
        let dir = std::env::temp_dir().join(format!("mc-models-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ModelStore::new(dir)
    }

    fn file_url(path: &Path) -> String {
        format!("file:///{}", path.display().to_string().replace('\\', "/"))
    }

    #[test]
    fn an_official_file_installed_from_an_old_url_is_not_downloaded_again() {
        let store = temp_store("old-url");
        std::fs::create_dir_all(store.dir()).unwrap();
        // Stand-in bytes with a registry entry pointing at their real hash.
        std::fs::write(store.dir().join("m.bin"), b"model").unwrap();
        let official = sha256_file(&store.dir().join("m.bin")).unwrap();
        let spec = ModelSpec {
            name: "m".into(),
            description: String::new(),
            file: "m.bin".into(),
            url: "https://example.invalid/pinned/m.bin".into(),
            sha256: Some(official.to_uppercase()),
        };
        let old = LockEntry {
            file: "m.bin".into(),
            url: "https://example.invalid/main/m.bin".into(),
            sha256: official.clone(),
            size: 5,
            pinned: false,
            pulled_wall_ms: 1,
        };
        let lock = Lock {
            models: BTreeMap::from([("m".to_string(), old)]),
        };
        store.save_lock(&lock).unwrap();
        let outcome = store
            .pull_spec("m", spec, &lock, &PullOptions::default(), &mut |_, _| {})
            .unwrap();
        assert!(matches!(outcome, PullOutcome::AlreadyInstalled { .. }));
        let entry = &store.load_lock().models["m"];
        assert_eq!(entry.url, "https://example.invalid/pinned/m.bin");
        assert!(entry.pinned);
        assert_eq!(entry.pulled_wall_ms, 1);
    }

    #[test]
    fn a_custom_url_ignores_the_registry_checksum() {
        let store = temp_store("custom-url");
        let source = store.dir().join("src");
        std::fs::create_dir_all(&source).unwrap();
        let file = source.join("m.bin");
        std::fs::write(&file, b"custom bytes").unwrap();
        let name = builtin().remove(0).name;
        let opts = PullOptions {
            url: Some(file_url(&file)),
            ..PullOptions::default()
        };
        let outcome = store.pull(&name, &opts, &mut |_, _| {}).unwrap();
        assert!(matches!(
            outcome,
            PullOutcome::Downloaded { unpinned: true, .. }
        ));
        assert!(!store.load_lock().models[&name].pinned);

        let wrong = PullOptions {
            sha256: Some("0".repeat(64)),
            force: true,
            ..opts
        };
        assert!(matches!(
            store.pull(&name, &wrong, &mut |_, _| {}),
            Err(ModelError::Mismatch { .. })
        ));
    }

    #[test]
    fn the_registry_has_unique_names_and_files() {
        let all = builtin();
        let mut names: Vec<_> = all.iter().map(|m| m.name.clone()).collect();
        let mut files: Vec<_> = all.iter().map(|m| m.file.clone()).collect();
        names.sort();
        files.sort();
        names.dedup();
        files.dedup();
        assert_eq!(names.len(), all.len());
        assert_eq!(files.len(), all.len());
        assert!(all.iter().all(|m| m.url.starts_with("https://")));
    }
}
