use std::path::Path;
use std::process::Command;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

#[derive(serde::Serialize, Clone)]
struct AppEntry {
    name: String,
    path: String,
}

#[cfg(target_os = "macos")]
fn scan_dir(dir: &Path, out: &mut Vec<AppEntry>, depth: u8) {
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "app") {
            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(AppEntry { name: name.to_string(), path: path.to_string_lossy().into_owned() });
            }
        } else if depth > 0 && path.is_dir() {
            scan_dir(&path, out, depth - 1);
        }
    }
}

#[tauri::command]
fn list_apps() -> Vec<AppEntry> {
    #[cfg(target_os = "macos")]
    {
        let mut dirs = vec!["/Applications".to_string(), "/System/Applications".to_string()];
        if let Ok(h) = std::env::var("HOME") { dirs.push(format!("{h}/Applications")); }
        let mut apps = Vec::new();
        for dir in &dirs { scan_dir(Path::new(dir), &mut apps, 1); }
        apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        apps.dedup_by(|a, b| a.path == b.path);
        apps
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut apps = Vec::new();
        let mut dirs = vec!["/usr/share/applications".to_string(), "/usr/local/share/applications".to_string()];
        if let Ok(h) = std::env::var("HOME") { dirs.push(format!("{h}/.local/share/applications")); }
        for dir in &dirs {
            let Ok(rd) = std::fs::read_dir(dir) else { continue; };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "desktop") {
                    let Ok(text) = std::fs::read_to_string(&p) else { continue; };
                    let mut name: Option<String> = None;
                    let mut hidden = false;
                    for line in text.lines() {
                        if line.starts_with("Name=") && name.is_none() { name = Some(line[5..].to_string()); }
                        if line == "NoDisplay=true" || line == "Hidden=true" { hidden = true; }
                    }
                    if let Some(n) = name { if !hidden { apps.push(AppEntry { name: n, path: p.to_string_lossy().into_owned() }); } }
                }
            }
        }
        apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        apps.dedup_by(|a, b| a.name == b.name);
        apps
    }
}

#[tauri::command]
fn launch_app(app: AppHandle, path: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    Command::new("open").arg(&path).spawn().map_err(|e| e.to_string())?;
    #[cfg(not(target_os = "macos"))]
    {
        let stem = std::path::Path::new(&path).file_stem()
            .and_then(|s| s.to_str()).unwrap_or(&path).to_string();
        Command::new("gtk-launch").arg(&stem).spawn()
            .or_else(|_| Command::new("xdg-open").arg(&path).spawn())
            .map_err(|e| e.to_string())?;
    }
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
- troubleshoot: the user reports a computer problem to diagnose — disk full, wifi/internet not working, computer running slow, sound/audio not working, or a specific file that won't open. Set \"query\" to the problem area: disk, wifi, slow, audio — or for a file, the word file plus the file's name (\"file report.pdf\").\n\
- empty_trash: the user wants to empty the trash/bin for good.\n\
- history: the user asks what the agent has done or changed recently.\n\
- file_trash: the user wants to delete a specific file or folder, or move it to the trash. Set \"query\" to words identifying the file.\n\
- settings: the user wants to change the sound volume or screen brightness (\"turn it down\", \"volume 30\", \"dim the screen\", \"mute\"). Set \"query\" to: volume or brightness, then a 0-100 number or up/down/max/mute/unmute.\n\
- unknown: anything else — including changing other settings or installing software. Set \"reply\" to one short sentence stating you can't do that yet.\n\
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
            "intent": { "type": "string", "enum": ["app_launch", "web_open", "web_search", "file_search", "file_organize", "troubleshoot", "settings", "empty_trash", "file_trash", "history", "process_kill", "process_list", "port_lookup", "process_priority", "unknown"] },
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
    // how the file earned its place: 4 exact name, 3 stemmed name,
    // 2 partial name, 1 content-only. Sorting must never let a weaker
    // match outrank a stronger one, whatever the qualifier says.
    rank: u8,
}

#[cfg(target_os = "macos")]
fn mdfind(args: &[&str]) -> Vec<String> {
    Command::new("mdfind").args(args).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn plat_name(dir: &str, pat: &str) -> Vec<String> { mdfind(&["-onlyin", dir, "-name", pat]) }
#[cfg(not(target_os = "macos"))]
fn plat_name(dir: &str, pat: &str) -> Vec<String> {
    Command::new("find")
        .args([dir, "-iname", &format!("*{}*", pat), "-not", "-path", "*/.*", "-not", "-path", "*/node_modules/*"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}
#[cfg(target_os = "macos")]
fn plat_content(dir: &str, q: &str, kind: &str) -> Vec<String> {
    mdfind(&["-onlyin", dir, &format!("{} kind:{}", q, kind)])
}
#[cfg(not(target_os = "macos"))]
fn plat_content(dir: &str, q: &str, _kind: &str) -> Vec<String> {
    Command::new("grep")
        .args(["-rl", "--include=*.txt", "--include=*.md", "--include=*.doc", "--include=*.docx", "-m", "1", q, dir])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}
#[cfg(target_os = "macos")]
fn plat_biggest(dir: &str) -> Vec<(String, f64)> {
    Command::new("mdfind")
        .args(["-onlyin", dir, "kMDItemFSSize > 1073741824", "-attr", "kMDItemFSSize"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| {
            let (path, attr) = l.split_once("   kMDItemFSSize = ")?;
            let bytes: f64 = attr.trim().parse().ok()?;
            if path.rfind(".app/").is_some() { return None; }
            Some((path.to_string(), bytes / 1e9))
        }).collect())
        .unwrap_or_default()
}
#[cfg(not(target_os = "macos"))]
fn plat_biggest(dir: &str) -> Vec<(String, f64)> {
    Command::new("find")
        .args([dir, "-type", "f", "-size", "+1G", "-not", "-path", "*/.*"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|p| {
            let gb = std::fs::metadata(p).ok()?.len() as f64 / 1e9;
            Some((p.to_string(), gb))
        }).collect())
        .unwrap_or_default()
}

// caches, dependencies, dotfolders — machinery, not the user's files
fn noise(p: &str) -> bool {
    p.contains("/Library/")
        || p.contains("/node_modules/")
        || p.contains("/venv/")
        || p.contains("/site-packages/")
        || p.contains("/.")
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
fn search_files(query: String, kind: Option<String>, since: Option<i64>, until: Option<i64>, order: Option<String>) -> Vec<FileHit> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut paths: Vec<String> = Vec::new();
    let no_terms = query.trim().is_empty();
    // Overshoot the visible 8: the surplus feeds the reranker when needed.
    // A kind/date-only search ("recent pictures") has no name to anchor on,
    // so it casts wide and lets the metadata sort pick the top.
    let cap = if no_terms { 3000 } else { 24 };

    // Exact phrase in the name beats everything. Stems too, so "resumes"
    // still finds Resume.pdf.
    let stemmed: String = query
        .split_whitespace()
        .map(|w| w.trim_end_matches('s'))
        .collect::<Vec<_>>()
        .join(" ");
    let mut tiers = [0usize; 3]; // paths.len() after exact / stemmed / partial
    if !no_terms {
        add_hits(plat_name(&home, &query), &mut paths, cap);
        tiers[0] = paths.len();
        if stemmed != query {
            add_hits(plat_name(&home, &stemmed), &mut paths, cap);
        }
        // A typed extension is a hint, not a requirement: "eva.jpeg" must
        // still find eva.jpg. Strip known extensions and search the bare
        // name in the same tier.
        let bare: String = query
            .split_whitespace()
            .map(|w| match w.rsplit_once('.') {
                Some((base, ext))
                    if !base.is_empty()
                        && CATEGORIES
                            .iter()
                            .any(|(_, exts)| exts.contains(&ext.to_lowercase().as_str())) =>
                {
                    base
                }
                _ => w,
            })
            .collect::<Vec<_>>()
            .join(" ");
        if bare != query && bare != stemmed {
            add_hits(plat_name(&home, &bare), &mut paths, cap);
        }
        tiers[1] = paths.len();
    }

    // People don't name things the way they ask for them: "spring break
    // pictures" must still find a folder called "spring break". Each query
    // word searches names on its own; results rank by how many words hit,
    // and a single shared word isn't enough to count.
    let words: Vec<&str> = query.split_whitespace().filter(|w| w.len() > 2).collect();
    if words.len() > 1 {
        let mut scored: Vec<(String, usize)> = Vec::new();
        for w in &words {
            for p in plat_name(&home, w.trim_end_matches('s')) {
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
    tiers[2] = paths.len();

    // Content matches are a FALLBACK, not filler: a file that merely
    // mentions the words is never relevant next to files named for them,
    // so it only appears when nothing matched by name at all. A requested
    // kind scopes the match; otherwise documents only, since "contains
    // the word" is meaningless for code, caches, and binaries.
    if paths.is_empty() {
        let k = kind.as_deref().unwrap_or("document");
        add_hits(
            plat_content(&home, &stemmed, k),
            &mut paths,
            cap,
        );
    }
    let mut hits: Vec<FileHit> = paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
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
                rank: match i {
                    i if i < tiers[0] => 4,
                    i if i < tiers[1] => 3,
                    i if i < tiers[2] => 2,
                    _ => 1,
                },
                path: p,
            }
        })
        .filter(|h| since.is_none_or(|t| h.mtime >= t) && until.is_none_or(|t| h.mtime < t))
        .collect();
    // No name terms means no relevance order exists — the requested order
    // (newest by default) decides which of the wide net survive the trim.
    if no_terms {
        match order.as_deref() {
            Some("old") => hits.sort_by(|a, b| a.mtime.cmp(&b.mtime)),
            Some("big") => hits.sort_by(|a, b| b.size.cmp(&a.size)),
            Some("small") => hits.sort_by(|a, b| a.size.cmp(&b.size)),
            _ => hits.sort_by(|a, b| b.mtime.cmp(&a.mtime)),
        }
        hits.truncate(24);
    }
    hits
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
    let _ = conn.execute(
        "INSERT INTO history (action, detail) VALUES ('organize', ?1)",
        [format!("{done} files into {} folders", dirs.len())],
    );
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
        // journaled trashing: ask Finder to move the item home again
        if let Some(name) = dst.strip_prefix("trash:") {
            let Some(parent) = Path::new(src).parent().map(|p| p.to_string_lossy().into_owned()) else {
                continue;
            };
            let out = osa(&format!(
                "tell application \"Finder\" to move (first item of trash whose name is \"{}\") to (POSIX file \"{}\" as alias)",
                name.replace('"', "\\\""),
                parent.replace('"', "\\\"")
            ));
            if !out.to_lowercase().contains("error") {
                back += 1;
            }
            continue;
        }
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
    if back > 0 {
        let _ = conn.execute(
            "INSERT INTO history (action, detail) VALUES ('undo', ?1)",
            [format!("{back} files put back")],
        );
    }
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

    let mut big: Vec<(String, f64)> = plat_biggest(&home);
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

fn sh(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            s
        })
        .unwrap_or_default()
}

// Walks the network stack layer by layer and names the first one that's
// broken — that's the whole diagnosis a human expert would do.
#[tauri::command]
fn diagnose_wifi() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let hw = sh("networksetup", &["-listallhardwareports"]);
        let dev = hw
            .split("Hardware Port: Wi-Fi")
            .nth(1)
            .and_then(|s| s.lines().find_map(|l| l.trim().strip_prefix("Device: ")))
            .unwrap_or("en0")
            .to_string();
        let ns = sh("networksetup", &["-getairportnetwork", &dev]);
        let mut ssid = ns.trim().strip_prefix("Current Wi-Fi Network: ").map(str::to_string);
        if ssid.is_none() {
            ssid = sh("ipconfig", &["getsummary", &dev])
                .lines()
                .find_map(|l| l.trim().strip_prefix("SSID : ").map(str::to_string));
        }
        let ip = sh("ipconfig", &["getifaddr", &dev]).trim().to_string();
        let gw = sh("route", &["-n", "get", "default"])
            .lines()
            .find_map(|l| l.trim().strip_prefix("gateway: ").map(str::to_string))
            .unwrap_or_default();
        let ping_ok = |host: &str| sh("ping", &["-c", "1", "-t", "3", host]).contains(" 0.0% packet loss");
        let gw_ok = !gw.is_empty() && ping_ok(&gw);
        let net_ok = ping_ok("1.1.1.1");
        let dns_ok = sh("dscacheutil", &["-q", "host", "-a", "name", "apple.com"]).contains("ip_address");
        let mut out = String::new();
        match &ssid {
            Some(name) if name == "<redacted>" => out.push_str("Wi-Fi: connected (macOS hides the network name from apps).\n"),
            Some(name) => out.push_str(&format!("Wi-Fi: connected to {name}.\n")),
            None if !ip.is_empty() => out.push_str("Wi-Fi: not detected, but you have a network address — wired or shared connection.\n"),
            None => out.push_str("Wi-Fi: not connected to any network.\n"),
        }
        out.push_str(&format!(
            "Address from router: {} · Router: {} · Internet: {} · DNS: {}\n",
            if ip.is_empty() { "none" } else { "yes" },
            if gw_ok { "reachable" } else { "unreachable" },
            if net_ok { "reachable" } else { "unreachable" },
            if dns_ok { "working" } else { "failing" }
        ));
        out.push_str(if ssid.is_none() && ip.is_empty() {
            "→ Join a Wi-Fi network from the menu bar."
        } else if ip.is_empty() || !gw_ok {
            "→ Your Mac can't talk to the router. Rejoin the network; if that fails, restart the router."
        } else if !net_ok {
            "→ Router is fine but the internet beyond it is down — modem or provider. Restart the modem; if it persists, it's your ISP."
        } else if !dns_ok {
            "→ Internet works but name lookups fail. Rejoining the network usually clears this."
        } else {
            "→ Network looks healthy end to end. If a site is failing, the problem is that site."
        });
        return Ok(out);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let ping_ok = |host: &str| sh("ping", &["-c", "1", "-W", "3", host]).contains(" 0% packet loss");
        let nm = sh("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"]);
        let wifi_line = nm.lines().find(|l| l.contains(":wifi:"));
        let connected = wifi_line.map(|l| l.contains(":connected")).unwrap_or(false);
        let ssid = wifi_line.and_then(|l| l.split(':').nth(3)).map(str::to_string);
        let ip = sh("ip", &["-4", "-o", "addr", "show"]).lines()
            .find(|l| !l.contains("lo ")).and_then(|l| l.split_whitespace().nth(3))
            .map(|s| s.split('/').next().unwrap_or(s).to_string()).unwrap_or_default();
        let gw = sh("ip", &["route", "show", "default"]).split_whitespace().nth(2)
            .unwrap_or("").to_string();
        let gw_ok = !gw.is_empty() && ping_ok(&gw);
        let net_ok = ping_ok("1.1.1.1");
        let dns_ok = !sh("getent", &["hosts", "archlinux.org"]).trim().is_empty();
        let mut out = String::new();
        if connected {
            out.push_str(&format!("Wi-Fi: connected to {}\n", ssid.as_deref().unwrap_or("(unknown)")));
        } else if !ip.is_empty() {
            out.push_str("Wi-Fi: not detected, but you have a network address — wired or shared.\n");
        } else {
            out.push_str("Wi-Fi: not connected to any network.\n");
        }
        out.push_str(&format!(
            "Address: {} · Router: {} · Internet: {} · DNS: {}\n",
            if ip.is_empty() { "none" } else { "yes" },
            if gw_ok { "reachable" } else { "unreachable" },
            if net_ok { "reachable" } else { "unreachable" },
            if dns_ok { "working" } else { "failing" }
        ));
        out.push_str(if !connected && ip.is_empty() {
            "→ Join a Wi-Fi network from your network manager."
        } else if !gw_ok {
            "→ Can't reach the router. Rejoin the network or restart it."
        } else if !net_ok {
            "→ Router is fine but internet is down — restart your modem or contact ISP."
        } else if !dns_ok {
            "→ Internet works but name lookups fail. Try restarting NetworkManager."
        } else {
            "→ Network looks healthy."
        });
        return Ok(out);
    }
}

