//! Finding `eli5` and other programs, and starting them without a console window.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The CLI as installed now, then its name before JustEli5.
const CLI_NAMES: [&str; 2] = ["eli5", "mc"];

const PATH_MARK: &str = "__ELI5_LOGIN_PATH__";
/// A profile that hangs (a prompt, a slow plugin manager) must not hold the window back.
const LOGIN_SHELL_WAIT: Duration = Duration::from_secs(3);

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

fn login_shell_path() -> Option<OsString> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let mut child = Command::new(shell)
        .args([
            "-ilc",
            &format!("printf %s {PATH_MARK}; /usr/bin/printenv PATH"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + LOGIN_SHELL_WAIT;
    while child.try_wait().ok()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    marked_path(&out)
}

fn marked_path(out: &str) -> Option<OsString> {
    let path = out.rsplit_once(PATH_MARK)?.1.trim();
    (!path.is_empty()).then(|| path.into())
}

/// A command with no console window flashing up on Windows.
pub fn quiet(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The folders programs are looked up in; children get them too, so `eli5` finds `copilot` or `tisty`.
#[derive(Clone)]
pub struct SearchPath(OsString);

impl SearchPath {
    /// Finder starts apps with launchd's short PATH, without what the shell profile adds (nvm, fnm, volta).
    pub fn detect() -> Self {
        let login = cfg!(target_os = "macos").then(login_shell_path).flatten();
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        Self::from_parts(login.as_deref(), &inherited, &extra_dirs())
    }

    fn from_parts(login: Option<&OsStr>, inherited: &OsStr, extra: &[PathBuf]) -> Self {
        let listed = login
            .into_iter()
            .chain([inherited])
            .flat_map(std::env::split_paths);
        let existing = extra.iter().filter(|dir| dir.is_dir()).cloned();
        let mut dirs: Vec<PathBuf> = Vec::new();
        for dir in listed.chain(existing) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        Self(std::env::join_paths(dirs).unwrap_or_else(|_| inherited.to_os_string()))
    }

    fn candidates(&self, name: &str) -> Vec<PathBuf> {
        let extensions: &[&str] = if cfg!(windows) {
            &[".exe", ".cmd", ".bat"]
        } else {
            &[""]
        };
        std::env::split_paths(&self.0)
            .flat_map(|dir| {
                extensions
                    .iter()
                    .map(move |ext| dir.join(format!("{name}{ext}")))
            })
            .filter(|path| path.is_file())
            .collect()
    }

    pub fn find(&self, name: &str) -> Option<PathBuf> {
        self.candidates(name).into_iter().next()
    }

    pub fn command(&self, program: &Path) -> Command {
        let mut cmd = quiet(program);
        cmd.env("PATH", &self.0);
        cmd
    }

    /// Other tools are called `mc` too (Midnight Commander, the MinIO client): ask before trusting one.
    fn is_eli5(&self, path: &Path) -> bool {
        self.command(path)
            .arg("--help")
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("meeting capture"))
    }

    /// `ELI5_BIN` or `MC_BIN`, then the CLI next to the app, then PATH (with `~/.cargo/bin`).
    pub fn locate_mc(&self) -> Option<PathBuf> {
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
            .chain(CLI_NAMES.into_iter().flat_map(|name| self.candidates(name)))
            .find(|path| self.is_eli5(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_path_is_what_follows_the_mark() {
        let out =
            format!("Welcome to zsh\n{PATH_MARK}/Users/a/.nvm/versions/node/v24/bin:/usr/bin\n");
        assert_eq!(
            marked_path(&out),
            Some("/Users/a/.nvm/versions/node/v24/bin:/usr/bin".into())
        );
        assert_eq!(marked_path("no mark here"), None);
        assert_eq!(marked_path(&format!("{PATH_MARK}\n")), None);
    }

    #[test]
    fn the_login_path_goes_first_then_the_app_s_then_the_extra_folders_that_exist() {
        let join = |dirs: &[&str]| std::env::join_paths(dirs).unwrap();
        let here = std::env::temp_dir();
        let extra = [here.clone(), here.join("missing-eli5-test-dir")];
        let paths = SearchPath::from_parts(
            Some(&join(&["nvm", "brew"])),
            &join(&["usr", "brew"]),
            &extra,
        );
        let dirs: Vec<PathBuf> = std::env::split_paths(&paths.0).collect();
        assert_eq!(
            dirs,
            [PathBuf::from("nvm"), "brew".into(), "usr".into(), here]
        );
        let alone = SearchPath::from_parts(None, &join(&["usr"]), &[]);
        assert_eq!(alone.0, join(&["usr"]));
    }
}
