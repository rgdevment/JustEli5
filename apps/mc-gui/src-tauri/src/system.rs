//! What the app asks of the operating system: opening files and settings pages,
//! and the build prerequisites of `mc` with the package-manager command that installs each.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::programs::{command, find};

pub fn open(target: &str) -> Result<(), String> {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // explorer returns 1 even when it opened the target.
    command(Path::new(opener))
        .arg(target)
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// The settings page behind a fix, by name; unknown names are refused.
pub fn settings_uri(page: &str) -> Option<&'static str> {
    if cfg!(windows) {
        return match page {
            "microphone" => Some("ms-settings:privacy-microphone"),
            "sound" => Some("ms-settings:sound"),
            _ => None,
        };
    }
    if cfg!(target_os = "macos") {
        return match page {
            "microphone" => {
                Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            }
            "screen" => Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            ),
            "accessibility" => Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            ),
            "sound" => Some("x-apple.systempreferences:com.apple.Sound-Settings.extension"),
            _ => None,
        };
    }
    None
}

#[derive(Serialize)]
pub struct Prereq {
    id: &'static str,
    label: &'static str,
    found: bool,
    /// The exact command the install button runs (shown before asking).
    install: Option<String>,
    /// What to do by hand when nothing can install it here.
    manual: &'static str,
    /// Whether the installer will ask for administrator rights.
    elevates: bool,
}

struct Tool {
    id: &'static str,
    label: &'static str,
    found: fn() -> bool,
    package: Option<&'static str>,
    manual: &'static str,
    elevates: bool,
}

fn on_path(name: &str) -> bool {
    find(name).is_some()
}

#[cfg(windows)]
fn tools() -> Vec<Tool> {
    vec![
        Tool {
            id: "rust",
            label: "Rust (cargo)",
            found: || on_path("cargo"),
            package: Some("Rustlang.Rustup"),
            manual: "Instala Rust desde https://rustup.rs",
            elevates: false,
        },
        Tool {
            id: "git",
            label: "Git",
            found: || on_path("git"),
            package: Some("Git.Git"),
            manual: "Instala Git desde https://git-scm.com",
            elevates: true,
        },
        Tool {
            id: "cmake",
            label: "CMake (para Whisper)",
            found: || on_path("cmake") || program_files_has("CMake\\bin\\cmake.exe"),
            package: Some("Kitware.CMake"),
            manual: "Instala CMake desde https://cmake.org/download",
            elevates: true,
        },
        Tool {
            id: "llvm",
            label: "LLVM (para Whisper)",
            found: || program_files_has("LLVM\\bin\\libclang.dll"),
            package: Some("LLVM.LLVM"),
            manual: "Instala LLVM desde https://releases.llvm.org",
            elevates: true,
        },
        Tool {
            id: "msvc",
            label: "Herramientas de compilación de C++ (Visual Studio Build Tools)",
            found: || {
                std::env::var_os("ProgramFiles(x86)").is_some_and(|dir| {
                    PathBuf::from(dir)
                        .join("Microsoft Visual Studio\\Installer\\vswhere.exe")
                        .is_file()
                })
            },
            package: Some("Microsoft.VisualStudio.2022.BuildTools"),
            manual: "Instala «Desktop development with C++» de Visual Studio Build Tools",
            elevates: true,
        },
    ]
}

#[cfg(windows)]
fn program_files_has(rel: &str) -> bool {
    std::env::var_os("ProgramFiles").is_some_and(|dir| PathBuf::from(dir).join(rel).is_file())
}

