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
const FILLER = new Set(["from", "of", "to", "the", "my", "a", "an", "in", "that"]);

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
  intent: "app_launch" | "web_open" | "web_search" | "file_search" | "file_organize" | "troubleshoot" | "settings" | "empty_trash" | "file_trash" | "file_delete" | "history" | "process_kill" | "process_list" | "port_lookup" | "process_priority" | "unknown";
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
  {
    verbs: [
      "history", "what have you done", "what did you do", "what have you changed",
      "what did you change", "what happened", "changelog", "past actions",
      "recent actions", "actions", "action log", "activity log", "activity",
      "recent changes", "changes", "audit", "audit log", "paper trail", "log",
    ],
    intent: "history",
  },
];

// sound/volume/brightness lead-words: with an action they're a settings
// change, bare they fall through (sound -> audio diagnosis).
const SETTING_TARGETS: Record<string, string> = {
  sound: "volume", volume: "volume", audio: "volume", mute: "volume", unmute: "volume",
  brightness: "brightness", display: "brightness", screen: "brightness",
};

// "move X to the trash" / "throw away X" have exactly one meaning — no
// model needed, and the model sometimes reads "move" as folder-organize.
const TRASH_PHRASE = /^(?:move|put|send|throw|chuck|drag)\s+(.+?)\s+(?:into|to|in)\s+(?:the\s+)?(?:trash|bin)$/i;
const THROW_AWAY = /^(?:throw away|throw out|get rid of|dispose of)\s+(.+)$/i;
// "X won't open" in its usual shapes — pins to the file diagnosis
const WONT_OPEN =
  /^(?:why )?(?:won'?t|wont|can'?t|cannot)\s+(.+?)\s+open$|^(.+?)\s+(?:won'?t|wont|isn'?t|is not|doesn'?t|does not)\s+open(?:ing)?$|^(?:why )?can'?t (?:i )?open\s+(.+)$/i;

// hard delete fires ONLY on these exact shapes — never from the model
const PERM_DELETE =
  /^(?:permanently |hard )delete\s+(.+)$|^delete\s+(.+?)\s+(?:forever|permanently|for good)$|^shred\s+(.+)$/i;

function verbPin(input: string): Intent | null {
  const wo = input.match(WONT_OPEN);
  if (wo) {
    const name = (wo[1] ?? wo[2] ?? wo[3]).replace(/^(?:my|the|this|that)\s+/i, "");
    return { intent: "troubleshoot", query: `file ${name}` };
  }
  const perm = input.match(PERM_DELETE);
  if (perm) return { intent: "file_delete", query: perm[1] ?? perm[2] ?? perm[3] };
  const phrase = input.match(TRASH_PHRASE) ?? input.match(THROW_AWAY);
  if (phrase) return { intent: "file_trash", query: phrase[1] };
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
      if (intent === "history") return { intent };
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
      if (/^(undo|undo (that|it|last)|revert|revert (that|it|last)|go back|put (it |that )?back)$/i.test(input.trim())) {
        setReply(await invoke<string>("undo_last"));
        return;
      }
      // Scoped permissions: exact phrases only — the model has no route
      // to granting itself anything.
      const scopeOf = (t: string) =>
        /empty/.test(t) ? "empty_trash"
        : /organiz/.test(t) ? "organize"
        : /permanent|forever|shred/.test(t) ? "delete"
        : /trash|delete|remove/.test(t) ? "trash"
        : t.trim();
      const grantM = input.trim().match(/^(?:always allow|stop asking (?:before|about|for|when i)?\s*)(.+)$/i);
      const askM = input.trim().match(/^(?:always ask(?:\s+(?:before|about|for))?|revoke)\s+(.+)$/i);
      if (grantM || askM) {
        const raw = (grantM?.[1] ?? askM?.[1] ?? "").toLowerCase();
        setReply(await invoke<string>("set_grant", { scope: scopeOf(raw), allow: !!grantM }));
        return;
      }
      if (/^(permissions|grants|what can you do without asking)$/i.test(input.trim())) {
        setReply(await invoke<string>("list_grants"));
        return;
      }
      // Process management (M6) — deterministic patterns, no LLM needed.
      const t = input.trim();

      // "top processes" / "what's eating my cpu" / "what's running"
      if (/^(?:top\s+processes?|what(?:'s|\s+is)\s+(?:using|eating)\s+(?:my\s+)?cpu|cpu\s+hogs?|show\s+processes?|what(?:'s|\s+is)\s+running\s+(?:right\s+now)?)$/i.test(t)) {
        setStatus("checking processes…");
        setReply(await invoke<string>("list_processes"));
        return;
      }

      // "what's on port 3000" / "what's using port 8080" / "port 3000"
      const portM = t.match(/^(?:what(?:'s|\s+is)\s+(?:on|using)\s+port\s+|port\s+)(\d+)$/i);
      if (portM) {
        const port = parseInt(portM[1]);
        setStatus(`checking port ${port}…`);
        setReply(await invoke<string>("port_lookup", { port }));
        return;
      }

      // "set chrome to high priority" / "boost safari priority" / "lower discord priority"
      const priorityM =
        t.match(/^(?:set\s+)?(.+?)\s+to\s+(high|low|normal|background)\s+priority$/i) ??
        t.match(/^(boost|prioritize)\s+(.+?)(?:\s+priority)?$/i) ??
        t.match(/^(lower|deprioritize)\s+(.+?)(?:\s+priority)?$/i);
      if (priorityM) {
        let procName: string;
        let level: string;
        if (/^(boost|prioritize)/.test(priorityM[1] ?? "")) {
          procName = priorityM[2].trim();
          level = "high";
        } else if (/^(lower|deprioritize)/.test(priorityM[1] ?? "")) {
          procName = priorityM[2].trim();
          level = "low";
        } else {
          procName = priorityM[1].trim();
          level = (priorityM[2] ?? "normal").toLowerCase();
        }
        setReply(await invoke<string>("set_process_priority", { name: procName, level }));
        return;
      }

      // Media Controls (M8)
      // "play" / "pause" / "resume" — optional native app name launches it if not open
      // web-only services (soundcloud, youtube music, etc.) open in browser
      const playM = t.match(/^(?:pause|play|resume)(?:\s+(spotify|apple\s+music|soundcloud|youtube\s*music|tidal|deezer|amazon\s*music))?$/i);
      if (playM) {
        const svc = (playM[1] ?? "").toLowerCase().replace(/\s+/g, "");
        const webServices: Record<string, string> = {
          soundcloud: "https://soundcloud.com",
          youtubemusic: "https://music.youtube.com",
          deezer: "https://www.deezer.com",
        };
        if (webServices[svc]) {
          await invoke("open_url", { url: webServices[svc], browserPath: null, fallbackUrl: null });
          setReply(`Opening ${playM[1]!} in browser.`);
        } else {
          const appArg = svc || undefined;
          if (appArg) setStatus(`launching ${playM[1]}…`);
          setReply(await invoke<string>("media_control", { action: "playpause", app: appArg }));
        }
        return;
      }
      // "next" / "skip" / "skip track" / "skip song"
      if (/^(?:next|next\s+(?:track|song)|skip(?:\s+(?:track|song))?)$/i.test(t)) {
        setReply(await invoke<string>("media_control", { action: "next", app: undefined }));
        return;
      }
      if (/^(?:previous|prev|previous\s+(?:track|song)|last\s+(?:track|song)|go\s+back\s+(?:a\s+)?(?:track|song))$/i.test(t)) {
        setReply(await invoke<string>("media_control", { action: "previous" }));
        return;
      }
      // "skip forward 30" / "skip 30" / "skip back 30" / "rewind 30"
      const skipFwdM = t.match(/^(?:skip(?:\s+(?:forward|ahead))?|fast\s+forward)\s+(\d+)(?:\s+(?:seconds?|secs?))?$/i);
      const skipBckM = t.match(/^(?:skip\s+back(?:ward)?|rewind)\s+(\d+)(?:\s+(?:seconds?|secs?))?$/i);
      if (skipFwdM) {
        setReply(await invoke<string>("media_skip", { seconds: parseInt(skipFwdM[1]) }));
        return;
      }
      if (skipBckM) {
        setReply(await invoke<string>("media_skip", { seconds: -parseInt(skipBckM[1]) }));
        return;
      }
      // "what's playing" / "now playing" / "current song"
      if (/^(?:what(?:'s|\s+is)\s+(?:playing|this\s+(?:song|track))|now\s+playing|current\s+(?:track|song|music))$/i.test(t)) {
        setReply(await invoke<string>("now_playing"));
        return;
      }
      // "play [song] on spotify/apple music" — specific app requested
      const playOnM = t.match(/^play\s+(.+?)\s+on\s+(spotify|apple\s+music|music)$/i);
      if (playOnM) {
        const query = playOnM[1].trim();
        const preferApp = playOnM[2].trim().toLowerCase();
        setStatus(`searching ${preferApp}…`);
        setReply(await invoke<string>("play_track", { query, preferApp }));
        return;
      }
      // "play [song]" — use whatever music app is open, YouTube fallback
      // Guard: don't catch bare service names (those are handled by playM above)
      const MEDIA_SERVICES = /^(?:spotify|apple\s+music|soundcloud|youtube\s*music|tidal|deezer|amazon\s*music)$/i;
      const playTrackM = t.match(/^play\s+(.+)$/i);
      if (playTrackM && !MEDIA_SERVICES.test(playTrackM[1].trim())) {
        const query = playTrackM[1].trim();
        setStatus(`searching for ${query}…`);
        setReply(await invoke<string>("play_track", { query, preferApp: undefined }));
        return;
      }
      // "setup spotify CLIENT_ID CLIENT_SECRET"
      const setupSpotifyM = t.match(/^(?:setup|configure|set up)\s+spotify\s+(\S+)\s+(\S+)$/i);
      if (setupSpotifyM) {
        setReply(await invoke<string>("setup_spotify", { clientId: setupSpotifyM[1], clientSecret: setupSpotifyM[2] }));
        return;
      }
      // "shuffle on" / "shuffle off"
      const shuffleM = t.match(/^shuffle\s+(on|off)$/i);
      if (shuffleM) {
        const on = shuffleM[1].toLowerCase() === "on";
        setReply(await invoke<string>("media_shuffle", { on }));
        return;
      }

      // Window Management (M9)
      // "snap safari left/right/top/bottom/center"
      const snapM = t.match(/^snap\s+(.+?)\s+(left|right|top|bottom|center)$/i);
      if (snapM) {
        setReply(await invoke<string>("window_manage", {
          action: `snap_${snapM[2].toLowerCase()}`,
          appName: snapM[1].trim(),
        }));
        return;
      }
      // "center safari"
      const centerWinM = t.match(/^center\s+(.+)$/i);
      if (centerWinM) {
        setReply(await invoke<string>("window_manage", { action: "center", appName: centerWinM[1].trim() }));
        return;
      }
      // "fullscreen spotify" / "spotify fullscreen" / "make spotify fullscreen"
      const fullscreenM =
        t.match(/^(?:fullscreen|make\s+fullscreen|toggle\s+fullscreen)\s+(.+)$/i) ??
        t.match(/^(?:make\s+)?(.+?)\s+(?:full\s*screen|fullscreen)$/i);
      if (fullscreenM) {
        setReply(await invoke<string>("window_manage", { action: "fullscreen", appName: fullscreenM[1].trim() }));
        return;
      }
      // "minimize safari"
      const minimizeM = t.match(/^(?:minimize|minimise)\s+(.+)$/i);
      if (minimizeM) {
        setReply(await invoke<string>("window_manage", { action: "minimize", appName: minimizeM[1].trim() }));
        return;
      }
      // "focus safari" / "bring safari to front"
      const focusWinM =
        t.match(/^focus\s+(.+)$/i) ??
        t.match(/^bring\s+(.+?)\s+(?:to\s+)?(?:the\s+)?front(?:ward)?$/i);
      if (focusWinM) {
        setReply(await invoke<string>("window_manage", { action: "focus", appName: focusWinM[1].trim() }));
        return;
      }
      // "hide all" / "hide safari"
      if (/^hide\s+all$/i.test(t)) {
        setReply(await invoke<string>("window_manage", { action: "hide_all", appName: undefined }));
        return;
      }
      const hideWinM = t.match(/^hide\s+(.+)$/i);
      if (hideWinM) {
        setReply(await invoke<string>("window_manage", { action: "hide", appName: hideWinM[1].trim() }));
        return;
      }
      // "show all" / "show safari"
      if (/^show\s+all$/i.test(t)) {
        setReply(await invoke<string>("window_manage", { action: "show_all", appName: undefined }));
        return;
      }
      const showWinM = t.match(/^show\s+(.+)$/i);
      if (showWinM) {
        setReply(await invoke<string>("window_manage", { action: "show", appName: showWinM[1].trim() }));
        return;
      }

      // Game Mode (M7)
      // "game mode off"
      if (/^game\s+mode\s+off$/i.test(t)) {
        setStatus("exiting game mode…");
        setReply(await invoke<string>("game_mode_off"));
        return;
      }
      // "game mode on" / "game mode minecraft" / "game mode on for minecraft"
      const gameModeM = t.match(/^game\s+mode\s+(?:on(?:\s+for\s+)?|for\s+)?(.*)$/i);
      if (gameModeM) {
        const profile = gameModeM[1].trim().toLowerCase() || null;
        setStatus(profile ? `activating ${profile} mode…` : "activating game mode…");
        setReply(await invoke<string>("game_mode_on", { profile: profile || undefined }));
        return;
      }
      // "save game profile minecraft"
      const saveProfileM = t.match(/^save\s+(?:game\s+)?profile\s+(.+)$/i);
      if (saveProfileM) {
        const name = saveProfileM[1].trim();
        setStatus(`saving profile ${name}…`);
        setReply(await invoke<string>("save_game_profile", { name }));
        return;
      }
      // "game profiles" / "list game profiles" / "my game profiles"
      if (/^(?:list\s+|show\s+|my\s+)?game\s+profiles?$/i.test(t)) {
        setReply(await invoke<string>("list_game_profiles"));
        return;
      }

      // "kill chrome" / "force quit discord" / "terminate spotify"
      // Guard: skip if the word after kill looks like a pronoun/article
      const killM = t.match(/^(?:force\s+)?(?:kill|force\s+quit|terminate|end)\s+(?!the\b|my\b|it\b|that\b|a\b)(.+)$/i);
      if (killM) {
        const procName = killM[1].trim();
        const force = /^force/i.test(t);
        const exec = () => invoke<string>("kill_process", { name: procName, force });
        if (force) {
          setPending({ exec });
          setReply(`Force-kill "${procName}"? This is instant and can't be undone. Enter to confirm, Esc to cancel.`);
        } else {
          setPending({ exec });
          setReply(`Quit "${procName}"? Enter to confirm, Esc to cancel.`);
        }
        return;
      }

      // Workflows: precise names, deterministic grammar only.
      const wfSave = t.match(/^(?:save|record)\b.*?\bas workflow\s+(.+)$/i) ?? t.match(/^(?:save|record) workflow\s+(.+)$/i);
      const wfDel = t.match(/^(?:delete|remove|forget) workflow\s+(.+)$/i);
      const wfRun = t.match(/^(?:run |open |load |start )?workflow\s+(.+)$/i);
      if (/^(?:list |show |my )?workflows$/i.test(t)) {
        setReply(await invoke<string>("list_workflows"));
        return;
      }
      // Bare "save workflow" with no name — prompt instead of falling to LLM.
      if (/^(?:save|record)\s+(?:as\s+)?workflow$/i.test(t)) {
        setReply(`What would you like to name it? Try: save workflow 1`);
        return;
      }
      if (wfSave) {
        const name = wfSave[1];
        if (await invoke<boolean>("workflow_exists", { name })) {
          setPending({ exec: () => invoke<string>("save_workflow", { name }) });
          setReply(`Workflow ${name} already exists — Enter to overwrite it with the current setup, Esc to keep it.`);
        } else {
          setReply(await invoke<string>("save_workflow", { name }));
        }
        return;
      }
      if (wfDel) {
        const name = wfDel[1];
        setPending({ exec: () => invoke<string>("delete_workflow", { name }) });
        setReply(`Forget workflow ${name}? Enter to confirm, Esc to cancel.`);
        return;
      }
      if (wfRun) {
        setReply(await invoke<string>("run_workflow", { name: wfRun[1] }));
        return;
      }
      // restore trash ≠ undo: brings back everything the bar ever trashed
      if (/^restore (the )?trash$/i.test(input.trim())) {
        setReply(await invoke<string>("restore_trash"));
        return;
      }
      // named restore: "put eva.jpg back", "restore eva", "bring back eva"
      const named =
        input.trim().match(/^(?:put|move|bring)\s+(.+?)\s+back$/i) ??
        input.trim().match(/^(?:restore|bring back|recover)\s+(.+)$/i);
      if (named && !/^(it|that|this|them)$/i.test(named[1])) {
        const what = named[1].replace(/^(?:my|the)\s+/i, "");
        setReply(await invoke<string>("restore_named", { what }));
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
        const fileName = area.match(/^file\s+(.+)$/)?.[1];
        if (fileName) {
          const { terms, kind, since, until } = parseSearch(fileName);
          setStatus(`finding ${terms || fileName}…`);
          const hits = await invoke<FileHit[]>("search_files", {
            query: terms || fileName,
            kind,
            since,
            until,
            order: null,
          });
          if (hits.length === 0) {
            setReply(`Couldn't find “${fileName}” to check.`);
          } else {
            setStatus(`checking ${hits[0].name}…`);
            setReply(await invoke<string>("diagnose_file", { path: hits[0].path }));
          }
          return;
        }
        const [cmd, what] = /wi-?fi|internet|network|online|connect/.test(area)
          ? ["diagnose_wifi", "network"]
          : /slow|sluggish|lag|freez|frozen/.test(area)
          ? ["diagnose_slow", "what's slow"]
          : /audio|sound|speaker|volume|mute|hear/.test(area)
          ? ["diagnose_audio", "audio"]
          : ["diagnose_disk", "disk"];
        setStatus(`checking ${what}…`);
        setReply(await invoke<string>(cmd));
      } else if (intent.intent === "file_delete" || intent.intent === "file_trash") {
        const permanent = intent.intent === "file_delete";
        // whatever routed us here, scrub trash-talk and fillers so only
        // the file description reaches the search
        const what = (intent.query ?? "")
          .replace(/\s+(?:into|to|in)\s+(?:the\s+)?(?:trash|bin)\s*$/i, "")
          .replace(/^(?:my|the|that|this)\s+/i, "")
          .trim();
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
            const exec = () =>
              invoke<string>(permanent ? "delete_file" : "trash_file", { path: top.path });
            // trash is grantable (reversible); permanent delete never is
            if (!permanent && (await invoke<boolean>("check_grant", { scope: "trash" }))) {
              setReply((await exec()) + " (auto — “always ask before trash” reverts)");
            } else {
              setPending({ exec });
              setReply(
                permanent
                  ? `DELETE “${top.name}” FOR GOOD?\n${top.path.replace(/^\/Users\/[^/]+/, "~")}\nThis can NOT be undone. Enter to delete forever, Esc to cancel.${more}`
                  : `Move “${top.name}” to Trash?\n${top.path.replace(/^\/Users\/[^/]+/, "~")}\nEnter to confirm — undo puts it back. Esc to cancel.${more}`
              );
            }
          }
        }
      } else if (intent.intent === "process_list") {
        setStatus("checking processes…");
        setReply(await invoke<string>("list_processes"));
      } else if (intent.intent === "port_lookup") {
        const port = parseInt(intent.query ?? "0");
        if (!port) {
          setReply("Which port? Try: what's on port 3000");
        } else {
          setStatus(`checking port ${port}…`);
          setReply(await invoke<string>("port_lookup", { port }));
        }
      } else if (intent.intent === "process_kill") {
        const procName = intent.app ?? intent.query ?? "";
        if (!procName) {
          setReply("Which process? Try: kill chrome");
        } else {
          const force = /force/i.test(input);
          const exec = () => invoke<string>("kill_process", { name: procName, force });
          setPending({ exec });
          setReply(`Quit "${procName}"? Enter to confirm, Esc to cancel.`);
        }
      } else if (intent.intent === "process_priority") {
        const procName = intent.app ?? "";
        const level = intent.query ?? "normal";
        if (!procName) {
          setReply("Which process? Try: set chrome to high priority");
        } else {
          setReply(await invoke<string>("set_process_priority", { name: procName, level }));
        }
      } else if (intent.intent === "history") {
        setReply(await invoke<string>("get_history"));
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
        } else if (await invoke<boolean>("check_grant", { scope: "organize" })) {
          setReply(
            (await invoke<string>("apply_organize", { moves: p.moves })) +
              " (auto — “always ask before organize” reverts)"
          );
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
        invoke("hide_bar");
      }
      return;
    }
    if (e.key === "Escape") {
      getCurrentWindow().hide();
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
