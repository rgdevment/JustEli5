//! The model cache against local files and a throwaway HTTP server.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use mc_models::{ModelError, ModelStore, PullOptions, PullOutcome, sha256_file};

const BODY: &[u8] = b"pretend these are model weights, 0123456789";

fn source(dir: &Path, name: &str, bytes: &[u8]) -> (PathBuf, String) {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    let url = format!("file://{}", path.display());
    (path, url)
}

fn pull(store: &ModelStore, name: &str, opts: &PullOptions) -> Result<PullOutcome, ModelError> {
    store.pull(name, opts, &mut |_, _| {})
}

#[test]
fn a_custom_model_is_pulled_recorded_and_found() {
    let sources = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path().join("models"));
    let (_, url) = source(sources.path(), "m.bin", BODY);
    let outcome = pull(
        &store,
        "mi-modelo",
        &PullOptions {
            url: Some(url),
            ..PullOptions::default()
        },
    )
    .unwrap();
    let PullOutcome::Downloaded {
        path,
        sha256,
        size,
        unpinned,
    } = outcome
    else {
        panic!("expected a download");
    };
    assert!(unpinned, "no checksum was known in advance");
    assert_eq!(size, BODY.len() as u64);
    assert_eq!(sha256_file(&path).unwrap(), sha256);
    assert_eq!(store.path("mi-modelo").unwrap(), path);

    let listed = store.list();
    let mine = listed.iter().find(|m| m.name == "mi-modelo").unwrap();
    assert!(mine.installed && !mine.builtin && !mine.pinned);
    assert_eq!(mine.sha256.as_deref(), Some(sha256.as_str()));
    // Built-in models are listed too, not installed.
    let turbo = listed
        .iter()
        .find(|m| m.name == "whisper-large-v3-turbo-q5_0")
        .unwrap();
    assert!(turbo.builtin && !turbo.installed);

    assert!(store.verify("mi-modelo").unwrap().ok);
    assert!(store.remove("mi-modelo").unwrap());
    assert!(!path.exists());
    assert!(matches!(
        store.path("mi-modelo"),
        Err(ModelError::Unknown(_))
    ));
}

#[test]
fn a_second_pull_is_a_no_op_until_forced_or_corrupted() {
    let sources = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path());
    let (_, url) = source(sources.path(), "m.bin", BODY);
    let opts = PullOptions {
        url: Some(url),
        ..PullOptions::default()
    };
    assert!(matches!(
        pull(&store, "m", &opts).unwrap(),
        PullOutcome::Downloaded { .. }
    ));
    assert!(matches!(
        pull(&store, "m", &opts).unwrap(),
        PullOutcome::AlreadyInstalled { .. }
    ));
    let forced = PullOptions {
        force: true,
        ..PullOptions {
            url: opts.url.clone(),
            ..PullOptions::default()
        }
    };
    assert!(matches!(
        pull(&store, "m", &forced).unwrap(),
        PullOutcome::Downloaded { .. }
    ));

    // A damaged file no longer matches the recorded hash: verify says so and a
    // plain pull fetches it again.
    let path = store.path("m").unwrap();
    std::fs::write(&path, b"damaged").unwrap();
    let report = store.verify("m").unwrap();
    assert!(!report.ok);
    assert_ne!(Some(&report.actual), report.expected.as_ref());
    assert!(matches!(
        pull(&store, "m", &opts).unwrap(),
        PullOutcome::Downloaded { .. }
    ));
    assert!(store.verify("m").unwrap().ok);
}