#[tauri::command]
fn diagnose_slow() -> Result<String, String> {
    // biggest CPU users right now
    let ps = sh("ps", &["-Areo", "pcpu,comm"]);
    let mut procs: Vec<(f32, String)> = ps
        .lines()
        .skip(1)
        .filter_map(|l| {
            let t = l.trim();
            let (cpu, comm) = t.split_once(' ')?;
            let name = comm.trim().rsplit('/').next().unwrap_or(comm).to_string();
            Some((cpu.trim().parse::<f32>().ok()?, name))
        })
        .collect();
    procs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    procs.truncate(3);

    #[cfg(target_os = "macos")]
    let cores = sh("sysctl", &["-n", "hw.ncpu"]).trim().parse::<f32>().unwrap_or(1.0);
    #[cfg(not(target_os = "macos"))]
    let cores = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default()
        .lines().filter(|l| l.starts_with("processor")).count() as f32;

    #[cfg(target_os = "macos")]
    let load = sh("sysctl", &["-n", "vm.loadavg"])
        .split_whitespace().nth(1).and_then(|x| x.parse::<f32>().ok()).unwrap_or(0.0);
    #[cfg(not(target_os = "macos"))]
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default()
        .split_whitespace().next().and_then(|x| x.parse::<f32>().ok()).unwrap_or(0.0);

    #[cfg(target_os = "macos")]
    let swap_mb = sh("sysctl", &["-n", "vm.swapusage"])
        .split("used = ").nth(1).and_then(|x| x.split('M').next())
        .and_then(|x| x.trim().parse::<f32>().ok()).unwrap_or(0.0);
    #[cfg(not(target_os = "macos"))]
    let swap_mb = {
        let mi = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let val = |key: &str| mi.lines().find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1)).and_then(|x| x.parse::<f32>().ok()).unwrap_or(0.0);
        (val("SwapTotal:") - val("SwapFree:")) / 1024.0
    };
    let free_gb = {
        let home = std::env::var("HOME").unwrap_or_default();
        sh("df", &["-k", &home])
            .lines()
            .nth(1)
            .and_then(|l| l.split_whitespace().nth(3))
            .and_then(|x| x.parse::<f64>().ok())
            .map(|kb| kb * 1024.0 / 1e9)
            .unwrap_or(0.0)
    };

    let mut out = String::new();
    out.push_str("Busiest right now:\n");
    for (cpu, name) in &procs {
        out.push_str(&format!("  {cpu:.0}% CPU  {name}\n"));
    }
    out.push_str(&format!(
        "Load {load:.1} on {cores:.0} cores · swap {:.1} GB · disk {free_gb:.0} GB free\n",
        swap_mb / 1024.0
    ));
    let hog = procs.first().filter(|(c, _)| *c > 80.0);
    out.push_str(&if let Some((cpu, name)) = hog {
        format!("→ {name} is eating {cpu:.0}% CPU — quitting it should help immediately.")
    } else if load > cores * 1.5 {
        "→ The whole machine is overloaded — close apps you're not using, or restart.".into()
    } else if swap_mb / 1024.0 > 4.0 {
        "→ Memory is the bottleneck (heavy swapping). Closing browser tabs and unused apps helps most.".into()
    } else if free_gb < 15.0 {
        "→ The disk is nearly full, which slows everything. Try \"disk space\".".into()
    } else {
        "→ Nothing obviously wrong right now. If it still feels slow, a restart clears most of it.".into()
    });
    Ok(out)
}

// Why won't this file open? Walk the reasons in order of likelihood and
// name the first one that applies — each with the fix in plain language.
#[tauri::command]
fn diagnose_file(path: String) -> Result<String, String> {
    let p = Path::new(&path);
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
    let home = std::env::var("HOME").unwrap_or_default();
    let short = path.replacen(&home, "~", 1);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    let md = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Ok(format!(
                "{short}\n→ macOS won't even let me look at it — the folder it's in is off-limits to this account. If it's on an external drive or another user's folder, that's why."
            ))
        }
        Err(e) => return Ok(format!("{short}\n→ Can't read it at all: {e}.")),
    };

    let verdict = if name.ends_with(".icloud") || (ext == "icloud") {
        "This is an iCloud placeholder — the real file isn't on this Mac yet. Open the folder in Finder and click the cloud icon to download it, then it'll open.".to_string()
    } else if ["crdownload", "download", "part", "partial"].contains(&ext.as_str()) {
        "This is an unfinished download, not the real file. Go back to the browser and let the download complete (or restart it).".to_string()
    } else if md.is_file() && md.len() == 0 {
        "The file is completely empty (0 bytes) — the download or save that created it never finished. Re-download or re-export it; there's nothing inside to open.".to_string()
    } else if std::fs::File::open(p).err().is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied) {
        "macOS says this account doesn't have permission to read it. Right-click → Get Info, and check Sharing & Permissions at the bottom — you need at least Read.".to_string()
    } else if sh("ls", &["-ldO", &path]).contains("uchg") {
        "The file is locked. Right-click → Get Info and untick Locked, then it'll open normally.".to_string()
    } else if ext == "app" && !sh("xattr", &["-p", "com.apple.quarantine", &path]).trim().is_empty() {
        "This app is quarantined by Gatekeeper (it was downloaded from the internet). Right-click it → Open, then confirm — that only needs doing once.".to_string()
    } else {
        let kind = sh("mdls", &["-name", "kMDItemKind", "-raw", &path]);
        let kind = kind.trim();
        if kind.is_empty() || kind == "(null)" || kind.eq_ignore_ascii_case("data") {
            format!("No app on this Mac claims this file type ({}). Whoever sent it can say what app made it — or try right-click → Open With to test a likely one.",
                if ext.is_empty() { "no extension".to_string() } else { format!(".{ext}") })
        } else {
            let mb = md.len() as f64 / 1e6;
            format!("The file itself looks healthy — readable, {mb:.1} MB, recognized as {kind}. The problem is likely the app that opens it: quit and reopen that app, or try right-click → Open With to use a different one.")
        }
    };
    Ok(format!("{short}\n→ {verdict}"))
}

