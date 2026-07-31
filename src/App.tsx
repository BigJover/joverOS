import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

type AppEntry = { name: string; path: string };

type FileHit = { name: string; path: string; mtime: number; size: number; rank: number };

// Determiners aren't search terms — nothing is *named* "recent" or
// "pictures". They come out of the query and become ordering (sort words),
// a date window (time phrases), or a Spotlight kind filter (type words).
type SortKey = "new" | "old" | "big" | "small";
const SORT_FNS: Record<SortKey, (a: FileHit, b: FileHit) => number> = {
  new: (a, b) => b.mtime - a.mtime,
  old: (a, b) => a.mtime - b.mtime,
  big: (a, b) => b.size - a.size,
  small: (a, b) => a.size - b.size,
};
const SORT_WORDS: Record<string, SortKey> = {
  recent: "new", latest: "new", newest: "new", new: "new", last: "new",
  current: "new", freshest: "new",
  oldest: "old", old: "old", earliest: "old", original: "old", first: "old",
  biggest: "big", largest: "big", big: "big", large: "big", huge: "big", fattest: "big",
  smallest: "small", small: "small", tiniest: "small", tiny: "small", lightest: "small",
};

// Type words -> Spotlight kind filters (folders that *name* the thing still
// match — a "spring break" folder counts as pictures).
const KIND_WORDS: Record<string, string> = {
  picture: "image", photo: "image", image: "image", pic: "image",
  screenshot: "image", wallpaper: "image",
  video: "movie", movie: "movie", film: "movie", clip: "movie", recording: "movie",
  song: "music", music: "music", audio: "music", track: "music",
  pdf: "pdf",
  document: "document", doc: "document",
  spreadsheet: "spreadsheet", excel: "spreadsheet",
  presentation: "presentation", powerpoint: "presentation", slideshow: "presentation", deck: "presentation",
  folder: "folder", directory: "folder",
  note: "text", text: "text",
};

// Time phrases -> [since, until) windows in epoch seconds. Checked before
// single words so "last week" never reads as sort-word "last" + "week".
const DAY = 86400;
function timeWindow(phrase: string): [number, number | null] | null {
  const now = Math.floor(Date.now() / 1000);
  const midnight = Math.floor(new Date().setHours(0, 0, 0, 0) / 1000);
  switch (phrase) {
    case "today": return [midnight, null];
    case "yesterday": return [midnight - DAY, midnight];
    case "this week": case "past week": return [now - 7 * DAY, null];
    case "last week": return [now - 14 * DAY, now - 7 * DAY];
    case "this month": case "past month": return [now - 30 * DAY, null];
    case "last month": return [now - 60 * DAY, now - 30 * DAY];
    case "this year": case "past year": return [now - 365 * DAY, null];
    case "last year": return [now - 730 * DAY, now - 365 * DAY];
    default: return null;
  }
}

// dangling connectives left behind once determiners are stripped
const FILLER = new Set(["from", "of", "the", "my", "a", "an", "in", "that"]);

type ParsedSearch = {
  terms: string;
  sortBy: SortKey | null;
  kind: string | null;
  since: number | null;
  until: number | null;
};

function parseSearch(raw: string): ParsedSearch {
  const words = raw.split(/\s+/);
  const out: ParsedSearch = { terms: "", sortBy: null, kind: null, since: null, until: null };
  const kept: string[] = [];
  for (let i = 0; i < words.length; i++) {
    const w = words[i].toLowerCase();
    const pair = i + 1 < words.length ? `${w} ${words[i + 1].toLowerCase()}` : "";
    const win = timeWindow(pair) ?? timeWindow(w);
    if (win) {
      [out.since, out.until] = win;
      if (timeWindow(pair)) i++;
      continue;
    }
    const stem = w.replace(/s$/, "");
    if (!out.kind && KIND_WORDS[stem]) {
      out.kind = KIND_WORDS[stem];
      continue;
    }
    if (!out.sortBy && SORT_WORDS[w]) {
      out.sortBy = SORT_WORDS[w];
      continue;
    }
    kept.push(words[i]);
  }
  const terms = kept.filter((w) => !FILLER.has(w.toLowerCase()));
  out.terms = terms.join(" ");
  // a bare "find pictures" with no other signal should read as browsing,
  // newest first — not an alphabetical grab-bag
  if (!out.terms && !out.sortBy) out.sortBy = "new";
  return out;
}

type PlannedMove = { from: string; to: string };
type OrganizePlan = { summary: string; moves: PlannedMove[] };

