# joverOS

**The agent-first OS.** One text bar, summoned from anywhere with `⌥ Space`, that does what you mean: launches your apps, takes you straight to pages on the web — and learns your destinations as you use it. All routing runs on a **local LLM**; nothing leaves your machine, no API keys, works offline.

Today it's a macOS launcher bar. The plan is bigger: this app becomes the desktop shell of a custom Linux distro that boots straight into the bar — an OS anyone can use, where the agent replaces the terminal, the settings maze, and the call to a tech-savvy relative.

## What it does right now (v0.1 — milestone M1)

- **Launch apps by name** — instant fuzzy matching as you type, no LLM in the loop (`obs`, `open the calculator`)
- **Go anywhere on the web by intent** —
  - `youtube` → homepage, `linkedin job board` → the jobs page
  - `youtube mr beast` → the channel itself, not a search detour
  - `amazon airpods` → Amazon's own search results
  - `chrome search turtles` → your search, in the browser you named (default browser otherwise — always)
- **Find, don't know** — for pages the model can't know, the bar searches the live web (DuckDuckGo, keyless) scoped to the site you named, and opens the top real result. Worst case is the site's own search page, never a Google detour.
- **Learned web memory** — destinations that worked are remembered in a local SQLite database and open instantly next time, zero model calls. Pages that die (404) are automatically unlearned and re-resolved fresh.
- **Organize with a paper trail** — "organize my downloads" plans the moves, shows you the plan, and touches nothing until you confirm. Every move is logged to SQLite; `undo` puts everything back.
- **Troubleshooting** — disk space ("what's eating my disk"), network ("wifi isn't working" — walks the stack and names the broken layer), slowness ("why is my mac slow" — CPU hogs, memory pressure, load), and audio ("no sound" — mute/volume/output device). Real diagnostics, plain-language verdicts, read-only: it looks, tells you, and touches nothing.
- **Destructive actions ask first** — "empty the trash" reports what's in it and waits for Enter, stating plainly that it can't be undone. macOS's own Finder consent backs it up.
- **Honest refusals** — anything it can't safely do yet gets a terse "can't do that yet", not a guess.

## Shortcuts — the bar's command vocabulary

Certain first words pin what happens, deterministically — no model, no guessing, instant:

| You type | It always means |
|---|---|
| `find …` / `where is …` / `locate …` | search **your files** |
| &nbsp;&nbsp;↳ `recent` / `oldest` / `biggest` / `smallest` … | **order** the results (`find recent resume`) |
| &nbsp;&nbsp;↳ `pictures` / `videos` / `pdfs` / `music` / `folders` … | filter by **type** (`find biggest video`) |
| &nbsp;&nbsp;↳ `today` / `yesterday` / `this week` / `last month` … | filter by **when** (`find screenshots from today`) |
| `search …` / `look up …` | search **the web** |
| `launch …` | open **an app** |
| `youtube …`, `reddit …`, `amazon …`, … | go to / search **that site** |
| `chrome …` / `firefox …` / `safari …` first word, or `… in firefox` | use **that browser** |
| `organize downloads` / `tidy desktop` | plan a **by-type cleanup**, shown first — nothing moves until you press Enter |
| `undo` | **reverse** the last file operation |
| `trash <file>` / `delete <file>` | move a file **to the Trash** — shows which file first, Enter confirms, `undo` puts it back |
| `empty trash` | count what's there, then **empty it** — only after you press Enter |
| `disk space` / `wifi` / `sound` / "why is my mac slow" | **diagnose** the problem, layer by layer |
| `sound 15` / `volume up` / `mute` / `brightness 70` | **set** volume or brightness — loose: `sound 15`, `sound volume 15%`, "dim the screen" all work |

Everything else is free-form — the local model figures out what you meant.

## Install (macOS)

1. Grab the `.dmg` from [Releases](../../releases) and drag **joverOS** to Applications.
2. Install [Ollama](https://ollama.com), then pull the brain: `ollama pull llama3.1:8b`. Keep Ollama running (menu-bar app) — without it the bar still launches apps, but web intents report "brain offline".
3. First launch: the app is unsigned, so **right-click → Open** and confirm the Gatekeeper prompt.
4. Press **⌥ Space** and type.

## Build from source

```
git clone https://github.com/BigJover/joverOS
cd joverOS
npm install
npm run tauri dev     # dev build
npm run tauri build   # produces the .dmg
```

Requires Node, Rust (rustup), and Ollama with `llama3.1:8b`.

## How it's built

Tauri 2 (Rust backend, React + TypeScript frontend). Three-tier design that keeps a local 8B model on small decisions and lets deterministic code do the work:

1. **Instant tier** — fuzzy app matching in the frontend, no model.
2. **Router tier** — one structured-output call to Ollama classifies input into a fixed intent schema (`app_launch` / `web_open` / `web_search` / `file_search` / `unknown`). The model can only answer in shapes the app knows how to execute — never freeform prose driving execution.
3. **Tool tier** — real code per intent: live-web resolver, HTTPS-only opener with 404-fallback, SQLite memory (`recall` / `remember` / `forget`).

## Roadmap

| Milestone | | |
|---|---|---|
| M0 | Summonable bar, fuzzy app launch | ✅ |
| M1 | Local-LLM intent router + full web stack + learned memory | ✅ |
| M2 | File search & organization with permission prompts + undo log | ⏳ next |
| M3 | Troubleshooting domains (wifi, slow machine, audio), history view, scoped permissions | |
| M4 | Workflows — save and restore whole working sessions | |
| M5 | Linux, then the distro: the bar becomes the desktop shell | |

Full spec and product principles in [CLAUDE.md](CLAUDE.md).

---

Built by [Jovan Kirovski](https://github.com/BigJover).
