//! Desktop window for JustEli5: it drives the `eli5` CLI (`--json`) as a subprocess.

mod config_file;
mod jobs;
mod legacy;
mod programs;
mod settings;
mod system;

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, State};

/// Where `mc` was found, refreshed by `mc_locate`.
#[derive(Default)]
struct McPath(Mutex<Option<PathBuf>>);

impl McPath {
    fn get(&self) -> Result<PathBuf, String> {
        self.0
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or_else(|| "mc no está instalado".to_string())
    }
}

#[derive(Serialize)]
struct RunOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[tauri::command]
async fn mc_locate(mc: State<'_, McPath>) -> Result<Option<String>, String> {
    let found = tauri::async_runtime::spawn_blocking(programs::locate_mc)
        .await
        .map_err(|e| e.to_string())?;
    *mc.0.lock().map_err(|e| e.to_string())? = found.clone();
    Ok(found.map(|p| p.display().to_string()))
}

/// A short `mc` call whose whole output is wanted (`--json` documents).
#[tauri::command]
async fn mc_run(mc: State<'_, McPath>, args: Vec<String>) -> Result<RunOutput, String> {
    let program = mc.get()?;
    tauri::async_runtime::spawn_blocking(move || {
        let out = programs::command(&program)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        Ok(RunOutput {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A long `mc` call streamed as job events; `stdin` keeps a pipe for `job_write`.
#[tauri::command]
fn mc_spawn(
    app: AppHandle,
    mc: State<'_, McPath>,
    id: String,
    args: Vec<String>,
    stdin: bool,
) -> Result<(), String> {
    let mut cmd = programs::command(&mc.get()?);
    cmd.args(args);
    jobs::spawn(&app, id, cmd, stdin)
}

#[tauri::command]
fn job_write(app: AppHandle, id: String, line: String) -> Result<(), String> {
    jobs::write(&app, &id, &line)
}

#[tauri::command]
fn prereqs() -> Vec<system::Prereq> {
    system::prereqs()
}

#[tauri::command]
fn prereq_install(app: AppHandle, id: String, tool: String) -> Result<(), String> {
    jobs::spawn(&app, id, system::installer(&tool)?, false)
}

#[tauri::command]
async fn agents_detect() -> Result<system::Agents, String> {
    tauri::async_runtime::spawn_blocking(system::agents)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn mc_source() -> Option<String> {
    system::source_checkout().map(|p| p.display().to_string())
}

#[tauri::command]
fn mc_install(app: AppHandle, id: String) -> Result<(), String> {
    jobs::spawn(&app, id, system::mc_installer()?, false)
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    system::open(&path)
}

#[tauri::command]
fn open_settings(page: String) -> Result<(), String> {
    let uri = system::settings_uri(&page).ok_or("no settings page for this system")?;
    system::open(uri)
}

#[tauri::command]
fn settings_load(app: AppHandle) -> Value {
    settings::load(&app)
}

#[tauri::command]
fn settings_save(app: AppHandle, value: Value) -> Result<(), String> {
    settings::save(&app, &value)
}

#[tauri::command]
fn context_write(app: AppHandle, text: String) -> Result<Option<String>, String> {
    settings::write_context(&app, &text)
}

/// The participant notes older versions kept (name to who they are), for the memory to take in.
#[tauri::command]
fn people_file(app: AppHandle) -> Option<String> {
    settings::existing_file(&app, "people.json")
}

#[tauri::command]
fn note_image_save(session_dir: String, ext: String, bytes: Vec<u8>) -> Result<String, String> {
    settings::save_note_image(&session_dir, &ext, &bytes)
}

#[tauri::command]
fn config_load() -> config_file::Loaded {
    config_file::load()
}

#[tauri::command]
fn config_create() -> Result<String, String> {
    config_file::create()
}

#[tauri::command]
async fn config_save(mc: State<'_, McPath>, value: Value) -> Result<String, String> {
    let program = mc.get()?;
    tauri::async_runtime::spawn_blocking(move || config_file::save(&program, &value))
        .await
        .map_err(|e| e.to_string())?
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            legacy::migrate_app_dir(app.handle());
            Ok(())
        })
        .manage(McPath::default())
        .manage(jobs::Jobs::default())
        .invoke_handler(tauri::generate_handler![
            mc_locate,
            mc_run,
            mc_spawn,
            job_write,
            prereqs,
            prereq_install,
            agents_detect,
            mc_source,
            mc_install,
            open_path,
            open_settings,
            settings_load,
            settings_save,
            context_write,
            people_file,
            note_image_save,
            config_load,
            config_create,
            config_save,
        ])
        .run(tauri::generate_context!())
        .expect("the window could not start");
}
