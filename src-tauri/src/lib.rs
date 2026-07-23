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
- file_organize: the user wants to tidy/organize/sort a folder's files into subfolders (moving only, never deleting). Set \"query\" to the folder name (e.g. downloads, desktop).\n\
- troubleshoot: the user reports a computer problem to diagnose — disk full, out of storage, wants to know what's taking up space. Set \"query\" to the problem area (disk).\n\
- unknown: anything else — including deleting files, emptying trash, changing settings, or installing software. Set \"reply\" to one short sentence stating you can't do that yet.\n\
Respond with JSON only.";

// One structured-output call to the local model: the schema means it can
// only ever answer in a shape the caller knows how to execute.
async fn ollama_json(system: &str, user: &str, schema: serde_json::Value) -> Result<String, String> {
    let body = serde_json::json!({
        "model": ROUTER_MODEL,
        "stream": false,
        "format": schema,
        "options": { "temperature": 0 },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
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
    v["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("empty response: {v}"))
}

#[tauri::command]
async fn route_intent(input: String) -> Result<Intent, String> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "intent": { "type": "string", "enum": ["app_launch", "web_open", "web_search", "file_search", "file_organize", "troubleshoot", "unknown"] },
            "app": { "type": "string" },
            "query": { "type": "string" },
            "url": { "type": "string" },
            "reply": { "type": "string" }
        },
        "required": ["intent"]
    });
    let content = ollama_json(ROUTER_PROMPT, &input, schema).await?;
    serde_json::from_str(&content).map_err(|e| format!("bad intent JSON: {e}"))
}

// --- File search (M2). Spotlight IS the local file index on macOS; the Linux
// shell will need its own indexer at M5. Read-only — finding needs no
// permission prompt, opening is user-initiated, and changes will only ever
// come through the confirmation layer.

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct FileHit {
    name: String,
    path: String,
    // metadata rides along so "recent"/"biggest" can order results
    mtime: i64,
    size: u64,
}

fn mdfind(args: &[&str]) -> Vec<String> {
    Command::new("mdfind")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

// caches, dependencies, dotfolders — machinery, not the user's files
fn noise(p: &str) -> bool {
    p.contains("/Library/") || p.contains("/node_modules/") || p.contains("/.")
}

fn add_hits(list: Vec<String>, paths: &mut Vec<String>, cap: usize) {
    for p in list {
        if paths.len() >= cap {
            return;
        }
        if !noise(&p) && !paths.contains(&p) {
            paths.push(p);
        }
    }
}

#[tauri::command]
fn search_files(query: String) -> Vec<FileHit> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut paths: Vec<String> = Vec::new();
    // Overshoot the visible 8: the surplus feeds the reranker when needed.
    let cap = 24;

    // Exact phrase in the name beats everything. Stems too, so "resumes"
    // still finds Resume.pdf.
    let stemmed: String = query
        .split_whitespace()
        .map(|w| w.trim_end_matches('s'))
        .collect::<Vec<_>>()
        .join(" ");
    add_hits(mdfind(&["-onlyin", &home, "-name", &query]), &mut paths, cap);
    if stemmed != query {
        add_hits(mdfind(&["-onlyin", &home, "-name", &stemmed]), &mut paths, cap);
    }

    // People don't name things the way they ask for them: "spring break
    // pictures" must still find a folder called "spring break". Each query
    // word searches names on its own; results rank by how many words hit,
    // and a single shared word isn't enough to count.
    let words: Vec<&str> = query.split_whitespace().filter(|w| w.len() > 2).collect();
    if words.len() > 1 {
        let mut scored: Vec<(String, usize)> = Vec::new();
        for w in &words {
            for p in mdfind(&["-onlyin", &home, "-name", w.trim_end_matches('s')]) {
                if noise(&p) {
                    continue;
                }
                match scored.iter_mut().find(|(sp, _)| sp == &p) {
                    Some((_, n)) => *n += 1,
                    None => scored.push((p, 1)),
                }
            }
        }
        scored.retain(|(_, n)| *n >= 2);
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        add_hits(scored.into_iter().map(|(p, _)| p).collect(), &mut paths, cap);
    }

    // Content matches fill whatever room is left — documents only, since
    // "contains the word" is meaningless for code, caches, and binaries.
    add_hits(
        mdfind(&["-onlyin", &home, &format!("{stemmed} kind:document")]),
        &mut paths,
        cap,
    );
    paths
        .into_iter()
        .map(|p| {
            let md = std::fs::metadata(&p).ok();
            FileHit {
                name: Path::new(&p)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.clone()),
                mtime: md
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
                size: md.map(|m| m.len()).unwrap_or(0),
                path: p,
            }
        })
        .collect()
}