#[tauri::command]
fn diagnose_audio() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let vol = sh("osascript", &["-e", "get volume settings"]);
        let get = |key: &str| {
            vol.split(&format!("{key}:"))
                .nth(1)
                .and_then(|x| x.split(&[',', '\n'][..]).next())
                .map(|x| x.trim().to_string())
                .unwrap_or_default()
        };
        let volume = get("output volume").parse::<i32>().unwrap_or(-1);
        let muted = get("output muted") == "true";
        let sp = sh("system_profiler", &["SPAudioDataType", "-detailLevel", "mini"]);
        let output = sp.split("Default Output Device: Yes").next()
            .and_then(|before| before.lines().rev()
                .find(|l| l.ends_with(':') && !l.trim().is_empty() && !l.contains("Devices"))
                .map(|l| l.trim().trim_end_matches(':').to_string()))
            .unwrap_or_else(|| "unknown".into());
        let daemon_ok = !sh("pgrep", &["-x", "coreaudiod"]).trim().is_empty();
        let mut out = format!(
            "Output device: {output} · volume {volume}% · {}\n",
            if muted { "MUTED" } else { "not muted" }
        );
        out.push_str(&if muted {
            "â Sound is muted â press F10 or raise the volume.".into()
        } else if volume == 0 {
            "â Volume is at zero â turn it up.".into()
        } else if !daemon_ok {
            "â The sound system (coreaudiod) isn't running â restarting the Mac fixes this.".into()
        } else if output.to_lowercase().contains("display") || output.to_lowercase().contains("tv") {
            format!("â Sound is going to \"{output}\" (a screen), not speakers â switch the output device in Control Center.")
        } else {
            format!("â Audio setup looks fine ({output}, {volume}%). If one app is silent, check its own volume; if everything is, try switching output devices in Control Center.")
        });
        return Ok(out);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let pactl_vol = sh("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]);
        let volume: i32 = pactl_vol.split('%').next()
            .and_then(|s| s.split_whitespace().last())
            .and_then(|x| x.parse().ok()).unwrap_or(-1);
        let muted = sh("pactl", &["get-sink-mute", "@DEFAULT_SINK@"]).contains("yes");
        let sink = sh("pactl", &["info"]).lines()
            .find(|l| l.starts_with("Default Sink:"))
            .and_then(|l| l.split(':').nth(1)).map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".into());
        let daemon_ok = !sh("pgrep", &["-x", "pipewire"]).trim().is_empty()
            || !sh("pgrep", &["-x", "pulseaudio"]).trim().is_empty();
        let vol_display = if volume < 0 { 0 } else { volume };
        let mut out = format!(
            "Output sink: {sink} · volume {vol_display}% · {}\n",
            if muted { "MUTED" } else { "not muted" }
        );
        out.push_str(if muted {
            "â Sound is muted â unmute with your volume keys or: pactl set-sink-mute @DEFAULT_SINK@ 0"
        } else if volume == 0 {
            "â Volume is at zero â turn it up."
        } else if !daemon_ok {
            "â Audio daemon not running â try: systemctl --user start pipewire"
        } else {
            "â Audio looks healthy. If an app is silent, check its own mixer channel."
        });
        return Ok(out);
    }
}

// --- Settings (M3): volume and brightness. These run without a confirm
// step deliberately: the command itself names the exact change ("sound
// 15"), it applies instantly, and the same command reverses it — the
// confirmation layer is for operations the agent plans on your behalf.

#[cfg(target_os = "macos")]
fn osa(script: &str) -> String { sh("osascript", &["-e", script]) }
#[cfg(not(target_os = "macos"))]
fn osa(_script: &str) -> String { String::new() }

fn level_from(action: &str, current: impl Fn() -> i32) -> Result<i32, String> {
    Ok(match action {
        "up" => current() + 10,
        "down" => current() - 10,
        "max" => 100,
        "min" => 0,
        n => n
            .parse::<i32>()
            .map_err(|_| format!("didn't catch a level in {n:?} — try a number 0-100"))?,
    }
    .clamp(0, 100))
}

#[tauri::command]
fn set_volume(app: AppHandle, action: String) -> Result<String, String> {
    let log = |detail: &str| {
        if let Ok(conn) = mem_db(&app) {
            let _ = conn.execute(
                "INSERT INTO history (action, detail) VALUES ('volume', ?1)",
                [detail],
            );
        }
    };
    #[cfg(target_os = "macos")]
    {
        match action.as_str() {
            "mute" => { osa("set volume output muted true"); log("muted"); return Ok("Muted.".into()); }
            "unmute" => { osa("set volume output muted false"); log("unmuted"); return Ok("Unmuted.".into()); }
            _ => {}
        }
        let target = level_from(&action, || {
            osa("output volume of (get volume settings)").trim().parse().unwrap_or(50)
        })?;
        osa(&format!("set volume output volume {target}"));
        if target > 0 { osa("set volume output muted false"); }
        log(&format!("{target}%"));
        return Ok(format!("Volume {target}%."));
    }
    #[cfg(not(target_os = "macos"))]
    {
        match action.as_str() {
            "mute" => {
                let _ = Command::new("pactl").args(["set-sink-mute", "@DEFAULT_SINK@", "1"]).status();
                log("muted"); return Ok("Muted.".into());
            }
            "unmute" => {
                let _ = Command::new("pactl").args(["set-sink-mute", "@DEFAULT_SINK@", "0"]).status();
                log("unmuted"); return Ok("Unmuted.".into());
            }
            _ => {}
        }
        let target = level_from(&action, || {
            let v = sh("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]);
            v.split('%').next().and_then(|s| s.split_whitespace().last())
                .and_then(|x| x.parse().ok()).unwrap_or(50)
        })?;
        let _ = Command::new("pactl")
            .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{target}%")]).status();
        log(&format!("{target}%"));
        return Ok(format!("Volume {target}%."));
    }
}

// No public API for brightness on macOS — uses private DisplayServices.
// On Linux uses brightnessctl.
#[tauri::command]
fn set_brightness(app: AppHandle, action: String) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let result = unsafe {
            let cg = libc::dlopen(
                c"/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics".as_ptr(),
                libc::RTLD_LAZY,
            );
            let ds = libc::dlopen(
                c"/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices".as_ptr(),
                libc::RTLD_LAZY,
            );
            if cg.is_null() || ds.is_null() {
                return Err("brightness control isn't available on this Mac".into());
            }
            let sym_main = libc::dlsym(cg, c"CGMainDisplayID".as_ptr());
            let sym_get = libc::dlsym(ds, c"DisplayServicesGetBrightness".as_ptr());
            let sym_set = libc::dlsym(ds, c"DisplayServicesSetBrightness".as_ptr());
            if sym_main.is_null() || sym_get.is_null() || sym_set.is_null() {
                return Err("brightness control isn't available on this Mac".into());
            }
            let main_id: extern "C" fn() -> u32 = std::mem::transmute(sym_main);
            let get: extern "C" fn(u32, *mut f32) -> i32 = std::mem::transmute(sym_get);
            let set: extern "C" fn(u32, f32) -> i32 = std::mem::transmute(sym_set);
            let id = main_id();
            let target = level_from(&action, || {
                let mut cur = 0.5f32;
                get(id, &mut cur);
                (cur * 100.0).round() as i32
            })?;
            if set(id, target as f32 / 100.0) != 0 {
                return Err("this display doesn't allow brightness control".into());
            }
            target
        };
        if let Ok(conn) = mem_db(&app) {
            let _ = conn.execute(
                "INSERT INTO history (action, detail) VALUES ('brightness', ?1)",
                [format!("{result}%")],
            );
        }
        return Ok(format!("Brightness {result}%."));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let target = level_from(&action, || {
            let cur = sh("brightnessctl", &["-m", "get"]);
            let max_s = sh("brightnessctl", &["-m", "max"]);
            let c: f32 = cur.trim().parse().unwrap_or(0.0);
            let m: f32 = max_s.trim().parse().unwrap_or(1.0);
            if m > 0.0 { (c / m * 100.0).round() as i32 } else { 50 }
        })?;
        if Command::new("brightnessctl").args(["set", &format!("{target}%")]).status().is_err() {
            return Err("brightnessctl not found -- install it to control brightness".into());
        }
        if let Ok(conn) = mem_db(&app) {
            let _ = conn.execute(
                "INSERT INTO history (action, detail) VALUES ('brightness', ?1)",
                [format!("{target}%")],
            );
        }
        return Ok(format!("Brightness {target}%."));
    }
}

// --- Process management (M6) ─────────────────────────────────────────────────

// These processes are off-limits regardless of what the user asks.
const PROTECTED_PROCS: &[&str] = &[
    "kernel_task", "windowserver", "launchd", "loginwindow",
    "systemuiserver", "dock", "finder", "joveros", "bash", "zsh", "sh",
    "python3", "python", "cargo", "rustc",
];

fn is_protected(name: &str) -> bool {
    let lower = name.to_lowercase();
    PROTECTED_PROCS.iter().any(|p| lower == *p || lower.contains(p))
}

#[tauri::command]
fn kill_process(app: AppHandle, name: String, force: bool) -> Result<String, String> {
    if is_protected(&name) {
        return Err(format!("That's a system process — won't kill \"{name}\"."));
    }
    let out = Command::new("pgrep")
        .args(["-i", "-l", &name])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if out.trim().is_empty() {
        return Ok(format!("No process matching \"{name}\" is running."));
    }
    let mut killed: Vec<String> = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(2, ' ');
        let pid = parts.next().unwrap_or("").trim().to_string();
        let proc_name = parts.next().unwrap_or(name.as_str()).trim().to_string();
        if pid.is_empty() || is_protected(&proc_name) { continue; }
        let sig = if force { "-9" } else { "-15" };
        if Command::new("kill").args([sig, &pid]).status().is_ok() {
            killed.push(proc_name);
        }
    }
    if killed.is_empty() {
        return Ok(format!(
            "Couldn't quit \"{name}\" — it may need force. Try: force kill {name}"
        ));
    }
    if let Ok(conn) = mem_db(&app) {
        let _ = conn.execute(
            "INSERT INTO history (action, detail) VALUES ('kill_process', ?1)",
            [format!("{} (force={})", killed.join(", "), force)],
        );
    }
    let verb = if force { "Force-killed" } else { "Quit" };
    Ok(format!("{verb}: {}.", killed.join(", ")))
}