// Anything the agent plans on the user's behalf waits here for Enter.
type Pending = { exec: () => Promise<string> };

type Intent = {
  intent: "app_launch" | "web_open" | "web_search" | "file_search" | "file_organize" | "troubleshoot" | "settings" | "empty_trash" | "file_trash" | "unknown";
  app?: string;
  query?: string;
  url?: string;
  reply?: string;
};

function score(query: string, name: string): number {
  const q = query.toLowerCase();
  const n = name.toLowerCase();
  if (n === q) return 1000;
  if (n.startsWith(q)) return 500 - n.length;
  const idx = n.indexOf(q);
  if (idx >= 0) return 200 - idx - n.length / 100;
  let qi = 0;
  for (let i = 0; i < n.length && qi < q.length; i++) {
    if (n[i] === q[qi]) qi++;
  }
  if (qi === q.length) return 50 - n.length;
  return -1;
}

// Deterministic browser targeting (the model is unreliable here): default
// browser always, unless the input *specifies* one — browser name as the
// first word ("chrome search turtles") or after in/with/using/on ("search
// turtles in firefox"). A browser word mid-query ("safari park tickets")
// is part of the query, not a target.
const BROWSERS = ["chrome", "safari", "firefox", "edge", "brave", "arc", "opera", "vivaldi"];
const TARGET_PREPS = ["in", "with", "using", "on"];

function browserFromInput(input: string, apps: AppEntry[]): string | null {
  const words = input.toLowerCase().split(/\s+/);
  for (const b of BROWSERS) {
    const i = words.indexOf(b);
    const specified = i === 0 || (i > 0 && TARGET_PREPS.includes(words[i - 1]));
    if (specified) {
      const match = fuzzyMatch(b, apps)[0];
      if (match) return match.path;
    }
  }
  return null;
}

// Sites the bar knows how to search directly. When the input starts with one
// of these, the destination site is certain regardless of what the model
// says — worst case is that site's own search page, never a Google detour.
const SITES: Record<string, { host: string; search: string }> = {
  youtube: { host: "youtube.com", search: "https://www.youtube.com/results?search_query=" },
  twitch: { host: "twitch.tv", search: "https://www.twitch.tv/search?term=" },
  reddit: { host: "reddit.com", search: "https://www.reddit.com/search/?q=" },
  amazon: { host: "amazon.com", search: "https://www.amazon.com/s?k=" },
  github: { host: "github.com", search: "https://github.com/search?q=" },
  linkedin: { host: "linkedin.com", search: "https://www.linkedin.com/search/results/all/?keywords=" },
  wikipedia: { host: "wikipedia.org", search: "https://en.wikipedia.org/wiki/Special:Search?search=" },
  steam: { host: "steampowered.com", search: "https://store.steampowered.com/search/?term=" },
  ebay: { host: "ebay.com", search: "https://www.ebay.com/sch/i.html?_nkw=" },
  google: { host: "google.com", search: "https://www.google.com/search?q=" },
};

function siteFromInput(
  input: string
): { name: string; site: (typeof SITES)[string]; rest: string } | null {
  const words = input.trim().split(/\s+/);
  const name = words[0].toLowerCase().replace(/[^a-z]/g, "");
  const site = SITES[name];
  const rest = words.slice(1).join(" ");
  return site && rest ? { name, site, rest } : null;
}

// Loose settings parsing: "sound volume 15%", "sound 15", "volume up",
// "brightness 70" all resolve the same. A number wins; otherwise a
// direction word; bare "sound" stays a diagnosis, not a change.
const SET_ACTIONS: Record<string, string> = {
  up: "up", higher: "up", louder: "up", raise: "up", increase: "up", brighter: "up",
  down: "down", lower: "down", quieter: "down", decrease: "down", dim: "down", dimmer: "down", darker: "down",
  max: "max", maximum: "max", full: "max", min: "min", minimum: "min", zero: "min",
  mute: "mute", muted: "mute", silence: "mute", unmute: "unmute",
};

function parseSetAction(text: string): string | null {
  const num = text.match(/(\d{1,3})\s*%?/);
  if (num) return String(Math.min(100, parseInt(num[1], 10)));
  for (const w of text.toLowerCase().split(/\s+/)) {
    if (SET_ACTIONS[w]) return SET_ACTIONS[w];
  }
  return null;
}