#[cfg(not(windows))]
fn tools() -> Vec<Tool> {
    vec![
        Tool {
            id: "xcode",
            label: "Herramientas de línea de comandos de Xcode",
            found: || {
                Command::new("xcode-select")
                    .arg("-p")
                    .output()
                    .is_ok_and(|o| o.status.success())
            },
            package: None,
            manual: "Ejecuta en Terminal: xcode-select --install",
            elevates: false,
        },
        Tool {
            id: "rust",
            label: "Rust (cargo)",
            found: || on_path("cargo"),
            package: Some("rustup"),
            manual: "Instala Rust desde https://rustup.rs",
            elevates: false,
        },
        Tool {
            id: "cmake",
            label: "CMake (para Whisper)",
            found: || on_path("cmake"),
            package: Some("cmake"),
            manual: "Instala CMake desde https://cmake.org/download",
            elevates: false,
        },
    ]
}

/// The package manager command for `package`, when the manager is installed.
fn install_command(package: &str) -> Option<(PathBuf, Vec<String>)> {
    if cfg!(windows) {
        let winget = find("winget")?;
        let mut args: Vec<String> = [
            "install",
            "--id",
            package,
            "--exact",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ]
        .map(String::from)
        .to_vec();
        if package == "Microsoft.VisualStudio.2022.BuildTools" {
            args.extend([
                "--override".to_string(),
                "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
                    .to_string(),
            ]);
        }
        Some((winget, args))
    } else if cfg!(target_os = "macos") {
        let brew = find("brew")?;
        Some((brew, vec!["install".to_string(), package.to_string()]))
    } else {
        None
    }
}

fn shown(program: &Path, args: &[String]) -> String {
    let name = program
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let quoted: Vec<String> = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect();
    format!("{name} {}", quoted.join(" "))
}

pub fn prereqs() -> Vec<Prereq> {
    tools()
        .into_iter()
        .map(|tool| {
            let install = tool
                .package
                .and_then(install_command)
                .map(|(program, args)| shown(&program, &args));
            Prereq {
                id: tool.id,
                label: tool.label,
                found: (tool.found)(),
                install,
                manual: tool.manual,
                elevates: tool.elevates,
            }
        })
        .collect()
}

/// The installer for one prerequisite, ready to run as a job.
pub fn installer(id: &str) -> Result<Command, String> {
    let tool = tools()
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("unknown prerequisite {id}"))?;
    let (program, args) = tool
        .package
        .and_then(install_command)
        .ok_or_else(|| tool.manual.to_string())?;
    let mut cmd = command(&program);
    cmd.args(args);
    Ok(cmd)
}

/// The repository this app was built from, when it is still on disk: `mc` can be built from it.
pub fn source_checkout() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    root.join("crates/mc-cli/Cargo.toml")
        .is_file()
        .then(|| root.canonicalize().unwrap_or(root))
}

/// `cargo install` of `mc` with Whisper from the checkout.
pub fn mc_installer() -> Result<Command, String> {
    let root = source_checkout().ok_or("no source checkout of JustEli5 next to this app")?;
    let cargo = find("cargo").ok_or("cargo is not installed")?;
    let mut cmd = command(&cargo);
    cmd.args(["install", "--locked", "--path"])
        .arg(root.join("crates").join("mc-cli"))
        .args(["--features", "whisper"]);
    if cfg!(windows) {
        cmd.env("LIBCLANG_PATH", r"C:\Program Files\LLVM\bin");
    }
    Ok(cmd)
}

/// The AI agents this machine can use for the analyst and the minutes.
#[derive(Serialize)]
pub struct Agents {
    claude: Option<String>,
    copilot: Option<String>,
    anthropic_key: bool,
    openai_key: bool,
    ollama: bool,
    lmstudio: bool,
}

/// A local model server answers on its port; a short timeout keeps the wizard quick.
fn listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
}

fn has_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

pub fn agents() -> Agents {
    let found = |name: &str| find(name).map(|p| p.display().to_string());
    Agents {
        claude: found("claude"),
        copilot: found("copilot"),
        anthropic_key: has_env("ANTHROPIC_API_KEY"),
        openai_key: has_env("OPENAI_API_KEY"),
        ollama: listening(11434),
        lmstudio: listening(1234),
    }
}