#[tauri::command]
fn list_processes() -> Result<String, String> {
    let raw = sh("ps", &["-Areo", "pid,pcpu,pmem,comm"]);
    let mut procs: Vec<(f32, f32, String)> = raw
        .lines()
        .skip(1)
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            if cols.len() < 4 { return None; }
            let cpu: f32 = cols[1].parse().ok()?;
            let mem: f32 = cols[2].parse().ok()?;
            let name = cols[3..].join(" ");
            let name = name.rsplit('/').next().unwrap_or(&name).to_string();
            Some((cpu, mem, name))
        })
        .collect();
    procs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    procs.dedup_by(|a, b| a.2 == b.2);
    procs.truncate(8);
    if procs.is_empty() {
        return Ok("Nothing interesting running.".into());
    }
    let mut out = String::from("Top processes:\n");
    for (cpu, mem, name) in &procs {
        out.push_str(&format!("  {cpu:5.1}% CPU  {mem:4.1}% mem  {name}\n"));
    }
    out.push_str("Say \"kill <name>\" to quit one.");
    Ok(out)
}

#[tauri::command]
fn port_lookup(port: u16) -> Result<String, String> {
    let out = sh("lsof", &["-i", &format!(":{port}"), "-n", "-P"]);
    if out.trim().is_empty() || out.lines().count() <= 1 {
        return Ok(format!("Nothing is using port {port}."));
    }
    let mut procs: Vec<String> = Vec::new();
    for line in out.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 2 { continue; }
        let entry = format!("{} (PID {})", cols[0], cols[1]);
        if !procs.contains(&entry) { procs.push(entry); }
    }
    if procs.is_empty() {
        return Ok(format!("Nothing is using port {port}."));
    }
    Ok(format!("Port {port}: {}.", procs.join(", ")))
}

#[tauri::command]
fn set_process_priority(app: AppHandle, name: String, level: String) -> Result<String, String> {
    if is_protected(&name) {
        return Err(format!("Won't reprioritize system process \"{name}\"."));
    }
    let pid_out = Command::new("pgrep")
        .args(["-i", &name])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let pid = pid_out.lines().next().unwrap_or("").trim().to_string();
    if pid.is_empty() {
        return Ok(format!("No process matching \"{name}\" is running."));
    }
    let (nice_val, label) = match level.to_lowercase().trim() {
        "high" | "boost" | "max" => ("-10", "high"),
        "low" | "background" | "idle" => ("10", "low"),
        _ => ("0", "normal"),
    };
    let ok = Command::new("renice")
        .args(["-n", nice_val, &pid])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Err(format!(
            "Couldn't reprioritize \"{name}\" — boosting priority requires admin rights."
        ));
    }
    if let Ok(conn) = mem_db(&app) {
        let _ = conn.execute(
            "INSERT INTO history (action, detail) VALUES ('process_priority', ?1)",
            [format!("{name} → {label}")],
        );
    }
    Ok(format!("Set {name} to {label} priority."))
}

// --- Game Mode (M7) ──────────────────────────────────────────────────────────

// Default background processes killed on "game mode on".
// Apps that waste CPU/RAM while gaming but aren't needed.
// Discord intentionally excluded — it's used for voice chat while gaming.
const DEFAULT_GAME_KILL: &[&str] = &[
    "Slack", "Microsoft Teams", "zoom.us", "Zoom",
    "OneDrive", "Dropbox", "Google Drive",
    "Adobe Creative Cloud", "Creative Cloud",
    "Backblaze", "Backup and Sync",
];

#[tauri::command]
fn game_mode_on(app: AppHandle, profile: Option<String>) -> Result<String, String> {
    let conn = mem_db(&app)?;
    // Already on?
    let already: bool = conn.query_row(
        "SELECT COUNT(*) FROM game_session WHERE active = 1", [],
        |r| r.get::<_, i64>(0),
    ).unwrap_or(0) > 0;
    if already {
        return Ok("Game mode is already on. Say \"game mode off\" to exit.".into());
    }

    // Load kill list — from profile or default.
    let (kill_list, boost_proc): (Vec<String>, Option<String>) =
        if let Some(ref p) = profile {
            let lower = p.to_lowercase();
            conn.query_row(
                "SELECT kill_list, boost_process FROM game_profiles WHERE name = ?1",
                [&lower],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .ok()
            .and_then(|(kj, bp)| serde_json::from_str::<Vec<String>>(&kj).ok().map(|k| (k, bp)))
            .unwrap_or_else(|| {
                (DEFAULT_GAME_KILL.iter().map(|s| s.to_string()).collect(), Some(p.clone()))
            })
        } else {
            (DEFAULT_GAME_KILL.iter().map(|s| s.to_string()).collect(), None)
        };

    // Pause background apps with SIGSTOP — they freeze in place and resume
    // exactly where they left off on game mode off (no relaunch needed).
    // Store {name, pids} so we can SIGCONT the exact same processes later.
    let mut paused: Vec<serde_json::Value> = Vec::new();
    for name in &kill_list {
        let out = Command::new("pgrep")
            .args(["-i", name])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let pids: Vec<String> = out.lines()
            .map(|l| l.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if pids.is_empty() { continue; }
        for pid in &pids {
            let _ = Command::new("kill").args(["-STOP", pid]).status();
        }
        paused.push(serde_json::json!({ "name": name, "pids": pids }));
    }
    let killed: Vec<String> = paused.iter()
        .filter_map(|e| e["name"].as_str().map(str::to_string))
        .collect();

    // Spawn caffeinate to keep the system awake (macOS) / store PID.
    let caff_pid: Option<i64> = {
        #[cfg(target_os = "macos")]
        {
            Command::new("caffeinate").args(["-dims"]).spawn().ok().map(|c| c.id() as i64)
        }
        #[cfg(not(target_os = "macos"))]
        { None }
    };

    // Boost the game process priority if we know which one.
    let mut boosted: Option<String> = None;
    if let Some(ref bp) = boost_proc {
        let pid_out = Command::new("pgrep")
            .args(["-i", bp])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if let Some(pid) = pid_out.lines().next().map(|l| l.trim().to_string()) {
            if !pid.is_empty() {
                let _ = Command::new("renice").args(["-n", "-10", &pid]).status();
                boosted = Some(bp.clone());
            }
        }
    }

    // Persist session with PID list for exact SIGCONT later.
    let paused_json = serde_json::to_string(&paused).unwrap_or_default();
    conn.execute(
        "INSERT INTO game_session (killed_apps, caff_pid) VALUES (?1, ?2)",
        rusqlite::params![paused_json, caff_pid],
    ).map_err(|e| e.to_string())?;

    let profile_name = profile.as_deref().unwrap_or("default");
    let mut out = format!("Game mode ON ({profile_name}).\n");
    if killed.is_empty() {
        out.push_str("Background apps weren't running — nothing to pause.");
    } else {
        out.push_str(&format!("Paused: {}.", killed.join(", ")));
    }
    if let Some(b) = boosted {
        out.push_str(&format!("\nBoosted {b} to high priority."));
    }
    out.push_str("\nSay \"game mode off\" when you're done.");
    Ok(out)
}

#[tauri::command]
fn game_mode_off(app: AppHandle) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let result = conn.query_row(
        "SELECT id, killed_apps, caff_pid FROM game_session WHERE active = 1 ORDER BY id DESC LIMIT 1",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?)),
    );
    let (session_id, killed_json, caff_pid) = match result {
        Ok(r) => r,
        Err(_) => return Ok("Game mode isn't on.".into()),
    };
    // Kill the caffeinate process.
    if let Some(pid) = caff_pid {
        let _ = Command::new("kill").args([&pid.to_string()]).status();
    }

    // Resume paused apps with SIGCONT — they continue exactly where they stopped.
    let entries: Vec<serde_json::Value> = serde_json::from_str(&killed_json).unwrap_or_default();
    let mut resumed: Vec<String> = Vec::new();
    for entry in &entries {
        let name = entry["name"].as_str().unwrap_or("").to_string();
        let pids = entry["pids"].as_array().cloned().unwrap_or_default();
        for pid_val in &pids {
            if let Some(pid) = pid_val.as_str() {
                let _ = Command::new("kill").args(["-CONT", pid]).status();
            }
        }
        if !name.is_empty() { resumed.push(name); }
    }

    // Mark session closed.
    let _ = conn.execute(
        "UPDATE game_session SET active = 0 WHERE id = ?1",
        [session_id],
    );

    let mut out = "Game mode OFF.\n".to_string();
    if !resumed.is_empty() {
        out.push_str(&format!("Resumed: {}.", resumed.join(", ")));
    } else {
        out.push_str("Nothing to resume.");
    }
    Ok(out)
}

// Save a game profile from the currently running apps — smart capture.
// The named game is excluded from the kill list and tagged as the boost target.
#[tauri::command]
fn save_game_profile(app: AppHandle, name: String) -> Result<String, String> {
    let lower = name.trim().to_lowercase();
    let running = running_apps();
    // Build kill list: running apps that aren't the game, the bar, or system procs.
    let kill_list: Vec<String> = running.iter()
        .filter(|a| {
            let al = a.to_lowercase();
            !is_protected(a) && !al.contains(&lower) && al != "joveros" && al != "ai-os"
        })
        .cloned()
        .collect();
    // Boost target: process matching the game name, or "java" for JVM games (Minecraft).
    let boost = running.iter()
        .find(|a| a.to_lowercase().contains(&lower))
        .cloned()
        .or_else(|| {
            // Minecraft / JVM fallback
            if lower.contains("minecraft") {
                running.iter().find(|a| a.to_lowercase().contains("java")).cloned()
            } else {
                None
            }
        });
    let final_kill: Vec<String> = if kill_list.is_empty() {
        DEFAULT_GAME_KILL.iter().map(|s| s.to_string()).collect()
    } else {
        kill_list
    };
    let conn = mem_db(&app)?;
    let kill_json = serde_json::to_string(&final_kill).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO game_profiles (name, kill_list, boost_process) VALUES (?1, ?2, ?3)
         ON CONFLICT(name) DO UPDATE SET kill_list = ?2, boost_process = ?3",
        rusqlite::params![lower, kill_json, boost.as_deref()],
    ).map_err(|e| e.to_string())?;
    let mut out = format!("Saved profile \"{name}\".\n");
    out.push_str(&format!("Will kill: {}.\n", final_kill.join(", ")));
    if let Some(ref b) = boost {
        out.push_str(&format!("Will boost: {b}."));
    }
    Ok(out)
}