#[test]
fn a_wrong_checksum_leaves_nothing_behind() {
    let sources = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path());
    let (_, url) = source(sources.path(), "m.bin", BODY);
    let error = pull(
        &store,
        "m",
        &PullOptions {
            url: Some(url.clone()),
            sha256: Some("00".repeat(32)),
            ..PullOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, ModelError::Mismatch { .. }), "{error}");
    let leftovers: Vec<_> = std::fs::read_dir(cache.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(matches!(store.path("m"), Err(ModelError::Unknown(_))));

    // The right checksum (upper case is fine) pins it.
    let right = sha256_file(&sources.path().join("m.bin"))
        .unwrap()
        .to_uppercase();
    let outcome = pull(
        &store,
        "m",
        &PullOptions {
            url: Some(url),
            sha256: Some(right),
            ..PullOptions::default()
        },
    )
    .unwrap();
    assert!(matches!(
        outcome,
        PullOutcome::Downloaded {
            unpinned: false,
            ..
        }
    ));
    assert!(store.list().iter().find(|m| m.name == "m").unwrap().pinned);
}

#[test]
fn a_pinned_model_refuses_a_changed_source() {
    let sources = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path());
    let (path, url) = source(sources.path(), "m.bin", BODY);
    let hash = sha256_file(&path).unwrap();
    let opts = PullOptions {
        url: Some(url.clone()),
        sha256: Some(hash),
        ..PullOptions::default()
    };
    pull(&store, "m", &opts).unwrap();
    // Same URL, different bytes, no new checksum given: still held to the old one.
    std::fs::write(&path, b"swapped under our feet").unwrap();
    let again = PullOptions {
        url: Some(url),
        force: true,
        ..PullOptions::default()
    };
    assert!(matches!(
        pull(&store, "m", &again),
        Err(ModelError::Mismatch { .. })
    ));
    assert!(store.verify("m").unwrap().ok, "the good copy is untouched");
}

#[test]
fn bad_input_is_refused() {
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path());
    let none = PullOptions::default();
    assert!(matches!(
        pull(&store, "nope", &none),
        Err(ModelError::Unknown(_))
    ));
    assert!(matches!(
        pull(&store, "../x", &none),
        Err(ModelError::BadName(_))
    ));
    let ftp = PullOptions {
        url: Some("ftp://example.com/m.bin".into()),
        ..PullOptions::default()
    };
    assert!(matches!(
        pull(&store, "m", &ftp),
        Err(ModelError::BadUrl(_))
    ));
    let missing = PullOptions {
        url: Some("file:///definitely/not/here.bin".into()),
        ..PullOptions::default()
    };
    assert!(matches!(
        pull(&store, "m", &missing),
        Err(ModelError::Io { .. })
    ));
    assert!(matches!(
        store.verify("whisper-base"),
        Err(ModelError::NotInstalled(_))
    ));
    assert!(!store.remove("whisper-base").unwrap());
}

/// Serves `body` once, honouring nothing but the path; returns the base URL.
fn serve(body: Vec<u8>, status: &'static str, claim_length: Option<usize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                claim_length.unwrap_or(body.len())
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://127.0.0.1:{port}/m.bin")
}

#[test]
fn downloads_over_http_and_reports_progress() {
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path());
    let body = vec![7u8; 3 * 1024 * 1024 + 5];
    let url = serve(body.clone(), "200 OK", None);
    let mut seen = Vec::new();
    let outcome = store
        .pull(
            "web",
            &PullOptions {
                url: Some(url),
                ..PullOptions::default()
            },
            &mut |done, total| seen.push((done, total)),
        )
        .unwrap();
    let PullOutcome::Downloaded { size, .. } = outcome else {
        panic!()
    };
    assert_eq!(size, body.len() as u64);
    assert!(seen.len() >= 3);
    assert_eq!(seen.last().unwrap().0, body.len() as u64);
    assert!(seen.iter().all(|(_, t)| *t == Some(body.len() as u64)));
}

#[test]
fn http_errors_and_short_downloads_leave_no_file() {
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path());
    let not_found = serve(b"nope".to_vec(), "404 Not Found", None);
    let error = pull(
        &store,
        "gone",
        &PullOptions {
            url: Some(not_found),
            ..PullOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, ModelError::Download(_)), "{error}");

    // The server promises more bytes than it sends.
    let short = serve(vec![1u8; 100], "200 OK", Some(5000));
    let error = pull(
        &store,
        "short",
        &PullOptions {
            url: Some(short),
            ..PullOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, ModelError::Download(_)), "{error}");
    let files: Vec<_> = std::fs::read_dir(cache.path()).unwrap().flatten().collect();
    assert!(files.is_empty(), "{files:?}");
}

