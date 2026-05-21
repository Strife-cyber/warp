# Warp Upgrade Audit

After a complete review of the codebase, here is the honest assessment: Warp has a solid architecture for segmented HTTP downloading, but it is not practically useful in its current state. The core problem is **scope sprawl** — it tries to be three things at once (download manager, packet sniffer, TUI+GUI app) and does none of them well enough to be worth reaching for.

Below is the diagnosis and a prioritized upgrade path.

---

## Critical Problems

### 1. The Network Interceptor Is a Dead End

The `capture` feature (pcap + pnet) is the biggest misinvestment. It consumes dependency weight, complicates the build, and creates a second entire feature set that doesn't serve the core purpose.

**Why it doesn't work:**
- Raw packet capture cannot parse HTTPS traffic (the vast majority of modern web). TLS encryption means you see TCP packets with encrypted payloads — no HTTP method, no URL, no meaningful data.
- HTTP/2 and HTTP/3 are framed binary protocols, not parseable by the text-based `parse_http_request` function in `parser.rs`.
- Npcap is Windows-only, requires admin privileges, and is a manual install outside of Rust's dependency management.
- The interceptor is passive — it can't modify or redirect requests. It's a packet *sniffer*, not an interceptor.
- The TUI/GUI interceptor tabs are wired to fake demo data, not real captures.

**Recommendation:** Remove the interceptor module entirely. If network monitoring is genuinely needed, it should be a separate project. For download management, the equivalent value comes from clipboard monitoring and browser extension integration — not packet capture.

### 2. The TUI and GUI Are Duplicated Maintenance

Both `ratatui` (TUI) and `egui` (GUI) frontends exist, consuming `Cargo.toml` dependencies and duplicating layout logic. Neither is polished:

- The TUI has an interceptor tab with a hard-coded "Press 's' to start" that injects a single fake request.
- The GUI has redundant Npcap warning dialogs.
- Neither exposes priority, speed limits, proxy, or checksum fields that already exist in `DownloadEntry`.
- Neither has file-open-after-complete, failure-reason inspection, or download history.

**Recommendation:** Drop one. Keep the TUI (lighter, ratatui + crossterm pair well, no GPU dependencies). Move the GUI to an optional behind-a-feature-flag if someone maintains it.

### 3. The Downloader Loses to curl, wget, and aria2

The core download engine is well-structured but doesn't offer enough over existing tools to be worth the install:

**Missing basics:**
- **Queue management**: No drag-reorder, no bulk-add, no scheduled start times.
- **Speed scheduling**: Fields like `max_speed_bytes` exist on `DownloadEntry` but are never exposed in the CLI or UI.
- **Completion actions**: No notification, no "open file", no shutdown-after-complete, no webhook.
- **M3U8/HLS support**: `m3u8-rs` is in `Cargo.toml` but never imported. Streaming media download is a key differentiator for a Rust download manager.
- **Cookie/header management**: Can't add custom headers or cookies per download.
- **Mirror/fallback URLs**: aria2 supports multiple sources per file. Warp doesn't.
- **Torrent/magnet**: Not expected, but worth noting.
- **Resume negotiation**: If the server doesn't support Range requests, the download fails instead of falling back to a single-stream full download.

### 4. The Binary Is Heavy for What It Does

Dependencies pulled in: `eframe/egui` (OpenGL), `pcap/pnet` (raw sockets), `ratatui/crossterm`, `reqwest` with `rustls`, `indicatif`, `scraper` (html parsing — imported but only used in one place?). Build time and binary size are high for a download manager.

---

## Upgrade Roadmap

### Phase 1 — Trim Scope (High Impact, Low Effort)

| Task | Why |
|---|---|
| Delete the interceptor module | Removes ~700 lines of fragile, non-functional code and pcap/pnet deps |
| Drop one UI (keep TUI) | Removes eframe/egui dependency tree, halves UI maintenance |
| Remove `scraper` dep | Not meaningfully used anywhere |
| Remove `toml` dep | Only used for `.warp` inspection display; serde_json covers it |
| Feature-gate `gui` behind `--features gui` | Lets users choose |