#[tauri::command]
fn list_game_profiles(app: AppHandle) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let mut stmt = conn
        .prepare("SELECT name, kill_list, boost_process FROM game_profiles ORDER BY name")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    if rows.is_empty() {
        return Ok(
            "No saved profiles.\nDefault mode kills: ".to_string()
                + &DEFAULT_GAME_KILL.join(", ")
                + ".",
        );
    }
    let mut out = "Game profiles:\n".to_string();
    for (name, kill_json, boost) in &rows {
        let kills: Vec<String> = serde_json::from_str(kill_json).unwrap_or_default();
        out.push_str(&format!("  {name}: kills {}", kills.join(", ")));
        if let Some(b) = boost {
            out.push_str(&format!(" · boosts {b}"));
        }
        out.push('\n');
    }
    Ok(out.trim_end().to_string())
}

// --- Media Controls (M8) ─────────────────────────────────────────────────────

// Returns the first media app that is open (Spotify checked before Music).
// Does NOT require it to be playing — used for shuffle/skip when paused.
#[cfg(target_os = "macos")]
fn open_media_app() -> Option<String> {
    for app in &["Spotify", "Music"] {
        let running = !Command::new("pgrep")
            .args(["-xi", app])
            .output()
            .map(|o| o.stdout.is_empty())
            .unwrap_or(true);
        if running {
            return Some(app.to_string());
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn playing_media_app() -> Option<String> {
    open_media_app().filter(|app| {
        osa(&format!("tell application \"{}\" to player state as string", app))
            .trim()
            .to_lowercase()
            == "playing"
    })
}

#[tauri::command]
fn media_control(action: String, app: Option<String>) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        // Normalize requested app name to the real macOS app name.
        let requested = app.as_deref().map(|a| match a.to_lowercase().replace(' ', "").as_str() {
            "spotify"               => "Spotify",
            "music" | "applemusic" => "Music",
            _                      => "Spotify", // unknown → try Spotify
        }.to_string());

        // Launch the requested app if it isn't already running.
        if let Some(ref name) = requested {
            let running = !Command::new("pgrep")
                .args(["-xi", name])
                .output()
                .map(|o| o.stdout.is_empty())
                .unwrap_or(true);
            if !running {
                let _ = Command::new("open").args(["-a", name]).status();
                // Give the app time to initialize before sending AppleScript.
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }

        let app = requested
            .or_else(open_media_app)
            .ok_or_else(|| "No media app is open. Say 'play spotify' to launch one.".to_string())?;
        let script = match action.as_str() {
            "playpause" | "play" | "pause" => {
                format!("tell application \"{}\" to playpause", app)
            }
            "next" => format!("tell application \"{}\" to next track", app),
            "previous" => format!("tell application \"{}\" to previous track", app),
            _ => return Err(format!("Unknown action: {action}")),
        };
        osa(&script);
        let reply = match action.as_str() {
            "next" => format!("Next track. [{app}]"),
            "previous" => format!("Previous track. [{app}]"),
            _ => {
                let state = osa(&format!(
                    "tell application \"{}\" to player state as string",
                    app
                ));
                if state.trim().is_empty() {
                    return Err(format!(
                        "Couldn't control {app} — check System Settings → Privacy & Security → Automation and allow joverOS to control {app}."
                    ));
                }
                if state.trim().to_lowercase() == "playing" {
                    format!("Playing. [{app}]")
                } else {
                    format!("Paused. [{app}]")
                }
            }
        };
        return Ok(reply);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let cmd = match action.as_str() {
            "playpause" | "play" | "pause" => "play-pause",
            "next" => "next",
            "previous" => "previous",
            _ => return Err(format!("Unknown action: {action}")),
        };
        if Command::new("playerctl").arg(cmd).status().map(|s| s.success()).unwrap_or(false) {
            return Ok("Done.".into());
        }
        return Err("playerctl not found — install it to control media.".into());
    }
}

#[tauri::command]
fn media_skip(seconds: i32) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let app = playing_media_app()
            .ok_or_else(|| "Nothing is playing.".to_string())?;
        osa(&format!(
            "tell application \"{}\" to set player position to (player position + {})",
            app, seconds
        ));
        let dir = if seconds >= 0 { "forward" } else { "back" };
        return Ok(format!("Skipped {} {}s. [{app}]", dir, seconds.unsigned_abs()));
    }
    #[cfg(not(target_os = "macos"))]
    {
        // playerctl uses microseconds for seek offset
        let micros = seconds as i64 * 1_000_000;
        let offset = if micros >= 0 {
            format!("+{micros}")
        } else {
            format!("{micros}")
        };
        if Command::new("playerctl").args(["position", &offset]).status().map(|s| s.success()).unwrap_or(false) {
            let dir = if seconds >= 0 { "forward" } else { "back" };
            return Ok(format!("Skipped {} {}s.", dir, seconds.unsigned_abs()));
        }
        return Err("playerctl not found.".into());
    }
}

#[tauri::command]
fn now_playing() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        for app in &["Spotify", "Music"] {
            let running = !Command::new("pgrep")
                .args(["-xi", app])
                .output()
                .map(|o| o.stdout.is_empty())
                .unwrap_or(true);
            if !running { continue; }
            let state = osa(&format!("tell application \"{}\" to player state as string", app));
            if state.trim().to_lowercase() != "playing" { continue; }
            let info = osa(&format!(
                "tell application \"{}\" to get {{name of current track, artist of current track, album of current track}}",
                app
            ));
            let parts: Vec<&str> = info.trim().splitn(3, ", ").collect();
            let title  = parts.first().copied().unwrap_or("Unknown").trim();
            let artist = parts.get(1).copied().unwrap_or("Unknown").trim();
            let album  = parts.get(2).copied().unwrap_or("").trim();
            let pos_raw = osa(&format!("tell application \"{}\" to player position as integer", app));
            let secs: u32 = pos_raw.trim().parse().unwrap_or(0);
            let mut out = format!("{title} \u{2014} {artist}");
            if !album.is_empty() { out.push_str(&format!(" \u{00b7} {album}")); }
            out.push_str(&format!(" ({}:{:02}) [{app}]", secs / 60, secs % 60));
            return Ok(out);
        }
        return Ok("Nothing is playing.".into());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let status = sh("playerctl", &["status"]).trim().to_lowercase();
        if status != "playing" { return Ok("Nothing is playing.".into()); }
        let title  = sh("playerctl", &["metadata", "title"]).trim().to_string();
        let artist = sh("playerctl", &["metadata", "artist"]).trim().to_string();
        let pos_us: u64 = sh("playerctl", &["metadata", "mpris:length"]).trim().parse().unwrap_or(0);
        let secs = pos_us / 1_000_000;
        return Ok(format!("{title} \u{2014} {artist} ({}:{:02})", secs / 60, secs % 60));
    }
}

#[tauri::command]
fn media_shuffle(on: bool) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let app = open_media_app()
            .ok_or_else(|| "No media app is open.".to_string())?;
        let script = match app.as_str() {
            "Spotify" => format!(
                "tell application \"Spotify\" to set shuffling to {}",
                if on { "true" } else { "false" }
            ),
            "Music" => format!(
                "tell application \"Music\" to set shuffle enabled to {}",
                if on { "true" } else { "false" }
            ),
            _ => return Err("Shuffle not supported for this player.".into()),
        };
        osa(&script);
        return Ok(format!("Shuffle {}. [{app}]", if on { "on" } else { "off" }));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let val = if on { "Shuffle" } else { "None" };
        if Command::new("playerctl").args(["shuffle", val]).status().map(|s| s.success()).unwrap_or(false) {
            return Ok(format!("Shuffle {}.", if on { "on" } else { "off" }));
        }
        return Err("playerctl not found.".into());
    }
}

// --- Window Management (M9) ──────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn screen_size() -> (i32, i32) {
    let raw = osa(r#"tell application "Finder" to get bounds of window of desktop"#);
    let parts: Vec<i32> = raw.trim().split(", ")
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if parts.len() >= 4 { (parts[2], parts[3]) } else { (1440, 900) }
}

// Normalize user-supplied app name to the real process name.
fn proc_name(app: &str) -> String {
    match app.to_lowercase().trim() {
        "vscode" | "vs code" | "visual studio code" | "code" => "Code".into(),
        "chrome"                                              => "Google Chrome".into(),
        "word"                                               => "Microsoft Word".into(),
        "excel"                                              => "Microsoft Excel".into(),
        "powerpoint"                                         => "Microsoft PowerPoint".into(),
        other => {
            let mut chars = other.chars();
            chars.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default() + chars.as_str()
        }
    }
}