// Too many hits? One model call judges which candidates ARE the thing
// asked for (by name, folder, type) vs merely mention it. Paths only —
// Spotlight's index already did the content reading, and extracting text
// from files at search time would blow the latency budget.
#[tauri::command]
async fn rerank_files(query: String, files: Vec<FileHit>) -> Result<Vec<usize>, String> {
    let listing = files
        .iter()
        .enumerate()
        .map(|(i, f)| format!("{i}: {}", f.path))
        .collect::<Vec<_>>()
        .join("\n");
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "matches": { "type": "array", "items": { "type": "integer" } } },
        "required": ["matches"]
    });
    let system = "You filter file-search results on the user's own computer. From the numbered candidate paths, return the indices of files that genuinely ARE what the user asked for, best match first, at most 8. Judge by file name, folder, and file type. Exclude files that merely mention the topic. Respond with JSON only.";
    let user = format!("Searched for: {query}\n\nCandidates:\n{listing}");
    let content = ollama_json(system, &user, schema).await?;
    let v: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    Ok(v["matches"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_u64().map(|n| n as usize))
                .filter(|n| *n < files.len())
                .collect()
        })
        .unwrap_or_default())
}

// --- File organization (M2): plan -> confirm -> apply, every move logged in
// SQLite and reversible with "undo". Moving only, never deleting — the undo
// log is the trust feature that makes the bar safe to say yes to.

const CATEGORIES: &[(&str, &[&str])] = &[
    ("Images", &["png", "jpg", "jpeg", "gif", "heic", "webp", "svg", "tiff", "bmp"]),
    ("Videos", &["mp4", "mov", "avi", "mkv", "webm"]),
    ("Music", &["mp3", "wav", "m4a", "flac", "aac", "ogg"]),
    ("Documents", &["pdf", "doc", "docx", "txt", "rtf", "md", "pages", "xls", "xlsx", "numbers", "ppt", "pptx", "key", "csv", "epub"]),
    ("Archives", &["zip", "rar", "7z", "tar", "gz"]),
    ("Installers", &["dmg", "pkg"]),
    ("Code", &["js", "ts", "tsx", "py", "rs", "go", "java", "c", "cpp", "h", "html", "css", "json", "sh"]),
];

