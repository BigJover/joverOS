import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

type AppEntry = { name: string; path: string };

type Intent = {
  intent: "app_launch" | "web_open" | "web_search" | "file_search" | "unknown";
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
  const [thinking, setThinking] = useState(false);
  const [reply, setReply] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    invoke<AppEntry[]>("list_apps").then(setApps);
    const unlisten = listen("bar-shown", () => {
      setQuery("");
      setSelected(0);
      setThinking(false);
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
    try {
      const intent = await invoke<Intent>("route_intent", { input });
      if (intent.intent === "app_launch" && intent.app) {
        const match = fuzzyMatch(intent.app, apps)[0];
        if (match) {
          launch(match.path);
        } else {
          setReply(`No app called “${intent.app}” here.`);
        }
      } else if (
        (intent.intent === "web_open" && intent.url) ||
        (intent.intent === "web_search" && intent.query)
      ) {
        // Normalize model-produced URLs: force https, no bare domains.
        const raw = (intent.url ?? "").trim().replace(/^http:\/\//, "");
        const url =
          intent.intent === "web_open"
            ? raw.startsWith("https://")
              ? raw
              : "https://" + raw
            : "https://www.google.com/search?q=" + encodeURIComponent(intent.query!);
        // Browser choice is decided from the raw input only — the model's
        // "app" field is ignored (it sometimes holds the site name, e.g.
        // "github", which would resolve to the wrong installed app).
        const browserPath = browserFromInput(input, apps);
        await invoke("open_url", { url, browserPath });
      } else if (intent.intent === "file_search") {
        setReply(`File search isn't wired up yet (coming in M2). Heard: “${intent.query ?? input}”.`);
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
    if (e.key === "Escape") {
      invoke("hide_bar");
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected((s) => Math.min(s + 1, results.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((s) => Math.max(s - 1, 0));
    } else if (e.key === "Enter") {
      if (results[selected]) {
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
          setReply("");
        }}
      />
      {results.length > 0 && (
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
      {thinking && <div className="reply thinking">…</div>}
      {!thinking && reply && <div className="reply">{reply}</div>}
    </div>
  );
}

export default App;