#[tauri::command]
fn window_manage(action: String, app_name: Option<String>) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let raw_app = app_name.as_deref().unwrap_or("").trim().to_string();
        let proc = proc_name(&raw_app);

        match action.as_str() {
            "focus" => {
                if proc.is_empty() { return Err("Specify an app to focus.".into()); }
                osa(&format!("tell application \"{}\" to activate", proc));
                return Ok(format!("Focused {proc}."));
            }
            "minimize" => {
                if proc.is_empty() { return Err("Specify an app to minimize.".into()); }
                // Direct app scripting — no Accessibility needed for scriptable apps.
                osa(&format!(
                    r#"tell application "{proc}"
                        activate
                        set miniaturized of window 1 to true
                    end tell"#
                ));
                return Ok(format!("Minimized {proc}."));
            }
            "hide" => {
                if proc.is_empty() { return Err("Specify an app to hide.".into()); }
                osa(&format!("tell application \"{}\" to set visible of every window to false", proc));
                return Ok(format!("Hid {proc}."));
            }
            "hide_all" => {
                osa(r#"tell application "Finder" to set visible of every process to false"#);
                return Ok("Hid background windows.".into());
            }
            "show" => {
                if proc.is_empty() { return Err("Specify an app to show.".into()); }
                osa(&format!("tell application \"{}\" to activate", proc));
                return Ok(format!("Showed {proc}."));
            }
            "show_all" => {
                osa(r#"tell application "System Events" to set visible of every process to true"#);
                return Ok("Showed all windows.".into());
            }
            "fullscreen" => {
                if proc.is_empty() { return Err("Specify an app.".into()); }
                // zoomed fills the screen without needing Accessibility.
                osa(&format!(
                    r#"tell application "{proc}"
                        activate
                        set zoomed of window 1 to not zoomed of window 1
                    end tell"#
                ));
                return Ok(format!("Toggled fullscreen for {proc}."));
            }
            snap @ ("snap_left" | "snap_right" | "snap_top" | "snap_bottom" | "center") => {
                if proc.is_empty() { return Err("Specify an app to snap.".into()); }
                let (sw, sh) = screen_size();
                let mb = 25i32;
                let (l, t, r, b) = match snap {
                    "snap_left"   => (0,      mb,      sw / 2, sh),
                    "snap_right"  => (sw / 2, mb,      sw,     sh),
                    "snap_top"    => (0,      mb,      sw,     mb + (sh - mb) / 2),
                    "snap_bottom" => (0,      mb + (sh - mb) / 2, sw, sh),
                    "center" => {
                        let ww = sw * 2 / 3;
                        let wh = (sh - mb) * 2 / 3;
                        let cx = (sw - ww) / 2;
                        let cy = mb + ((sh - mb) - wh) / 2;
                        (cx, cy, cx + ww, cy + wh)
                    }
                    _ => unreachable!()
                };
                // Unminimize first, then snap — bounds can't be set on a minimized window.
                osa(&format!(
                    r#"tell application "{proc}"
                        activate
                        set miniaturized of window 1 to false
                        set bounds of window 1 to {{{l}, {t}, {r}, {b}}}
                    end tell"#
                ));
                let label = snap.replace("snap_", "");
                return Ok(format!("Snapped {proc} to the {label}."));
            }
            _ => return Err(format!("Unknown window action: {action}")),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let app = app_name.as_deref().unwrap_or("").trim().to_string();
        match action.as_str() {
            "focus" => {
                if Command::new("wmctrl").args(["-a", &app]).status().map(|s| s.success()).unwrap_or(false) {
                    return Ok(format!("Focused {app}."));
                }
                return Err("wmctrl not found — install it for window management on Linux.".into());
            }
            _ => return Ok("Install wmctrl and xdotool for full window management on Linux.".into()),
        }
    }
}

// --- Play Track (M8b) — search + play a specific song ───────────────────────

#[tauri::command]
fn setup_spotify(app: AppHandle, client_id: String, client_secret: String) -> Result<String, String> {
    let db = mem_db(&app).map_err(|e| e.to_string())?;
    kv_set(&db, "spotify_client_id", &client_id);
    kv_set(&db, "spotify_client_secret", &client_secret);
    Ok("Spotify credentials saved. Try 'play [song name] on spotify' now.".into())
}

async fn try_play_spotify(client: &reqwest::Client, query: &str, client_id: &str, client_secret: &str) -> Result<String, String> {
    // Client Credentials token (no user login required).
    let token_resp = client
        .post("https://accounts.spotify.com/api/token")
        .basic_auth(client_id, Some(client_secret))
        .form(&[("grant_type", "client_credentials")])
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| e.to_string())?;

    let token = token_resp["access_token"]
        .as_str()
        .ok_or_else(|| "Spotify auth failed — check your client ID and secret.".to_string())?;

    let enc = urlencoding::encode(query);
    let search = client
        .get(format!("https://api.spotify.com/v1/search?q={enc}&type=track&limit=1"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| e.to_string())?;

    let track = &search["tracks"]["items"][0];
    if track.is_null() {
        return Err(format!("No Spotify track found for '{query}'."));
    }

    let uri    = track["uri"].as_str().ok_or("missing track uri")?;
    let name   = track["name"].as_str().unwrap_or(query);
    let artist = track["artists"][0]["name"].as_str().unwrap_or("Unknown");

    // Make sure Spotify is open before sending AppleScript.
    let spotify_running = !Command::new("pgrep").args(["-xi", "Spotify"])
        .output().map(|o| o.stdout.is_empty()).unwrap_or(true);
    if !spotify_running {
        let _ = Command::new("open").args(["-a", "Spotify"]).status();
        std::thread::sleep(std::time::Duration::from_secs(3));
    }

    // AppleScript play track directly — more reliable than URI scheme.
    #[cfg(target_os = "macos")]
    osa(&format!("tell application \"Spotify\" to play track \"{}\"", uri));
    #[cfg(not(target_os = "macos"))]
    { let _ = Command::new("open").arg(uri).status(); }

    Ok(format!("{name} \u{2014} {artist} [Spotify]"))
}

#[tauri::command]
async fn play_track(app: AppHandle, query: String, prefer_app: Option<String>) -> Result<String, String> {
    let prefer = prefer_app.as_deref().map(|s| s.to_lowercase().replace(' ', ""));

    let want_spotify = prefer.as_deref() == Some("spotify");
    let want_music   = matches!(prefer.as_deref(), Some("music") | Some("applemusic"));

    #[cfg(target_os = "macos")]
    let spotify_open = !Command::new("pgrep").args(["-xi", "Spotify"])
        .output().map(|o| o.stdout.is_empty()).unwrap_or(true);
    #[cfg(not(target_os = "macos"))]
    let spotify_open = false;

    #[cfg(target_os = "macos")]
    let music_open = !Command::new("pgrep").args(["-xi", "Music"])
        .output().map(|o| o.stdout.is_empty()).unwrap_or(true);
    #[cfg(not(target_os = "macos"))]
    let music_open = false;

    let use_spotify = want_spotify || (!want_music && spotify_open);
    let use_music   = want_music   || (!want_spotify && !spotify_open && music_open);

    let http = reqwest::Client::new();

    // --- Spotify path ---
    if use_spotify {
        let db = mem_db(&app).map_err(|e| e.to_string())?;
        if let (Some(id), Some(secret)) = (kv_get(&db, "spotify_client_id"), kv_get(&db, "spotify_client_secret")) {
            match try_play_spotify(&http, &query, &id, &secret).await {
                Ok(msg) => return Ok(msg),
                Err(e)  => {
                    // API failed — fall back to in-app search.
                    let enc = urlencoding::encode(&query);
                    let _ = Command::new("open").arg(format!("spotify:search:{enc}")).status();
                    return Ok(format!("Opened Spotify search for '{query}'. ({e})"));
                }
            }
        } else {
            // No credentials — open in-app search, remind user to set up.
            let enc = urlencoding::encode(&query);
            let _ = Command::new("open").arg(format!("spotify:search:{enc}")).status();
            return Ok(format!(
                "Opened Spotify search for '{query}'. \
                 For auto-play, say: setup spotify CLIENT_ID CLIENT_SECRET"
            ));
        }
    }

    // --- Apple Music path (local library) ---
    #[cfg(target_os = "macos")]
    if use_music {
        let safe = query.replace('"', "'");
        let result = osa(&format!(
            r#"tell application "Music"
                set res to search playlist "Library" for "{safe}"
                if (count of res) > 0 then
                    play (item 1 of res)
                    return (name of item 1 of res) & " — " & (artist of item 1 of res)
                end if
                return ""
            end tell"#
        ));
        if !result.trim().is_empty() {
            return Ok(format!("{} [Apple Music]", result.trim()));
        }
        // Not in library — fall through to YouTube.
    }

    // --- YouTube fallback ---
    let enc = urlencoding::encode(&query);
    let url = format!("https://www.youtube.com/results?search_query={enc}");
    let _ = Command::new("open").arg(&url).status();
    Ok(format!("Opening YouTube search for '{query}'."))
}

// The bar keeps its OWN trash. macOS lets apps script files INTO the
// system Trash but blocks getting them OUT without Full Disk Access —
// and TCC grants pin to the code signature, which changes every dev
// rebuild. Instead of fighting that, trashed files move to a holding
// area the bar owns: restore is a plain rename, guaranteed, zero
// permissions. "empty trash" purges this AND the system Trash.
fn bar_trash_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("trash");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

#[tauri::command]
fn trash_file(app: AppHandle, path: String) -> Result<String, String> {
    let name = Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or("bad path")?;
    let dir = bar_trash_dir(&app)?;
    let mut hold = dir.join(&name);
    if hold.exists() {
        let stem = hold.file_stem().and_then(|x| x.to_str()).unwrap_or("file").to_string();
        let ext = hold
            .extension()
            .and_then(|x| x.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let mut n = 2;
        while hold.exists() {
            hold = dir.join(format!("{stem} {n}{ext}"));
            n += 1;
        }
    }
    std::fs::rename(&path, &hold).map_err(|e| format!("couldn't move {name}: {e}"))?;
    let conn = mem_db(&app)?;
    let batch: i64 = conn
        .query_row("SELECT COALESCE(MAX(batch), 0) + 1 FROM file_ops", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO file_ops (batch, src, dst) VALUES (?1, ?2, ?3)",
        rusqlite::params![batch, path, hold.to_string_lossy()],
    )
    .map_err(|e| e.to_string())?;
    let _ = conn.execute(
        "INSERT INTO history (action, detail) VALUES ('trash_file', ?1)",
        [&name],
    );
    Ok(format!("Trashed {name} — “put {name} back” or undo restores it; “empty trash” deletes it for good."))
}

// macOS protects the Trash: Finder will script items IN but refuses to
// move them OUT (error -5000) unless the caller has Full Disk Access.
// So restoring tries the direct route first (instant with FDA), then
// Finder, and if both are blocked says exactly which switch to flip.
// Ok(true) = home again (or already was); Ok(false) = blocked.
fn restore_row(conn: &rusqlite::Connection, id: i64, src: &str, dst: &str) -> bool {
    let restored = if let Some(name) = dst.strip_prefix("trash:") {
        // legacy row: item went to the system Trash, which macOS guards
        Path::new(src).exists() || {
            let out = osa(&format!(
                "tell application \"Finder\" to move (first item of trash whose name is \"{}\") to (POSIX file \"{}\" as alias)",
                name.replace('"', "\\\""),
                Path::new(src).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default().replace('"', "\\\"")
            ));
            !out.to_lowercase().contains("error")
        }
    } else if Path::new(src).exists() {
        // stale iff the held copy is gone; a conflict keeps the row
        !Path::new(dst).exists()
    } else {
        std::fs::rename(dst, src).is_ok()
    };
    if restored {
        let _ = conn.execute("DELETE FROM file_ops WHERE id = ?1", [id]);
        let _ = conn.execute(
            "INSERT INTO history (action, detail) VALUES ('restore', ?1)",
            [Path::new(src).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()],
        );
    }
    restored
}

const SYS_TRASH_HINT: &str = "it's in the system Trash, which macOS guards — use Finder's File → Put Back (⌘⌫) there.";

// Put one named thing back: "put eva back", "restore eva.jpg".
#[tauri::command]
fn trash_rows(conn: &rusqlite::Connection, dir: &Path) -> Result<Vec<(i64, String, String)>, String> {
    let mut stmt = conn
        .prepare("SELECT id, src, dst FROM file_ops WHERE dst LIKE 'trash:%' OR dst LIKE ?1 ORDER BY id DESC")
        .map_err(|e| e.to_string())?;
    let r = stmt
        .query_map([format!("{}/%", dir.to_string_lossy())], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    Ok(r)
}

#[tauri::command]
fn restore_named(app: AppHandle, what: String) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let rows = trash_rows(&conn, &bar_trash_dir(&app)?)?;
    let q = what.trim().to_lowercase();
    let stem = q.rsplit_once('.').map(|(b, _)| b.to_string()).unwrap_or_else(|| q.clone());
    let hit = rows.iter().find(|(_, src, _)| {
        let name = Path::new(src)
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        name.contains(&q) || name.contains(&stem)
    });
    let Some((id, src, dst)) = hit else {
        // most common reason there's nothing to restore: it's already home
        let home = std::env::var("HOME").unwrap_or_default();
        if let Some(found) = plat_name(&home, &stem)
            .into_iter()
            .find(|p| !noise(p))
        {
            return Ok(format!(
                "Nothing to restore — “{what}” is already where it belongs: {}",
                found.replacen(&home, "~", 1)
            ));
        }
        #[cfg(target_os = "macos")]
        {
            let names = osa("tell application \"Finder\" to get name of items in trash").to_lowercase();
            return Ok(if names.contains(&stem) {
                format!("“{what}” wasn't trashed by me — {SYS_TRASH_HINT}")
            } else {
                format!("I don't have “{what}” in my trash, and nothing like it is in the system Trash.")
            });
        }
        #[cfg(not(target_os = "macos"))]
        return Ok(format!("I don't have “{what}” in my trash."));
        #[allow(unreachable_code)]
        return Ok(String::new());
    };
    let name = Path::new(src)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if restore_row(&conn, *id, src, dst) {
        Ok(format!(
            "Put {name} back — {}",
            src.replacen(&std::env::var("HOME").unwrap_or_default(), "~", 1)
        ))
    } else {
        Ok(format!("Couldn't restore {name} — {SYS_TRASH_HINT}"))
    }
}

// "restore trash" restores everything the bar itself trashed that is
// still in the Trash — any batch, not just the last. Items trashed in
// Finder can't be bulk-restored by anyone but Finder: their original
// locations live in Finder's private records, with no API. Say so
// instead of silently doing nothing.
#[tauri::command]
fn restore_trash(app: AppHandle) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let rows = trash_rows(&conn, &bar_trash_dir(&app)?)?;
    let mut back = 0usize;
    let mut blocked = 0usize;
    for (id, src, dst) in &rows {
        if restore_row(&conn, *id, src, dst) {
            back += 1;
        } else {
            blocked += 1;
        }
    }
    let sys = sys_trash_count().unwrap_or(0);
    let mut out = match back {
        0 => "Nothing of mine to restore.".to_string(),
        b => format!("Put {b} file{} back where {} lived.", if b == 1 { "" } else { "s" }, if b == 1 { "it" } else { "they" }),
    };
    if blocked > 0 {
        out.push_str(&format!(" {blocked} couldn't be restored — {SYS_TRASH_HINT}"));
    } else if sys > 0 {
        out.push_str(&format!(
            " ({sys} item{} in the system Trash {} put there in Finder — File → Put Back ⌘⌫ restores {}.)",
            if sys == 1 { "" } else { "s" },
            if sys == 1 { "was" } else { "were" },
            if sys == 1 { "it" } else { "them" }
        ));
    }
    Ok(out)
}

// Hard delete — no holding bay, no undo. Reachable ONLY through
// explicit phrasing ("permanently delete", "delete X forever"): the
// router model is never allowed to pick this intent, so an ambiguous
// "delete X" always lands in the reversible path.
#[tauri::command]
fn delete_file(app: AppHandle, path: String) -> Result<String, String> {
    let name = Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or("bad path")?;
    let p = Path::new(&path);
    if p.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
    } else {
        std::fs::remove_file(p).map_err(|e| e.to_string())?;
    }
    if let Ok(conn) = mem_db(&app) {
        let _ = conn.execute(
            "INSERT INTO history (action, detail) VALUES ('delete_file', ?1)",
            [&name],
        );
    }
    Ok(format!("Deleted {name} — gone for good."))
}

