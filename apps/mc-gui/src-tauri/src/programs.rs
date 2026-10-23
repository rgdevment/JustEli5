//! Finding `eli5` and other programs, and starting them without a console window.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The CLI as installed now, then its name before JustEli5.
const CLI_NAMES: [&str; 2] = ["eli5", "mc"];

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from)
}

/// Folders a terminal would have on PATH but an app opened from Finder or the Start menu may not.
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
        dirs.push(home.join(".local").join("bin"));
    }
    if cfg!(target_os = "macos") {
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
    }
    dirs
}

/// PATH plus the extra folders that exist; children get it too, so `mc` finds `claude` or `tisty`.
pub fn search_path() -> OsString {
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for dir in extra_dirs() {
        if dir.is_dir() && !paths.contains(&dir) {
            paths.push(dir);
        }
    }
    std::env::join_paths(paths).unwrap_or_default()
}

fn candidates(name: &str) -> Vec<PathBuf> {
    let extensions: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&search_path())
        .flat_map(|dir| {
            extensions
                .iter()
                .map(move |ext| dir.join(format!("{name}{ext}")))
        })
        .filter(|path| path.is_file())
        .collect()
}

pub fn find(name: &str) -> Option<PathBuf> {
    candidates(name).into_iter().next()
}

/// A command with the widened PATH and, on Windows, no console window flashing up.
pub fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("PATH", search_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Other tools are called `mc` too (Midnight Commander, the MinIO client): ask before trusting one.
fn is_eli5(path: &Path) -> bool {
    command(path)
        .arg("--help")
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("meeting capture"))
}

/// `ELI5_BIN` or `MC_BIN`, then the CLI next to the app, then PATH (with `~/.cargo/bin`).
pub fn locate_mc() -> Option<PathBuf> {
    let explicit = ["ELI5_BIN", "MC_BIN"]
        .into_iter()
        .filter_map(|var| std::env::var_os(var).map(PathBuf::from));
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let beside = CLI_NAMES
        .into_iter()
        .filter_map(move |name| dir.as_ref().map(|d| d.join(exe(name))));
    explicit
        .chain(beside)
        .filter(|path| path.is_file())
        .chain(CLI_NAMES.into_iter().flat_map(candidates))
        .find(|path| is_eli5(path))
}