// Verb shortcuts: leading command words that pin the intent outright — a
// small vocabulary the user can rely on, instant and misroute-proof (no
// model call). Free-form phrasing still falls through to the router.
// Contract: find = files, search = web, launch = apps. No overlaps.
const VERB_PINS: { verbs: string[]; intent: Intent["intent"] }[] = [
  { verbs: ["find", "where is", "wheres", "locate"], intent: "file_search" },
  { verbs: ["search for", "search", "look up", "lookup"], intent: "web_search" },
  { verbs: ["launch", "open app"], intent: "app_launch" },
  { verbs: ["organize", "tidy", "clean up", "sort"], intent: "file_organize" },
  { verbs: ["disk space", "storage", "wifi", "internet"], intent: "troubleshoot" },
  { verbs: ["empty trash", "empty the trash", "take out the trash"], intent: "empty_trash" },
  { verbs: ["trash", "delete", "remove"], intent: "file_trash" },
];

// sound/volume/brightness lead-words: with an action they're a settings
// change, bare they fall through (sound -> audio diagnosis).
const SETTING_TARGETS: Record<string, string> = {
  sound: "volume", volume: "volume", audio: "volume", mute: "volume", unmute: "volume",
  brightness: "brightness", display: "brightness", screen: "brightness",
};

function verbPin(input: string): Intent | null {
  const lower = input.toLowerCase();
  const first = lower.split(/\s+/)[0];
  const target = SETTING_TARGETS[first];
  if (target) {
    const rest = input.slice(first.length).trim();
    const action =
      first === "mute" || first === "unmute" ? first : parseSetAction(rest);
    if (action) return { intent: "settings", query: `${target} ${action}` };
    if (first === "sound" || first === "audio" || first === "volume")
      return { intent: "troubleshoot", query: "audio" };
  }
  for (const { verbs, intent } of VERB_PINS) {
    for (const v of verbs) {
      if (lower !== v && !lower.startsWith(v + " ")) continue;
      let rest = input.slice(v.length + 1).trim();
      if (intent === "file_search") rest = rest.replace(/^(my|the)\s+/i, "");
      if (intent === "app_launch") return { intent, app: rest };
      if (intent === "troubleshoot") return { intent, query: v };
      if (intent === "file_trash") rest = rest.replace(/^(my|the|that|this)\s+/i, "");
      return { intent, query: rest };
    }
  }
  return null;
}

// "search turtles in firefox" — the browser tail is targeting, not query.
const stripBrowserTail = (q: string) =>
  q.replace(
    new RegExp(`\\s+(?:${TARGET_PREPS.join("|")})\\s+(?:${BROWSERS.join("|")})\\s*$`, "i"),
    ""
  );

function fuzzyMatch(query: string, apps: AppEntry[]): AppEntry[] {
  return apps
    .map((app) => ({ app, s: score(query, app.name) }))
    .filter((r) => r.s >= 0)
    .sort((a, b) => b.s - a.s)
    .slice(0, 6)
    .map((r) => r.app);
}