// --- Empty trash (M3): the first destructive action, so it sets the
// pattern — count first, confirm in the bar, go through Finder (macOS
// asks its own one-time consent for that), record it in history.
// Deleting is exactly what the undo log can NOT reverse, and the
// confirm text says so.

fn sys_trash_count() -> Result<i64, String> {
    let out = osa("tell application \"Finder\" to count items in trash");
    out.trim()
        .parse()
        .map_err(|_| format!("couldn't check the Trash — macOS may have blocked Finder access ({})", out.trim()))
}

#[tauri::command]
fn trash_count(app: AppHandle) -> Result<i64, String> {
    let ours = std::fs::read_dir(bar_trash_dir(&app)?)
        .map(|d| d.flatten().count() as i64)
        .unwrap_or(0);
    Ok(ours + sys_trash_count().unwrap_or(0))
}

#[tauri::command]
fn empty_trash(app: AppHandle) -> Result<String, String> {
    let dir = bar_trash_dir(&app)?;
    let held: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let sys = sys_trash_count().unwrap_or(0);
    if held.is_empty() && sys == 0 {
        return Ok("Trash is already empty.".into());
    }
    for p in &held {
        let _ = if p.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        };
    }
    if sys > 0 {
        let out = osa("tell application \"Finder\" to empty trash");
        if out.to_lowercase().contains("error") {
            return Err(format!("Emptied mine ({}), but Finder wouldn't empty the system Trash: {}", held.len(), out.trim()));
        }
    }
    let total = held.len() as i64 + sys;
    if let Ok(conn) = mem_db(&app) {
        let _ = conn.execute(
            "DELETE FROM file_ops WHERE dst LIKE ?1",
            [format!("{}/%", dir.to_string_lossy())],
        );
        let _ = conn.execute(
            "INSERT INTO history (action, detail) VALUES ('empty_trash', ?1)",
            [format!("{total} items")],
        );
    }
    Ok(format!("Emptied the Trash — {total} item{} gone for good.", if total == 1 { "" } else { "s" }))
}

// --- Scoped permissions (M3). Only REVERSIBLE actions can be granted:
// trash (holding bay, restorable) and organize (undo log). Emptying and
// permanent deletion always ask — no grant exists that lets the agent
// silently do something it can't take back. Grants are changed only by
// exact phrases in the frontend; the model has no path to this code.

const GRANTABLE: &[&str] = &["trash", "organize"];

#[tauri::command]
fn set_grant(app: AppHandle, scope: String, allow: bool) -> Result<String, String> {
    if !GRANTABLE.contains(&scope.as_str()) {
        return Ok(match scope.as_str() {
            "empty_trash" => "Emptying the Trash is forever, so I'll always ask — that one isn't grantable.".into(),
            "delete" => "Permanent deletion always asks. Always.".into(),
            _ => format!("Nothing called “{scope}” to grant. Grantable: trash, organize."),
        });
    }
    let conn = mem_db(&app)?;
    if allow {
        conn.execute("INSERT OR IGNORE INTO grants (scope) VALUES (?1)", [&scope])
            .map_err(|e| e.to_string())?;
        let _ = conn.execute(
            "INSERT INTO history (action, detail) VALUES ('grant', ?1)",
            [&scope],
        );
        Ok(format!("Okay — I'll {scope} without asking from now on. “always ask before {scope}” reverts this."))
    } else {
        conn.execute("DELETE FROM grants WHERE scope = ?1", [&scope])
            .map_err(|e| e.to_string())?;
        let _ = conn.execute(
            "INSERT INTO history (action, detail) VALUES ('revoke', ?1)",
            [&scope],
        );
        Ok(format!("Back to asking before every {scope}."))
    }
}

#[tauri::command]
fn check_grant(app: AppHandle, scope: String) -> bool {
    mem_db(&app)
        .ok()
        .and_then(|c| {
            c.query_row("SELECT 1 FROM grants WHERE scope = ?1", [&scope], |_| Ok(true))
                .ok()
        })
        .unwrap_or(false)
}

#[tauri::command]
fn list_grants(app: AppHandle) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let mut stmt = conn
        .prepare("SELECT scope FROM grants ORDER BY scope")
        .map_err(|e| e.to_string())?;
    let granted: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    let mut out = String::new();
    if granted.is_empty() {
        out.push_str("I ask before every change (the default).\n");
    } else {
        out.push_str(&format!("Without asking: {}.\n", granted.join(", ")));
    }
    let ask: Vec<&str> = GRANTABLE.iter().filter(|s| !granted.contains(&s.to_string())).copied().collect();
    if !ask.is_empty() {
        out.push_str(&format!("Ask first: {}.\n", ask.join(", ")));
    }
    out.push_str("Always ask, not grantable: empty trash, permanent delete.\n");
    out.push_str("“always allow trash” grants · “always ask before trash” reverts.");
    Ok(out)
}

// --- Workflows (M4): record the current session, replay it by name.
// Recording = apps open right now (System Events) + tabs from Chrome/Safari
// (AppleScript; Firefox has no tab API so gets plugin treatment later).
// Replaying = launch all apps then reopen tab URLs in the right browser.

#[cfg(target_os = "macos")]
fn chrome_tabs() -> Vec<String> {
    let script = r#"tell application "Google Chrome"
set out to {}
repeat with w in windows
    repeat with t in tabs of w
        set end of out to (URL of t)
    end repeat
end repeat
set AppleScript's text item delimiters to "\n"
out as text
end tell"#;
    let o = std::process::Command::new("osascript").args(["-e", script]).output();
    match o {
        Ok(r) if r.status.success() => String::from_utf8_lossy(&r.stdout)
            .lines().map(str::trim).filter(|u| u.starts_with("http")).map(str::to_string).collect(),
        _ => vec![],
    }
}
#[cfg(not(target_os = "macos"))]
fn chrome_tabs() -> Vec<String> { vec![] }

#[cfg(target_os = "macos")]
fn safari_tabs() -> Vec<String> {
    let script = r#"tell application "Safari"
set out to {}
repeat with w in windows
    try
        repeat with t in tabs of w
            try
                set end of out to (URL of t)
            end try
        end repeat
    end try
end repeat
set AppleScript's text item delimiters to "\n"
out as text
end tell"#;
    let o = std::process::Command::new("osascript").args(["-e", script]).output();
    match o {
        Ok(r) if r.status.success() => String::from_utf8_lossy(&r.stdout)
            .lines().map(str::trim).filter(|u| u.starts_with("http")).map(str::to_string).collect(),
        _ => vec![],
    }
}
#[cfg(not(target_os = "macos"))]
fn safari_tabs() -> Vec<String> { vec![] }

