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
            "intent": { "type": "string", "enum": ["app_launch", "web_open", "web_search", "file_search", "file_organize", "troubleshoot", "settings", "empty_trash", "file_trash", "history", "unknown"] },
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

fn mdfind(args: &[&str]) -> Vec<String> {
    Command::new("mdfind")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
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
        add_hits(mdfind(&["-onlyin", &home, "-name", &query]), &mut paths, cap);
        tiers[0] = paths.len();
        if stemmed != query {
            add_hits(mdfind(&["-onlyin", &home, "-name", &stemmed]), &mut paths, cap);
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
            add_hits(mdfind(&["-onlyin", &home, "-name", &bare]), &mut paths, cap);
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
    tiers[2] = paths.len();

    // Content matches are a FALLBACK, not filler: a file that merely
    // mentions the words is never relevant next to files named for them,
    // so it only appears when nothing matched by name at all. A requested
    // kind scopes the match; otherwise documents only, since "contains
    // the word" is meaningless for code, caches, and binaries.
    if paths.is_empty() {
        let k = kind.as_deref().unwrap_or("document");
        add_hits(
            mdfind(&["-onlyin", &home, &format!("{stemmed} kind:{k}").trim().to_string()]),
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
    let hw = sh("networksetup", &["-listallhardwareports"]);
    let dev = hw
        .split("Hardware Port: Wi-Fi")
        .nth(1)
        .and_then(|s| s.lines().find_map(|l| l.trim().strip_prefix("Device: ")))
        .unwrap_or("en0")
        .to_string();
    // networksetup lies on modern macOS ("not associated" while connected),
    // and getsummary redacts the name without Location permission — so
    // detect *connection* reliably and treat the name as a nice-to-have.
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
        Some(name) if name == "<redacted>" => {
            out.push_str("Wi-Fi: connected (macOS hides the network name from apps).\n")
        }
        Some(name) => out.push_str(&format!("Wi-Fi: connected to {name}.\n")),
        None if !ip.is_empty() => {
            out.push_str("Wi-Fi: not detected, but you have a network address — wired or shared connection.\n")
        }
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
    Ok(out)
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

    let cores = sh("sysctl", &["-n", "hw.ncpu"]).trim().parse::<f32>().unwrap_or(1.0);
    let load = sh("sysctl", &["-n", "vm.loadavg"])
        .split_whitespace()
        .nth(1)
        .and_then(|x| x.parse::<f32>().ok())
        .unwrap_or(0.0);
    let swap_mb = sh("sysctl", &["-n", "vm.swapusage"])
        .split("used = ")
        .nth(1)
        .and_then(|x| x.split('M').next())
        .and_then(|x| x.trim().parse::<f32>().ok())
        .unwrap_or(0.0);
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
    let output = sp
        .split("Default Output Device: Yes")
        .next()
        .and_then(|before| {
            before
                .lines()
                .rev()
                .find(|l| l.ends_with(':') && !l.trim().is_empty() && !l.contains("Devices"))
                .map(|l| l.trim().trim_end_matches(':').to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    let daemon_ok = !sh("pgrep", &["-x", "coreaudiod"]).trim().is_empty();

    let mut out = format!(
        "Output device: {output} · volume {volume}% · {}\n",
        if muted { "MUTED" } else { "not muted" }
    );
    out.push_str(&if muted {
        "→ Sound is muted — press F10 or raise the volume.".into()
    } else if volume == 0 {
        "→ Volume is at zero — turn it up.".into()
    } else if !daemon_ok {
        "→ The sound system (coreaudiod) isn't running — restarting the Mac fixes this.".into()
    } else if output.to_lowercase().contains("display") || output.to_lowercase().contains("tv") {
        format!("→ Sound is going to \"{output}\" (a screen), not speakers — switch the output device in Control Center.")
    } else {
        format!("→ Audio setup looks fine ({output}, {volume}%). If one app is silent, check its own volume; if everything is, try switching output devices in Control Center.")
    });
    Ok(out)
}

// --- Settings (M3): volume and brightness. These run without a confirm
// step deliberately: the command itself names the exact change ("sound
// 15"), it applies instantly, and the same command reverses it — the
// confirmation layer is for operations the agent plans on your behalf.

fn osa(script: &str) -> String {
    sh("osascript", &["-e", script])
}

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
    match action.as_str() {
        "mute" => {
            osa("set volume output muted true");
            log("muted");
            return Ok("Muted.".into());
        }
        "unmute" => {
            osa("set volume output muted false");
            log("unmuted");
            return Ok("Unmuted.".into());
        }
        _ => {}
    }
    let target = level_from(&action, || {
        osa("output volume of (get volume settings)").trim().parse().unwrap_or(50)
    })?;
    osa(&format!("set volume output volume {target}"));
    // asking for a level means you want to hear it
    if target > 0 {
        osa("set volume output muted false");
    }
    log(&format!("{target}%"));
    Ok(format!("Volume {target}%."))
}

// No public API for brightness — this is the same private DisplayServices
// call the settings daemon uses (verified on this hardware). Fails soft.
#[tauri::command]
fn set_brightness(app: AppHandle, action: String) -> Result<String, String> {
    unsafe {
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
        if let Ok(conn) = mem_db(&app) {
            let _ = conn.execute(
                "INSERT INTO history (action, detail) VALUES ('brightness', ?1)",
                [format!("{target}%")],
            );
        }
        Ok(format!("Brightness {target}%."))
    }
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
        // maybe it's in the system trash, just not ours
        let names = osa("tell application \"Finder\" to get name of items in trash").to_lowercase();
        return Ok(if names.contains(&stem) {
            format!("“{what}” wasn't trashed by me — {SYS_TRASH_HINT}")
        } else {
            format!("I don't have “{what}” in my trash, and nothing like it is in the system Trash.")
        });
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
            diagnose_wifi,
            diagnose_slow,
            diagnose_audio,
            diagnose_file,
            trash_file,
            delete_file,
            get_history,
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
            forget_web
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
