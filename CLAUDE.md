# Project: joverOS

## What this is

A long-term project to build an operating system whose primary interface is a natural-language
AI agent — a single text bar on the desktop that can launch apps, find and organize files,
troubleshoot problems, restore saved workflows, and eventually manage games and mods.
The goal is an OS usable by anyone, regardless of technical confidence: the agent replaces
the terminal, the settings maze, and the "call a tech-savvy relative" step.

Inspiration: TempleOS (one person can build an OS), SteamOS/Android (Linux underneath,
unrecognizable on top), Raycast/Spotlight (summonable bar) — but agentic: it *does* things,
not just finds them.

## Route decision (settled)

**Linux-based, not from-scratch.** A from-scratch kernel can never run real games or real
apps (no GPU drivers, no JVM, no Proton). Building on Linux inherits all drivers, Steam +
Proton (real Minecraft works), browsers, and 30 years of hardening. The OS's identity lives
in the shell and agent, not the kernel — same as Android.

**Sequence:**
1. Build the AI bar as a **cross-platform desktop app** (macOS + Linux). Developed and
   daily-driven on the user's Mac. This app IS the future OS shell — nothing is throwaway.
2. Test the Linux side in a **UTM virtual machine** on the Mac (free).
3. Later: the app becomes the desktop shell of a custom Linux distro (base TBD: Arch vs.
   Fedora Atomic vs. NixOS — leaning immutable/atomic for unbreakability). Boot straight
   into the bar. Terminal exists but hidden.
4. Possible future test hardware: user's old Surface Pro (currently not booting).

## Architecture

- **Input bar** — global-hotkey-summonable text field, always available.
- **Intent router** — local LLM (Ollama) classifies input into structured intents
  (app_launch, file_search, file_organize, troubleshoot, workflow, web_search, ...).
  Structured output, never freeform prose driving execution.
- **Capability plugins** — one module per intent domain, containing real deterministic
  code. The LLM decides *what*, plugins decide *how*. Pluggable so an ecosystem can
  form later (plugin vetting/sandboxing is a known open problem — permissions granted
  to the agent must not be inheritable by arbitrary plugins).
- **Confirmation layer** — see Permission model below.
- **Agent memory** — persistent local knowledge base: saved workflows, file habits,
  hardware profile, past problems and fixes.
- **History/undo log** — every change the agent makes is recorded and reversible where
  possible. This is a core trust feature, not an afterthought.

## Product principles (settled in discussion — treat as requirements)

1. **Terse agent output.** No chatty back-and-forth. Responses are at most a short
   paragraph plus relevant file paths ("technician leaving a note, not a chatbot").
2. **Permission-first.** v1 asks before every change and before reading user files.
   Later: scoped grants (e.g., always-read Documents, always-ask before delete, never
   touch system files without explicit confirmation) and an opt-in "trusted mode" for
   users who want silent fixes. Ask-every-time is the launch default.
3. **Offline-first.** Local LLM via Ollama (8–13B class). Online capabilities (web
   search, mod downloads) are additive, not required. No API-key dependency for core use.
4. **For everyone, eventually.** v1 does not pick a niche audience — it picks a
   universal feature set (see V1 scope). Power-user features follow.
5. **Unbreakable.** Nothing the agent or user does through normal use should be able to
   brick the system. (Full expression of this comes with the immutable distro base.)

## V1 scope (settled): the universal core

Features every computer user needs:
- **App launching** by natural language ("open obs", "launch minecraft").
- **File search & organization** ("find that invoice from March", "organize my downloads
  by type") — local file index + LLM interpretation of fuzzy requests.
- **Troubleshooting** — scoped to a fixed set of domains with real diagnostic code:
  wifi/network, slow computer, audio, file-won't-open. Agent reads logs, checks disk
  space, tests connectivity; reports findings in plain language; fixes only with permission.

**Fast-follow (v1.x): Workflows.** Saved sessions — user types "workflow 1" and their
editing software opens with their project, browser windows, music, window layout.
Workflows must be *recordable* ("save my current setup as workflow 1"), not just manually
configured. Known limitation: state restoration depth depends on each app's own session
handling (launch app + file + URLs is achievable; mid-edit state is the app's job).

**Later: game/mod management.** Example target interaction: "launch minecraft 1.14.6 with
optifine and a minimap mod" → resolve mods via Modrinth/CurseForge APIs, check version
compatibility, install to correct profile, launch. Confirmation before any download.

**Later: own games.** Small games written for/with the OS, shipped with the distro.

## Tech stack (proposed, confirm at kickoff)

- **App shell:** Tauri (Rust backend, web frontend) — small binaries, cross-platform,
  native performance. Electron + TypeScript is the fallback if Tauri friction is high.
- **Local LLM:** Ollama as the model runtime; model selected for intent routing quality
  vs. hardware floor (needs testing — llama3.1:8b class as starting point). Design for
  a smaller-model fallback on weak hardware; optional explicit-opt-in cloud fallback.
- **Agent memory / index:** SQLite locally.
- **Dev/test Linux:** UTM VM on macOS.

## Milestone ladder

- **M0 — Bar exists.** Tauri app, global hotkey summons a bar, it can launch a macOS app
  by fuzzy name match. No LLM yet.
- **M1 — Brain attached.** Ollama integration, intent router with structured output,
  routes between app_launch / file_search / unknown. Terse response panel.
- **M2 — Universal core.** File search + organization plugin with permission prompts and
  undo log. First troubleshooting domain (pick one: disk space / "why is my computer slow").
- **M3 — Full v1.** All four troubleshooting domains, history view, scoped permissions.
- **M4 — Workflows.** Record and replay sessions.
- **M5 — Linux.** Same app running in the UTM VM; begin the shell-takeover path
  (bar as the session's primary interface); distro base evaluation.

## Open questions (not yet decided — raise when relevant)

- Open-source or not.
- Distro base: Arch vs. Fedora Atomic vs. NixOS.
- Results UI details: how the bar expands, progress for long jobs, notification behavior.
- Plugin security/vetting model.
- Minimum hardware spec / small-model fallback strategy.

## How to work on this project

- Start at M0. Do not scaffold ahead of the current milestone.
- Keep the agent's own output style consistent with product principle 1 (terse) —
  including in prototypes.
- Every destructive operation goes through the confirmation layer from the very first
  implementation. No "add permissions later."
- The user (Jovan) is building this to learn as well as to ship: explain significant
  architectural choices briefly as they come up.
