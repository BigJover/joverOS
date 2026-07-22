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
    url: Option<String>,
    reply: Option<String>,
}

const OLLAMA_URL: &str = "http://localhost:11434/api/chat";
const ROUTER_MODEL: &str = "llama3.1:8b";

const ROUTER_PROMPT: &str = "You route commands for a desktop agent bar. Classify the user's input.\n\
- app_launch: the user wants to open or launch an already-installed application (NOT install new software). Set \"app\" to the application name only.\n\
- web_open: the user names a website, or a site plus something on it — a section, a channel, a profile, or content to find there. Set \"url\" to the real https:// URL:\n\
  * site alone -> homepage (\"youtube\" -> https://www.youtube.com/)\n\
  * site section -> its page (\"linkedin job board\" -> https://www.linkedin.com/jobs/)\n\
  * a well-known channel/profile/person on the site -> their page (\"youtube mr beast\" -> https://www.youtube.com/@MrBeast, \"twitch jynxzi\" -> https://www.twitch.tv/jynxzi)\n\
  * content to find on the site, or a person you don't know the exact page for -> the site's own search URL (\"youtube lofi study mix\" -> https://www.youtube.com/results?search_query=lofi+study+mix, \"amazon airpods\" -> https://www.amazon.com/s?k=airpods)\n\
- web_search: the user wants to search the web or look up a question or topic. Set \"query\" to the search terms only.\n\
For web_open and web_search: if the user names a browser (e.g. chrome, safari, firefox), also set \"app\" to that browser's name.\n\
- file_search: the user wants to FIND files, documents, or folders on this computer (finding only, no changes). Set \"query\" to only the words likely in the file's name or contents — drop filler like \"find\", \"my\", \"that\", \"file\".\n\
- unknown: anything else — including deleting or cleaning up files, emptying trash, changing settings, or installing software. Set \"reply\" to one short sentence stating you can't do that yet.\n\
Respond with JSON only.";

// The schema is enforced by Ollama's structured-output mode, so the model can
// only ever answer in a shape the frontend knows how to execute.
#[tauri::command]
async fn route_intent(input: String) -> Result<Intent, String> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "intent": { "type": "string", "enum": ["app_launch", "web_open", "web_search", "file_search", "unknown"] },
            "app": { "type": "string" },
            "query": { "type": "string" },
            "url": { "type": "string" },
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

// --- File search (M2). Spotlight IS the local file index on macOS; the Linux
// shell will need its own indexer at M5. Read-only — finding needs no
// permission prompt, opening is user-initiated, and changes will only ever
// come through the confirmation layer.

#[derive(serde::Serialize, Clone)]
struct FileHit {
    name: String,
    path: String,
}

fn mdfind(args: &[&str]) -> Vec<String> {
    Command::new("mdfind")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[tauri::command]
fn search_files(query: String) -> Vec<FileHit> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut paths: Vec<String> = Vec::new();
    // Name matches first (usually what's meant), content matches fill the rest.
    for list in [
        mdfind(&["-onlyin", &home, "-name", &query]),
        mdfind(&["-onlyin", &home, &query]),
    ] {
        for p in list {
            // caches, dependencies, dotfolders — machinery, not the user's files
            if p.contains("/Library/") || p.contains("/node_modules/") || p.contains("/.") {
                continue;
            }
            if paths.len() >= 8 {
                break;
            }
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    paths
        .into_iter()
        .map(|p| FileHit {
            name: Path::new(&p)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone()),
            path: p,
        })
        .collect()
}

// --- Agent memory (SQLite, per spec) — first table: learned web destinations.

fn mem_db(app: &AppHandle) -> Result<rusqlite::Connection, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let conn = rusqlite::Connection::open(dir.join("memory.db")).map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS web_memory (
            input TEXT PRIMARY KEY,
            url TEXT NOT NULL,
            hits INTEGER NOT NULL DEFAULT 1,
            last_used TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn normalize_input(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

#[tauri::command]
fn recall_web(app: AppHandle, input: String) -> Option<String> {
    let conn = mem_db(&app).ok()?;
    let key = normalize_input(&input);
    let url: Option<String> = conn
        .query_row("SELECT url FROM web_memory WHERE input = ?1", [&key], |r| r.get(0))
        .ok();
    if url.is_some() {
        let _ = conn.execute(
            "UPDATE web_memory SET hits = hits + 1, last_used = datetime('now') WHERE input = ?1",
            [&key],
        );
    }
    url
}

#[tauri::command]
fn remember_web(app: AppHandle, input: String, url: String) -> Result<(), String> {
    let conn = mem_db(&app)?;
    conn.execute(
        "INSERT INTO web_memory (input, url) VALUES (?1, ?2)
         ON CONFLICT(input) DO UPDATE SET url = ?2, last_used = datetime('now')",
        [&normalize_input(&input), &url],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn forget_web(app: AppHandle, input: String) -> Result<(), String> {
    let conn = mem_db(&app)?;
    conn.execute(
        "DELETE FROM web_memory WHERE input = ?1",
        [&normalize_input(&input)],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// Find a page by searching the live web (DuckDuckGo HTML — keyless) instead
// of trusting the model to know the URL. Returns the top result, optionally
// constrained to one site.
#[tauri::command]
async fn resolve_web(query: String, site_host: Option<String>) -> Result<String, String> {
    let q = match &site_host {
        Some(host) => format!("site:{host} {query}"),
        None => query,
    };
    let client = reqwest::Client::new();
    let html = client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", q.as_str())])
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
        )
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await
        .map_err(|e| format!("search failed: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    for chunk in html.split("uddg=").skip(1) {
        let encoded = chunk.split(&['&', '"'][..]).next().unwrap_or("");
        let url = urlencoding::decode(encoded).map_err(|e| e.to_string())?.into_owned();
        if !url.starts_with("https://") {
            continue;
        }
        if let Some(host) = &site_host {
            let ok = url::host_matches(&url, host);
            if !ok {
                continue;
            }
        }
        return Ok(url);
    }
    Err("no results".into())
}

mod url {
    pub fn host_matches(url: &str, host: &str) -> bool {
        url.strip_prefix("https://")
            .and_then(|rest| rest.split('/').next())
            .is_some_and(|h| h == host || h.ends_with(&format!(".{host}")))
    }
}

// Non-destructive (opens a browser tab), so it skips the confirmation layer.
// Model-guessed deep links can be wrong: when a fallback is given, the url is
// checked first and a 404/410 swaps in the fallback. Only those two statuses
// count as "page doesn't exist" — bot-blocking sites answer 403/999 for pages
// that load fine in a real browser.
#[tauri::command]
async fn open_url(
    app: AppHandle,
    url: String,
    browser_path: Option<String>,
    fallback_url: Option<String>,
) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err(format!("refusing non-https url: {url}"));
    }
    let mut target = url.clone();
    if let Some(fallback) = fallback_url.filter(|f| f.starts_with("https://")) {
        let client = reqwest::Client::new();
        let status = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(4))
            .send()
            .await
            .map(|r| r.status().as_u16());
        if matches!(status, Ok(404 | 410)) {
            target = fallback;
        }
    }
    let mut cmd = Command::new("open");
    if let Some(path) = &browser_path {
        cmd.arg("-a").arg(path);
    }
    cmd.arg(&target).spawn().map_err(|e| e.to_string())?;
    hide_bar(app);
    Ok(target)
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
            search_files,
            open_url,
            resolve_web,
            recall_web,
            remember_web,
            forget_web
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
