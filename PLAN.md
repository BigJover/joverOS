# joverOS Feature Roadmap

## M6 — Process Management
The foundation everything else builds on. Pure shell commands, no new frontend.

**Commands:**
- `kill chrome` / `force quit photoshop` — terminate by name
- `what's eating my CPU` / `top processes` — show CPU/memory hogs
- `what's using port 3000` — lsof lookup
- `set minecraft to high priority` — nice/renice

**Tech:** extend `diagnose_slow` logic into an action layer; add `kill_process`, `set_priority`, `port_lookup` tauri commands.

---

## M7 — Game Mode
Builds on M6. One command that does what Razer Cortex does, but via natural language.

**Commands:**
- `game mode on` — kills background hogs (Discord, Slack, OneDrive, etc.), boosts CPU to performance, disables notifications
- `game mode off` — restores everything that was running
- `game mode minecraft` — per-game profile (different kill list, settings)
- `save game profile minecraft` — let user define what "minecraft mode" kills/keeps

**Tech:** saved profiles in SQLite (game_profiles table); snapshot running apps before killing so restore is exact; renice the game process.

---

## M8 — Media Controls
Self-contained, high daily-driver value. macOS = AppleScript; Linux = playerctl.

**Commands:**
- `pause` / `play` / `next` / `previous`
- `skip back 30` — rewind 30 seconds
- `what's playing` — show current track + app
- `shuffle on` / `shuffle off`

**Tech:** detect active media app (Spotify, Music, YouTube in browser) then route the AppleScript/playerctl command to the right target.

---

## M9 — Window Management
Pairs naturally with workflows. macOS = System Events/AppleScript; Linux = wmctrl/xdotool.

**Commands:**
- `snap safari left` / `snap code right` — side by side
- `fullscreen spotify`
- `focus notes`
- `hide all` / `show all`
- `move safari to second monitor`

**Tech:** AppleScript for macOS window bounds; wmctrl for Linux. Build a `window_layout` tauri command that takes app name + position verb.

---

## M10 — Productivity Pack
Several small self-contained features. Ship as one milestone.

**Commands:**
- `paste history` / `what did I copy before` — clipboard history (last 20 items)
- `how many GB is 2.4 TB` / `250 USD to EUR` / `98F to C` — math + unit + currency
- `remind me in 20 minutes` / `timer 5 minutes` — local countdown, bar pops up when done
- `expand addr` → pastes your full address — text snippets

**Tech:** clipboard polling loop on startup; math/convert via local Rust parser (no API needed for units; currency needs a free exchange rate API); timers via tokio::time; snippets table in SQLite.

---

## M11 — Storage & Paths
Makes the bar the interface for disk management.

**Commands:**
- `move downloads to external drive` — repoints ~/Downloads via symlink, apps don't break
- `set screenshots to ~/Desktop/shots` — same pattern
- `eject backup drive` — unmount by label
- `disk health` — SMART status + temperature
- `what's on my external drives` — list mounted volumes

**Tech:** symlink swap for path redirection; diskutil/smartmontools for health; NSWorkspace for eject on macOS; udisksctl on Linux.

---

## M12 — Intelligence Layer
The hardest, most impactful milestone. Makes it feel like an OS, not a launcher.

**Commands (proactive, no user trigger):**
- Bar surfaces: "You open Figma + Spotify + Notion every morning — save as Work Mode?"
- Bar surfaces: "Disk is at 91% — want me to find what's biggest?"
- Bar surfaces: "CPU has been pegged for 8 min — Discord is the cause. Kill it?"

**Commands (user-triggered):**
- `what do I usually open on Mondays`
- `run my morning routine`
- `every Sunday clear downloads older than 2 weeks` — scheduled jobs

**Tech:**
- Usage log already exists (history table) — mine it for patterns
- Pattern detector runs on a background interval (weekly summary → detect repeated sequences)
- Proactive alerts hook into existing diagnose commands, surface via bar notification
- Scheduled jobs: cron-style table in SQLite + tokio background task checks on launch

---

## Parking Lot (future, not scheduled yet)
- AirPlay / cast target switching
- Default app reassignment per file type
- GPU profile switching (for discrete GPU Macs)
- Linux distro shell-takeover (M5b — original plan)
- Game/mod manager (Modrinth, CurseForge, version-pinned profiles)
- Own games shipped with the OS