function App() {
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [files, setFiles] = useState<FileHit[]>([]);
  const [pending, setPending] = useState<Pending | null>(null);
  const [thinking, setThinking] = useState(false);
  const [status, setStatus] = useState("");
  const [reply, setReply] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    invoke<AppEntry[]>("list_apps").then(setApps);
    const unlisten = listen("bar-shown", () => {
      setQuery("");
      setSelected(0);
      setFiles([]);
      setPending(null);
      setThinking(false);
      setStatus("");
      setReply("");
      invoke<AppEntry[]>("list_apps").then(setApps);
      inputRef.current?.focus();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const results = query ? fuzzyMatch(query, apps) : [];

  const launch = (path: string) => invoke("launch_app", { path });

  const route = async (input: string) => {
    setThinking(true);
    setReply("");
    setStatus("");
    try {
      // "undo" is a reserved word: straight to the log, no model.
      if (/^(undo|put (it |that )?back|restore trash)$/i.test(input.trim())) {
        setReply(await invoke<string>("undo_last"));
        return;
      }

      // Browser choice is decided from the raw input only — the model's
      // "app" field is ignored (it sometimes holds the site name, e.g.
      // "github", which would resolve to the wrong installed app).
      const browserPath = browserFromInput(input, apps);
      const pinned = siteFromInput(input);
      const siteSearch = pinned
        ? pinned.site.search + encodeURIComponent(pinned.rest)
        : null;
      const google = (q: string) =>
        "https://www.google.com/search?q=" + encodeURIComponent(q);

      // Learned destination? Opens instantly, no model call. If the page
      // has died (the fallback got swapped in), unlearn it so the next use
      // re-resolves fresh.
      const remembered = await invoke<string | null>("recall_web", { input });
      if (remembered) {
        const opened = await invoke<string>("open_url", {
          url: remembered,
          browserPath,
          fallbackUrl: siteSearch ?? google(input),
        });
        if (opened !== remembered) await invoke("forget_web", { input });
        return;
      }

      const intent = verbPin(input) ?? (await invoke<Intent>("route_intent", { input }));
      if (intent.intent === "app_launch" && intent.app) {
        const match = fuzzyMatch(intent.app, apps)[0];
        if (match) {
          launch(match.path);
        } else {
          setReply(`No app called “${intent.app}” here.`);
        }
      } else if (
        pinned !== null ||
        (intent.intent === "web_open" && intent.url) ||
        (intent.intent === "web_search" && intent.query)
      ) {
        // Normalize the model-produced URL: force https, no bare domains.
        const raw = (intent.url ?? "").trim().replace(/^http:\/\//, "");
        const modelUrl = raw
          ? raw.startsWith("https://")
            ? raw
            : "https://" + raw
          : "";
        let url: string;
        let fallbackUrl: string | null = null;
        let learn: string | null = null;

        if (pinned && siteSearch) {
          // Destination site is certain. Trust the model only if it gave an
          // on-site link beyond the bare homepage; otherwise *find* the page
          // by searching the live web. Worst case is the site's own search —
          // never a Google detour.
          let onSiteDeepLink = false;
          try {
            const u = new URL(modelUrl);
            onSiteDeepLink =
              u.hostname.endsWith(pinned.site.host) &&
              (u.pathname !== "/" || u.search !== "");
          } catch {
            /* no usable model url */
          }
          if (onSiteDeepLink) {
            url = modelUrl;
          } else {
            setStatus(`searching ${pinned.name} for ${pinned.rest}…`);
            url = await invoke<string>("resolve_web", {
              query: pinned.rest,
              siteHost: pinned.site.host,
            }).catch(() => siteSearch);
          }
          learn = url === siteSearch ? null : url;
          fallbackUrl = siteSearch;
        } else if (intent.intent === "web_open" && modelUrl) {
          url = modelUrl;
          fallbackUrl = google(input);
          learn = modelUrl;
        } else {
          url = google(stripBrowserTail(intent.query ?? input));
        }

        const opened = await invoke<string>("open_url", {
          url,
          browserPath,
          fallbackUrl,
        });
        // Only remember pages that opened as intended — a swapped-in
        // fallback or a search-results page isn't worth learning.
        if (learn && opened === learn) {
          await invoke("remember_web", { input, url: opened });
        }
      } else if (intent.intent === "troubleshoot") {
        const area = (intent.query ?? input).toLowerCase();
        const [cmd, what] = /wi-?fi|internet|network|online|connect/.test(area)
          ? ["diagnose_wifi", "network"]
          : /slow|sluggish|lag|freez|frozen/.test(area)
          ? ["diagnose_slow", "what's slow"]
          : /audio|sound|speaker|volume|mute|hear/.test(area)
          ? ["diagnose_audio", "audio"]
          : ["diagnose_disk", "disk"];
        setStatus(`checking ${what}…`);
        setReply(await invoke<string>(cmd));
      } else if (intent.intent === "file_trash") {
        const what = (intent.query ?? "").trim();
        if (!what) {
          setReply("Trash what? Name the file — like “trash old notes”.");
        } else {
          const { terms, kind, since, until } = parseSearch(what);
          setStatus(`finding ${terms || what}…`);
          const hits = await invoke<FileHit[]>("search_files", {
            query: terms || what,
            kind,
            since,
            until,
            order: null,
          });
          if (hits.length === 0) {
            setReply(`No file matching “${what}”.`);
          } else {
            const top = hits[0];
            const more =
              hits.length > 1
                ? ` (${hits.length - 1} other match${hits.length === 2 ? "" : "es"} — be more specific if this isn't it)`
                : "";
            setPending({ exec: () => invoke<string>("trash_file", { path: top.path }) });
            setReply(
              `Move “${top.name}” to Trash?\n${top.path.replace(/^\/Users\/[^/]+/, "~")}\nEnter to confirm — undo puts it back. Esc to cancel.${more}`
            );
          }
        }
      } else if (intent.intent === "empty_trash") {
        const n = await invoke<number>("trash_count");
        if (n === 0) {
          setReply("Trash is already empty.");
        } else {
          setPending({ exec: () => invoke<string>("empty_trash") });
          setReply(
            `Trash has ${n} item${n === 1 ? "" : "s"} — Enter to empty it for good (this can't be undone), Esc to cancel.`
          );
        }
      } else if (intent.intent === "settings") {
        const q = (intent.query ?? input).toLowerCase();
        const target = /bright|display|screen/.test(q) ? "set_brightness" : "set_volume";
        const action = parseSetAction(q);
        if (!action) {
          setReply('Say a level — like "sound 30", "volume up", or "brightness 70".');
        } else {
          setReply(await invoke<string>(target, { action }));
        }
      } else if (intent.intent === "file_organize") {
        const p = await invoke<OrganizePlan>("plan_organize", {
          folder: intent.query ?? input,
        });
        if (p.moves.length === 0) {
          setReply("Nothing loose to organize there.");
        } else {
          setPending({ exec: () => invoke<string>("apply_organize", { moves: p.moves }) });
          setReply(`${p.summary} — Enter to do it, Esc to cancel.`);
        }
      } else if (intent.intent === "file_search") {
        const raw = (intent.query ?? input).trim();
        const { terms: q, sortBy, kind, since, until } = parseSearch(raw);
        setStatus(`searching files for ${q || kind || "recent"}…`);
        let hits = await invoke<FileHit[]>("search_files", {
          query: q,
          kind,
          since,
          until,
          order: sortBy,
        });
        if (q && hits.length > 8) {
          setStatus(`narrowing ${hits.length} matches…`);
          const order = await invoke<number[]>("rerank_files", {
            query: q,
            files: hits,
          }).catch(() => [] as number[]);
          if (order.length > 0) hits = order.map((i) => hits[i]);
        }
        // Ordering contract, whatever the query: files matched by NAME
        // always rank above files that merely mention the words, and the
        // qualifier (recency when none given) orders within each side of
        // that line — never across it.
        const named = (h: FileHit) => (h.rank >= 2 ? 1 : 0);
        const fn = sortBy ? SORT_FNS[sortBy] : SORT_FNS.new;
        hits = [...hits].sort((a, b) => named(b) - named(a) || fn(a, b));
        hits = hits.slice(0, 8);
        if (hits.length > 0) {
          setFiles(hits);
          setSelected(0);
        } else {
          setReply(`No files matching “${raw}”.`);
        }
      } else {
        setReply(intent.reply || "Can't do that yet.");
      }
    } catch (e) {
      setReply(String(e));
    } finally {
      setThinking(false);
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    // A pending action owns the keyboard: nothing happens without Enter here.
    if (pending) {
      if (e.key === "Enter") {
        const { exec } = pending;
        setPending(null);
        setReply("working…");
        exec().then(setReply).catch((err) => setReply(String(err)));
      } else if (e.key === "Escape") {
        setPending(null);
        setReply("Cancelled — nothing changed.");
      }
      return;
    }
    if (e.key === "Escape") {
      invoke("hide_bar");
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected((s) => Math.min(s + 1, (files.length || results.length) - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((s) => Math.max(s - 1, 0));
    } else if (e.key === "Enter") {
      if (files[selected]) {
        launch(files[selected].path);
      } else if (results[selected]) {
        launch(results[selected].path);
      } else if (query.trim() && !thinking) {
        route(query.trim());
      }
    }
  };

  return (
    <div className="bar" onKeyDown={onKeyDown}>
      <input
        ref={inputRef}
        autoFocus
        spellCheck={false}
        value={query}
        placeholder="Type to launch, or ask…"
        onChange={(e) => {
          setQuery(e.target.value);
          setSelected(0);
          setFiles([]);
          setPending(null);
          setReply("");
        }}
      />
      {files.length > 0 && (
        <ul className="results">
          {files.map((f, i) => (
            <li
              key={f.path}
              className={i === selected ? "selected" : ""}
              onMouseDown={() => launch(f.path)}
            >
              {f.name}
              <span className="path">{f.path.replace(/^\/Users\/[^/]+/, "~")}</span>
            </li>
          ))}
        </ul>
      )}
      {files.length === 0 && results.length > 0 && (
        <ul className="results">
          {results.map((r, i) => (
            <li
              key={r.path}
              className={i === selected ? "selected" : ""}
              onMouseDown={() => launch(r.path)}
            >
              {r.name}
            </li>
          ))}
        </ul>
      )}
      {thinking && <div className="reply thinking">{status || "…"}</div>}
      {!thinking && reply && <div className="reply">{reply}</div>}
    </div>
  );
}

export default App;