**Result:** Cargo.toml drops from ~30 deps to ~18. Build time drops substantially. The user-facing feature set becomes coherent (it's a download manager).

### Phase 2 — Make the Downloader Worth Reaching For (High Impact, Medium Effort)

#### 2a. Expose Hidden Fields in CLI

The `DownloadEntry` struct already has `priority`, `proxy`, `checksum`, and `max_speed_bytes`. Expose them:

```bash
warp add <url> --priority 5 --speed-limit 2M --proxy http://proxy:8080
```

Also add metadata in the registry for headers, cookies, and output directory.

#### 2b. Add M3U8 / HLS Download

The `m3u8-rs` crate is already a dependency — use it. An HLS downloader that:
1. Fetches the master playlist
2. Parses variant streams
3. Downloads segments in parallel with the existing chunk/worker architecture
4. Merges them into a single `.ts` or remuxes to `.mp4`

This is **the single biggest feature that makes Warp distinct** from curl/wget.

#### 2c. Completion Actions

- Desktop notification via `notify-rust` (or `platform-notifications` on Windows)
- Webhook POST on completion (I use this for automation)
- `--exec` flag to run a command on completion: `warp add <url> --exec "open %path"`

#### 2d. Clipboard Monitoring (Basic)

Watch the clipboard for URLs and prompt to add them. Simple with `arboard` crate, cross-platform, and 10x more useful than packet capture.

### Phase 3 — Polish (Medium Impact, Medium Effort)

#### 3a. Configuration File

A `warp.toml` config file (in project config dir, already used for registry):
```toml
default_directory = "~/Downloads"
max_concurrent_downloads = 3
default_speed_limit = "5M"
completion_notification = true
completion_command = "open %path"
theme = "dark"
```

#### 3b. TUI Polish

- **Sortable columns** (status, size, speed, ETA)
- **Batch select** (mark multiple downloads, pause/resume/remove all marked)
- **Inline error inspection** (select a failed download, press `e` to see the full error)
- **Search/filter** (match by filename or URL)
- **ETA column** in the table view
- **Compact mode** for terminals

#### 3c. Graceful Shutdown & Persistence

Currently, killing the TUI with `Ctrl+C` drops all in-flight work. The cancellation tokens exist but there's no save-and-exit flow. Add `SIGINT`/`SIGTERM` handling that saves snapshots for all active downloads before exiting.

### Phase 4 — Scale (Lower Impact, Higher Effort)

- **BitTorrent support** via `torrust` or similar (complete download manager offering)
- **HTTP/3 (QUIC) support** via reqwest's quic feature
- **Remote control** (HTTP API server so `warp` can run as a daemon and you add downloads from a browser or mobile)
- **Browser extension** (right-click → "Download with Warp", requires the daemon API from above)
- **Plugin system** for post-download hooks (unzip, convert, tag)

---

## Quick Wins (Do This Week)

These are isolated, high-value changes that don't require the full refactor:

1. **Just delete `src/interceptor/`** and its re-exports. Also remove `pcap` and `pnet` from `Cargo.toml` and the `capture` feature. No functionality you'd actually use is lost.

2. **Wire up `max_speed_bytes`** — it already flows through `Manager::new()` → `download_worker()` → throttling logic. Just add a `--speed-limit` flag to the CLI `Add` subcommand and a speed-limit input to the TUI's download-add dialog.

3. **Hook up `m3u8-rs`** — add a `warp m3u8 <url>` subcommand that downloads all segments. The worker pool and concurrent chunk logic already exist.

4. **Remove unused deps**: `scraper`, `toml`. Each saves compile time.

5. **Add `--output-dir`** to set a default download directory, so you don't have to type `--output` every time.

---

## What Not to Do

- **Don't "fix" the packet interceptor.** No amount of HTTP parsing will decrypt TLS. If you need a network monitor, use Wireshark or mitmproxy.
- **Don't keep both UIs equally polished.** Pick one and go deep.
- **Don't abstract the UI code prematurely.** The backend (`UiBackend`) → frontend channel pattern is fine. Don't over-engineer until features are proven.
- **Don't add torrent support until Phase 4.** It's a separate protocol and a massive dependency.

---

## Current Code Quality Notes

The code that exists is **genuinely well-written**:
- Clean module boundaries, good use of `Arc`/`Mutex`/`CancellationToken`.
- The work-stealing chunk split in `segment.rs` is clever and correct.
- The snapshot/resume heartbeat is robust (atomic rename).
- Tests exist for core logic and are meaningful.
- Resource-aware worker scaling is a nice touch.

The problem is not the code quality — it's that the project tries to do too much and finishes nothing to a useful level. Cutting scope to a focused, polished download manager with HLS support as a differentiator would make this genuinely useful.