#[test]
fn a_tampered_lock_cannot_point_outside_the_models_folder() {
    let sources = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let dir = cache.path().join("models");
    let store = ModelStore::new(&dir);
    let victim = cache.path().join("precious.txt");
    std::fs::write(&victim, b"keep me").unwrap();
    let (_, url) = source(sources.path(), "m.bin", BODY);
    pull(
        &store,
        "m",
        &PullOptions {
            url: Some(url),
            ..PullOptions::default()
        },
    )
    .unwrap();
    let lock = std::fs::read_to_string(dir.join("lock.json"))
        .unwrap()
        .replace("custom-m.bin", "../precious.txt");
    std::fs::write(dir.join("lock.json"), lock).unwrap();
    assert!(matches!(store.path("m"), Err(ModelError::Unknown(_))));
    assert!(matches!(store.remove("m"), Err(ModelError::Unknown(_))));
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
}

#[test]
fn a_custom_model_never_shares_a_file_with_a_built_in_one() {
    let sources = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path());
    let (_, url) = source(sources.path(), "m.bin", BODY);
    pull(
        &store,
        "ggml-base",
        &PullOptions {
            url: Some(url),
            ..PullOptions::default()
        },
    )
    .unwrap();
    assert!(!cache.path().join("ggml-base.bin").exists());
    assert!(
        store
            .list()
            .iter()
            .find(|m| m.name == "whisper-base")
            .is_some_and(|m| !m.installed)
    );
    store.remove("ggml-base").unwrap();
}

#[test]
fn a_new_url_is_fetched_even_if_the_model_is_installed() {
    let sources = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let store = ModelStore::new(cache.path());
    let (_, first) = source(sources.path(), "a.bin", b"first version");
    let (_, second) = source(sources.path(), "b.bin", b"second version!");
    pull(
        &store,
        "m",
        &PullOptions {
            url: Some(first),
            ..PullOptions::default()
        },
    )
    .unwrap();
    let outcome = pull(
        &store,
        "m",
        &PullOptions {
            url: Some(second),
            ..PullOptions::default()
        },
    )
    .unwrap();
    assert!(
        matches!(outcome, PullOutcome::Downloaded { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read(store.path("m").unwrap()).unwrap(),
        b"second version!"
    );
}

#[test]
fn a_recorded_hash_stays_unofficial_and_concurrent_pulls_do_not_lose_entries() {
    let sources = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (_, url) = source(sources.path(), "m.bin", BODY);
    let store = ModelStore::new(cache.path());
    let opts = PullOptions {
        url: Some(url.clone()),
        ..PullOptions::default()
    };
    pull(&store, "m", &opts).unwrap();
    let forced = PullOptions {
        url: Some(url.clone()),
        force: true,
        ..PullOptions::default()
    };
    let PullOutcome::Downloaded { unpinned, .. } = pull(&store, "m", &forced).unwrap() else {
        panic!()
    };
    assert!(
        unpinned,
        "re-pulling does not make a recorded hash official"
    );

    let dir = cache.path().to_path_buf();
    let handles: Vec<_> = (0..6)
        .map(|n| {
            let dir = dir.clone();
            let url = url.clone();
            std::thread::spawn(move || {
                let store = ModelStore::new(&dir);
                let opts = PullOptions {
                    url: Some(url),
                    ..PullOptions::default()
                };
                store
                    .pull(&format!("par-{n}"), &opts, &mut |_, _| {})
                    .unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let names: Vec<_> = store
        .list()
        .into_iter()
        .filter(|m| m.installed)
        .map(|m| m.name)
        .collect();
    assert_eq!(names.len(), 7, "{names:?}");
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part") || n.starts_with(".lock"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