fn category_of(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    for (cat, exts) in CATEGORIES {
        if exts.contains(&ext.as_str()) {
            return cat;
        }
    }
    "Other"
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct PlannedMove {
    from: String,
    to: String,
}

#[derive(serde::Serialize)]
struct OrganizePlan {
    summary: String,
    moves: Vec<PlannedMove>,
}

#[tauri::command]
fn plan_organize(folder: String) -> Result<OrganizePlan, String> {
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let hint = folder.to_lowercase();
    let name = if hint.contains("download") {
        "Downloads"
    } else if hint.contains("desktop") {
        "Desktop"
    } else if hint.contains("document") {
        "Documents"
    } else {
        return Err("I can organize Downloads, Desktop, or Documents for now.".into());
    };
    let dir = Path::new(&home).join(name);
    let mut moves = Vec::new();
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // loose files only — folders stay where the user put them
        if fname.starts_with('.') || !path.is_file() {
            continue;
        }
        let cat = category_of(&path);
        match counts.iter_mut().find(|(c, _)| *c == cat) {
            Some((_, n)) => *n += 1,
            None => counts.push((cat, 1)),
        }
        moves.push(PlannedMove {
            from: path.to_string_lossy().into_owned(),
            to: dir.join(cat).join(fname).to_string_lossy().into_owned(),
        });
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    let breakdown = counts
        .iter()
        .map(|(c, n)| format!("{c} {n}"))
        .collect::<Vec<_>>()
        .join(" · ");
    Ok(OrganizePlan {
        summary: format!("{name}: {} loose files → {breakdown}", moves.len()),
        moves,
    })
}

#[tauri::command]
fn apply_organize(app: AppHandle, moves: Vec<PlannedMove>) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let batch: i64 = conn
        .query_row("SELECT COALESCE(MAX(batch), 0) + 1 FROM file_ops", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let mut done = 0usize;
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for m in &moves {
        let from = Path::new(&m.from);
        if !from.is_file() {
            continue; // changed since the plan was shown — skip, never guess
        }
        let mut to = std::path::PathBuf::from(&m.to);
        let parent = to.parent().map(|p| p.to_path_buf()).ok_or("bad destination")?;
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        // never overwrite: a name collision gets " 2", " 3", …
        if to.exists() {
            let stem = to.file_stem().and_then(|x| x.to_str()).unwrap_or("file").to_string();
            let ext = to
                .extension()
                .and_then(|x| x.to_str())
                .map(|e| format!(".{e}"))
                .unwrap_or_default();
            let mut n = 2;
            while to.exists() {
                to = parent.join(format!("{stem} {n}{ext}"));
                n += 1;
            }
        }
        std::fs::rename(from, &to).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO file_ops (batch, src, dst) VALUES (?1, ?2, ?3)",
            rusqlite::params![batch, m.from, to.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;
        if !dirs.contains(&parent) {
            dirs.push(parent);
        }
        done += 1;
    }
    if done == 0 {
        return Ok("Nothing to move.".into());
    }
    Ok(format!(
        "Moved {done} files into {} folders. Type undo to reverse.",
        dirs.len()
    ))
}

#[tauri::command]
fn undo_last(app: AppHandle) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let batch: Option<i64> = conn
        .query_row("SELECT MAX(batch) FROM file_ops", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let Some(batch) = batch else {
        return Ok("Nothing to undo.".into());
    };
    let rows: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT src, dst FROM file_ops WHERE batch = ?1 ORDER BY id DESC")
            .map_err(|e| e.to_string())?;
        let r = stmt
            .query_map([batch], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        r
    };
    let mut back = 0usize;
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for (src, dst) in &rows {
        let (s, d) = (Path::new(src), Path::new(dst));
        if d.is_file() && !s.exists() && std::fs::rename(d, s).is_ok() {
            back += 1;
            if let Some(p) = d.parent() {
                if !dirs.contains(&p.to_path_buf()) {
                    dirs.push(p.to_path_buf());
                }
            }
        }
    }
    conn.execute("DELETE FROM file_ops WHERE batch = ?1", [batch])
        .map_err(|e| e.to_string())?;
    // category folders the undo emptied get cleaned up; occupied ones stay
    for dir in dirs {
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(format!("Put {back} files back."))
}

// --- Troubleshooting (M2, first domain: disk space). Real diagnostics,
// plain-language report, read-only — freeing space is a change, and changes
// wait for the confirmation layer (M3).

#[tauri::command]
fn diagnose_disk() -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let gb = |kb: f64| kb * 1024.0 / 1e9;
    let run = |cmd: &str, args: &[&str]| {
        Command::new(cmd)
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };

    let mut out = String::new();
    if let Some(line) = run("df", &["-k", &home]).lines().nth(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() > 3 {
            let (total, used, free) = (
                f[1].parse::<f64>().unwrap_or(0.0),
                f[2].parse::<f64>().unwrap_or(0.0),
                f[3].parse::<f64>().unwrap_or(0.0),
            );
            out.push_str(&format!(
                "Disk: {:.0} GB used of {:.0} GB — {:.0} GB free.\n",
                gb(used),
                gb(total),
                gb(free)
            ));
        }
    }

    // The usual suspects. Trash is privacy-gated until the app has Full
    // Disk Access — report no access rather than a silent zero.
    let mut hotspots = Vec::new();
    for (label, rel) in [("Trash", ".Trash"), ("Downloads", "Downloads"), ("Caches", "Library/Caches")] {
        let du = run("du", &["-sk", &format!("{home}/{rel}")]);
        match du.split_whitespace().next().and_then(|k| k.parse::<f64>().ok()) {
            Some(kb) if gb(kb) >= 0.1 => hotspots.push(format!("{label} {:.1} GB", gb(kb))),
            Some(_) => {}
            None => hotspots.push(format!("{label} (no access)")),
        }
    }
    if !hotspots.is_empty() {
        out.push_str(&hotspots.join(" · "));
        out.push('\n');
    }

    // Biggest items, straight from Spotlight's index — instant, and it
    // sees inside app bundles the way a user thinks of them (one item).
    let mut big: Vec<(String, f64)> = run(
        "mdfind",
        &["-onlyin", &home, "kMDItemFSSize > 1073741824", "-attr", "kMDItemFSSize"],
    )
    .lines()
    .filter_map(|l| {
        let (path, attr) = l.split_once("   kMDItemFSSize = ")?;
        let bytes: f64 = attr.trim().parse().ok()?;
        // an item inside a .app belongs to the app, not the list
        if path.rfind(".app/").is_some() {
            return None;
        }
        Some((path.to_string(), bytes / 1e9))
    })
    .collect();
    big.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    big.truncate(5);
    if !big.is_empty() {
        out.push_str("Biggest:\n");
        for (path, g) in big {
            let short = path.replacen(&home, "~", 1);
            out.push_str(&format!("  {g:.1} GB  {short}\n"));
        }
    }
    out.push_str("I only looked — freeing space comes later, with your say-so each time.");
    Ok(out)
}

// --- Agent memory (SQLite, per spec) — first table: learned web destinations.

fn mem_db(app: &AppHandle) -> Result<rusqlite::Connection, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let conn = rusqlite::Connection::open(dir.join("memory.db")).map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS file_ops (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            batch INTEGER NOT NULL,
            src TEXT NOT NULL,
            dst TEXT NOT NULL,
            ts TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
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
            rerank_files,
            diagnose_disk,
            plan_organize,
            apply_organize,
            undo_last,
            open_url,
            resolve_web,
            recall_web,
            remember_web,
            forget_web
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
