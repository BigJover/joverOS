use std::path::Path;
use std::process::Command;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

#[derive(serde::Serialize, Clone)]
struct AppEntry {
    name: String,
    path: String,
}

fn scan_dir(dir: &Path, out: &mut Vec<AppEntry>, depth: u8) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "app") {
            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(AppEntry {
                    name: name.to_string(),
                    path: path.to_string_lossy().into_owned(),
                });
            }
        } else if depth > 0 && path.is_dir() {
            // one level of subfolders catches /Applications/Utilities etc.
            scan_dir(&path, out, depth - 1);
        }
    }
}

#[tauri::command]
fn list_apps() -> Vec<AppEntry> {
    let mut dirs = vec![
        String::from("/Applications"),
        String::from("/System/Applications"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(format!("{home}/Applications"));
    }
    let mut apps = Vec::new();
    for dir in &dirs {
        scan_dir(Path::new(dir), &mut apps, 1);
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps.dedup_by(|a, b| a.path == b.path);
    apps
}

#[tauri::command]
fn launch_app(app: AppHandle, path: String) -> Result<(), String> {
    Command::new("open")
        .arg(&path)
        .spawn()
        .map_err(|e| e.to_string())?;
    hide_bar(app);
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Intent {
    intent: String,
    app: Option<String>,
    query: Option<String>,
    reply: Option<String>,
}

const OLLAMA_URL: &str = "http://localhost:11434/api/chat";
const ROUTER_MODEL: &str = "llama3.1:8b";

const ROUTER_PROMPT: &str = "You route commands for a desktop agent bar. Classify the user's input.\n\
- app_launch: the user wants to open or launch an already-installed application (NOT install new software). Set \"app\" to the application name only.\n\
- web_search: the user wants to search the web or look something up. Set \"query\" to the search terms only. If they name a browser (e.g. chrome, safari, firefox), also set \"app\" to that browser's name.\n\
- file_search: the user wants to FIND files, documents, or folders on this computer (finding only, no changes). Set \"query\" to the search terms.\n\
- unknown: anything else — including deleting or cleaning up files, emptying trash, changing settings, or installing software. Set \"reply\" to one short sentence stating you can't do that yet.\n\
Respond with JSON only.";

// The schema is enforced by Ollama's structured-output mode, so the model can
// only ever answer in a shape the frontend knows how to execute.
#[tauri::command]
async fn route_intent(input: String) -> Result<Intent, String> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "intent": { "type": "string", "enum": ["app_launch", "web_search", "file_search", "unknown"] },
            "app": { "type": "string" },
            "query": { "type": "string" },
            "reply": { "type": "string" }
        },
        "required": ["intent"]
    });
    let body = serde_json::json!({
        "model": ROUTER_MODEL,
        "stream": false,
        "format": schema,
        "options": { "temperature": 0 },
        "messages": [
            { "role": "system", "content": ROUTER_PROMPT },
            { "role": "user", "content": input }
        ]
    });
    let client = reqwest::Client::new();
    let resp = client
        .post(OLLAMA_URL)
        .timeout(std::time::Duration::from_secs(60))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("brain offline ({e})"))?;
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let content = v["message"]["content"]
        .as_str()
        .ok_or_else(|| format!("empty response: {v}"))?;
    serde_json::from_str(content).map_err(|e| format!("bad intent JSON: {e}"))
}

// Non-destructive (opens a browser tab), so it skips the confirmation layer.
#[tauri::command]
fn open_url(app: AppHandle, url: String, browser_path: Option<String>) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("refusing non-https url: {url}"));
    }
    let mut cmd = Command::new("open");
    if let Some(path) = &browser_path {
        cmd.arg("-a").arg(path);
    }
    cmd.arg(&url).spawn().map_err(|e| e.to_string())?;
    hide_bar(app);
    Ok(())
}

#[tauri::command]
fn hide_bar(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

fn toggle_bar(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    } else {
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("bar-shown", ());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcut(Shortcut::new(Some(Modifiers::ALT), Code::Space))
                .expect("failed to parse shortcut")
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_bar(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            // Accessory: no Dock icon, no menu bar takeover — the bar is an overlay,
            // not a foreground app.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            if let Some(window) = app.get_webview_window("main") {
                let handle = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let _ = handle.hide();
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_apps,
            launch_app,
            hide_bar,
            route_intent,
            open_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