#[cfg(target_os = "macos")]
fn running_apps() -> Vec<String> {
    let out = osa(
        "tell application \"System Events\" to get name of (processes where background only is false)",
    );
    if out.to_lowercase().contains("error") { return Vec::new(); }
    out.trim().split(", ").map(str::trim)
        .filter(|a| !a.is_empty() && !["Finder", "joverOS", "joveros", "ai-os"].contains(a))
        .map(str::to_string).collect()
}
#[cfg(not(target_os = "macos"))]
fn running_apps() -> Vec<String> {
    let wm = sh("wmctrl", &["-l", "-x"]);
    if !wm.trim().is_empty() {
        let mut apps: Vec<String> = wm.lines().filter_map(|l| {
            let parts: Vec<&str> = l.splitn(5, char::is_whitespace).collect();
            parts.get(3).map(|a| a.split('.').last().unwrap_or(a).to_string())
        }).filter(|a| !a.is_empty()).collect();
        apps.dedup();
        return apps;
    }
    Command::new("ps").args(["-e", "-o", "comm="]).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines()
            .map(|l| l.rsplit('/').next().unwrap_or(l).to_string())
            .filter(|a| !a.is_empty() && !["bash","sh","zsh","ps","grep"].contains(&a.as_str()))
            .collect())
        .unwrap_or_default()
}

#[tauri::command]
fn workflow_exists(app: AppHandle, name: String) -> bool {
    mem_db(&app)
        .ok()
        .and_then(|c| {
            c.query_row("SELECT 1 FROM workflows WHERE name = ?1", [name.trim().to_lowercase()], |_| Ok(true))
                .ok()
        })
        .unwrap_or(false)
}

#[tauri::command]
fn save_workflow(app: AppHandle, name: String) -> Result<String, String> {
    let apps = running_apps();
    if apps.is_empty() {
        return Err("Couldn't see your open apps — macOS may have declined the System Events consent. Check System Settings → Privacy & Security → Automation.".into());
    }
    let lower_apps: Vec<String> = apps.iter().map(|a| a.to_lowercase()).collect();
    let mut tab_map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    if lower_apps.iter().any(|a| a.contains("chrome")) {
        let tabs = chrome_tabs();
        if !tabs.is_empty() { tab_map.insert("chrome".into(), tabs); }
    }
    if lower_apps.iter().any(|a| a == "safari") {
        let tabs = safari_tabs();
        if !tabs.is_empty() { tab_map.insert("safari".into(), tabs); }
    }
    let key = name.trim().to_lowercase();
    let conn = mem_db(&app)?;
    let tabs_json = serde_json::to_string(&tab_map).unwrap_or_default();
    conn.execute(
        "INSERT INTO workflows (name, apps, tabs) VALUES (?1, ?2, ?3)
         ON CONFLICT(name) DO UPDATE SET apps = ?2, tabs = ?3, created = datetime('now')",
        rusqlite::params![key, serde_json::to_string(&apps).unwrap_or_default(), tabs_json],
    )
    .map_err(|e| e.to_string())?;
    let tab_count: usize = tab_map.values().map(|v| v.len()).sum();
    let tab_note = if tab_count > 0 {
        let parts: Vec<String> = tab_map.iter()
            .map(|(b, urls)| format!("{} {} tab{}", urls.len(), b, if urls.len() == 1 { "" } else { "s" }))
            .collect();
        format!(" + {}", parts.join(", "))
    } else {
        String::new()
    };
    let summary = format!("{}{tab_note}", apps.join(", "));
    let _ = conn.execute(
        "INSERT INTO history (action, detail) VALUES ('workflow_save', ?1)",
        [format!("{key}: {summary}")],
    );
    Ok(format!("Saved workflow {key}: {summary}. Type \u{201c}workflow {key}\u{201d} to bring it all back."))
}

#[tauri::command]
fn run_workflow(app: AppHandle, name: String) -> Result<String, String> {
    let key = name.trim().to_lowercase();
    let conn = mem_db(&app)?;
    let (apps_json, tabs_json): (String, Option<String>) = conn
        .query_row("SELECT apps, tabs FROM workflows WHERE name = ?1", [&key], |r| Ok((r.get(0)?, r.get(1)?)))
        .or_else(|_| {
            conn.query_row(
                "SELECT apps, tabs FROM workflows WHERE name LIKE ?1 ORDER BY created DESC",
                [format!("%{key}%")],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .map_err(|_| format!("No workflow called \u{201c}{key}\u{201d}. \u{201c}workflows\u{201d} lists what\u{2019}s saved."))?;
    let apps: Vec<String> = serde_json::from_str(&apps_json).unwrap_or_default();
    // Skip apps already running — don't yank focus or duplicate windows.
    let already: Vec<String> = running_apps().into_iter().map(|a| a.to_lowercase()).collect();
    for a in &apps {
        if !already.contains(&a.to_lowercase()) {
            let _ = Command::new("open").args(["-a", a]).spawn();
        }
    }
    // Restore browser tabs: open each URL in the browser that had it open.
    let tab_map: std::collections::HashMap<String, Vec<String>> = tabs_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();
    for (browser, urls) in &tab_map {
        let app_name = if browser == "chrome" { "Google Chrome" } else { "Safari" };
        for url in urls {
            let _ = Command::new("open").args(["-a", app_name, url]).spawn();
        }
    }
    let _ = conn.execute(
        "INSERT INTO history (action, detail) VALUES ('workflow_run', ?1)",
        [&key],
    );
    hide_bar(app);
    let tab_count: usize = tab_map.values().map(|v| v.len()).sum();
    let tab_note = if tab_count > 0 { format!(" + {} tab{}", tab_count, if tab_count == 1 { "" } else { "s" }) } else { String::new() };
    Ok(format!("Workflow {key}: opening {}{tab_note}.", apps.join(", ")))
}

#[tauri::command]
fn list_workflows(app: AppHandle) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let mut stmt = conn
        .prepare("SELECT name, apps FROM workflows ORDER BY name")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    if rows.is_empty() {
        return Ok("No workflows yet. Open your apps the way you like, then “save as workflow 1”.".into());
    }
    Ok(rows
        .iter()
        .map(|(n, a)| {
            let apps: Vec<String> = serde_json::from_str(a).unwrap_or_default();
            format!("{n} — {}", apps.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

#[tauri::command]
fn delete_workflow(app: AppHandle, name: String) -> Result<String, String> {
    let key = name.trim().to_lowercase();
    let conn = mem_db(&app)?;
    let n = conn
        .execute("DELETE FROM workflows WHERE name = ?1", [&key])
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(format!("No workflow called “{key}”."));
    }
    let _ = conn.execute(
        "INSERT INTO history (action, detail) VALUES ('workflow_delete', ?1)",
        [&key],
    );
    Ok(format!("Forgot workflow {key}."))
}

// The undo log's face: everything the agent ever changed, tersely.
#[tauri::command]
fn get_history(app: AppHandle) -> Result<String, String> {
    let conn = mem_db(&app)?;
    let mut stmt = conn
        .prepare("SELECT strftime('%m-%d %H:%M', ts, 'localtime'), action, detail FROM history ORDER BY id DESC LIMIT 10")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    if rows.is_empty() {
        return Ok("Nothing yet — I haven't changed anything on this Mac.".into());
    }
    let lines: Vec<String> = rows
        .iter()
        .map(|(ts, action, detail)| {
            let label = match action.as_str() {
                "trash_file" => format!("Trashed {detail}"),
                "restore" => format!("Put back {detail}"),
                "restore_trash" => format!("Restored {detail}"),
                "empty_trash" => format!("Emptied Trash ({detail})"),
                "delete_file" => format!("Deleted {detail} forever"),
                "organize" => format!("Organized {detail}"),
                "undo" => format!("Undid — {detail}"),
                "volume" => format!("Volume → {detail}"),
                "brightness" => format!("Brightness → {detail}"),
                _ => format!("{action} {detail}"),
            };
            format!("{ts}  {label}")
        })
        .collect();
    Ok(lines.join("\n"))
}

// --- Agent memory (SQLite, per spec) — first table: learned web destinations.

fn mem_db(app: &AppHandle) -> Result<rusqlite::Connection, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let conn = rusqlite::Connection::open(dir.join("memory.db")).map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS workflows (
            name TEXT PRIMARY KEY,
            apps TEXT NOT NULL,
            tabs TEXT,
            created TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    // Migrate existing DB: add tabs column if it doesn't exist yet.
    let _ = conn.execute("ALTER TABLE workflows ADD COLUMN tabs TEXT", []);
    conn.execute(
        "CREATE TABLE IF NOT EXISTS grants (
            scope TEXT PRIMARY KEY,
            granted_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            action TEXT NOT NULL,
            detail TEXT NOT NULL,
            ts TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
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
    conn.execute(
        "CREATE TABLE IF NOT EXISTS game_profiles (
            name TEXT PRIMARY KEY,
            kill_list TEXT NOT NULL,
            boost_process TEXT,
            created TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS game_session (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            killed_apps TEXT NOT NULL,
            caff_pid INTEGER,
            active INTEGER NOT NULL DEFAULT 1,
            started TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS kv (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn kv_get(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0)).ok()
}

fn kv_set(conn: &rusqlite::Connection, key: &str, value: &str) {
    let _ = conn.execute(
        "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
        [key, value],
    );
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
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        if let Some(path) = &browser_path { cmd.arg("-a").arg(path); }
        cmd.arg(&target).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(path) = &browser_path {
            Command::new(path).arg(&target).spawn().map_err(|e| e.to_string())?;
        } else {
            Command::new("xdg-open").arg(&target).spawn().map_err(|e| e.to_string())?;
        }
    }
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
            diagnose_wifi,
            diagnose_slow,
            diagnose_audio,
            diagnose_file,
            trash_file,
            delete_file,
            get_history,
            set_grant,
            check_grant,
            list_grants,
            workflow_exists,
            save_workflow,
            run_workflow,
            list_workflows,
            delete_workflow,
            restore_trash,
            restore_named,
            trash_count,
            empty_trash,
            set_volume,
            set_brightness,
            plan_organize,
            apply_organize,
            undo_last,
            open_url,
            resolve_web,
            recall_web,
            remember_web,
            forget_web,
            kill_process,
            list_processes,
            port_lookup,
            set_process_priority,
            game_mode_on,
            game_mode_off,
            save_game_profile,
            list_game_profiles,
            media_control,
            media_skip,
            now_playing,
            media_shuffle,
            play_track,
            setup_spotify,
            window_manage
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
