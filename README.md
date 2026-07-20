# joverOS

The agent-first OS. A natural-language bar that launches apps, opens and finds
web destinations, and (soon) searches and organizes files — built as a
cross-platform Tauri app that will become the desktop shell of a custom Linux
distro. See CLAUDE.md for the full spec and milestone ladder.

## Dev

Requires [Ollama](https://ollama.com) running with `llama3.1:8b` pulled
(the bar reports "brain offline" without it).

```
npm install
npm run tauri dev
```

Summon the bar with **Option+Space**.
