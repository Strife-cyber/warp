# Warp Upgrade Audit

After a complete review of the codebase, here is the honest assessment: Warp has a solid architecture for segmented HTTP downloading, but it is not practically useful in its current state. The core problem is **scope sprawl** — it tries to be three things at once (download manager, packet sniffer, TUI+GUI app) and does none of them well enough to be worth reaching for.

The changes below have been partially applied (see the "Applied" markers). Remaining items are future work.

---

## Critical Problems

### 1. The Network Interceptor Is a Dead End

**✅ APPLIED — Module deleted.** The entire `src/interceptor/` directory, `pcap`/`pnet` dependencies, and `capture` feature flag have been removed. ~700 lines of fragile packet-sniffing code gone.

The `capture` feature (pcap + pnet) was the biggest misinvestment. It consumed dependency weight, complicated the build, and created a second entire feature set that didn't serve the core purpose. Raw packet capture cannot parse HTTPS traffic, HTTP/2/3 are binary protocols, Npcap is Windows-only, and the interceptor was passive — it could never modify or redirect requests.

### 2. The TUI and GUI Are Duplicated Maintenance

**✅ APPLIED — GUI removed.** The `egui`/`eframe` GUI (and its OpenGL dependency tree) has been removed. `ratatui` + `crossterm` TUI is the only frontend. The interceptor tab that showed fake demo data has been removed from the TUI.

### 3. The Downloader Loses to curl, wget, and aria2

**🔄 PARTIALLY APPLIED.** Key progress:
- Speed limits (`--speed-limit`), proxy, checksum, and priority flags are now exposed on `warp add`
- `m3u8-rs` is now wired up — `warp m3u8 <url>` downloads HLS streams

**Still missing (future work):**
- Queue management (drag-reorder, scheduled start)
- Completion actions (notifications, webhooks, `--exec`)
- Cookie/header management
- Mirror/fallback URLs
- Resume negotiation fallback

### 4. The Binary Is Heavy for What It Does

**✅ APPLIED — 30 deps → ~18.** Removed: `pcap`, `pnet`, `eframe`, `egui`, `scraper`, `toml`, `regex`. Removed features: `json`, `blocking`, `cookies` from reqwest.

---

## Upgrade Roadmap

### Phase 1 — Trim Scope ✅

| Task | Status |
|---|---|
| Delete the interceptor module | ✅ Done |
| Drop one UI (keep TUI) | ✅ Done |
| Remove `scraper` dep | ✅ Done |
| Remove `toml` dep | ✅ Done (using `serde_json` for snapshot inspection) |
| Flatten module structure | ✅ Done (all `downloader/` modules moved to `src/` root) |

### Phase 2 — Make the Downloader Worth Reaching For

#### 2a. Expose Hidden Fields in CLI ✅

```bash
warp add <url> --priority 5 --speed-limit 2M --proxy http://proxy:8080 --checksum abc123...
```

#### 2b. Add M3U8 / HLS Download ✅

```bash
warp m3u8 "https://example.com/stream.m3u8" -o video.ts --quality best --concurrent 8
```

Supports master playlists (auto-selects variant), media playlists, parallel segment download, and concatenation.

#### 2c. Completion Actions ❌

Still needed:
- Desktop notification
- Webhook POST on completion
- `--exec` flag: `warp add <url> --exec "open %path"`

#### 2d. Clipboard Monitoring ❌

Watch clipboard for URLs. Simple with `arboard` crate.

### Phase 3 — Polish (Future Work)

- Configuration file (`warp.toml` in config dir)
- TUI: sortable columns, batch select, inline error inspection, search, ETA column
- Graceful shutdown (SIGINT saves snapshots before exit)

### Phase 4 — Scale (Future Work)

- BitTorrent, HTTP/3, HTTP API daemon, browser extension, plugin system

---

## Quick Wins (Applied)

1. **✅ Interceptor deleted** — 700 lines, pcap/pnet deps, capture feature flag
2. **✅ Speed limits wired** — `--speed-limit` flag on `warp add`, throttling in download worker
3. **✅ M3U8/HLS hooked up** — `warp m3u8 <url>` subcommand downloads streams
4. **✅ Unused deps removed** — scraper, toml, pcap, pnet, eframe, egui, regex
5. **✅ Module structure flattened** — 13 files at `src/` root, no nested packages

---

## What Not to Do

- **Don't "fix" the packet interceptor.** No amount of HTTP parsing will decrypt TLS. If you need a network monitor, use Wireshark or mitmproxy.
- **Don't keep both UIs equally polished.** TUI only now. Go deep on it.
- **Don't abstract the UI code prematurely.** The backend (`UiBackend`) → frontend channel pattern is fine.
- **Don't add torrent support until Phase 4.** It's a separate protocol and a massive dependency.

---

## Current Code Quality Notes

The code that exists is **genuinely well-written**:
- Clean module boundaries, good use of `Arc`/`Mutex`/`CancellationToken`.
- The work-stealing chunk split in `segment.rs` is clever and correct.
- The snapshot/resume heartbeat is robust (atomic rename).
- Tests exist for core logic and are meaningful (19 pass).
- Resource-aware worker scaling is a nice touch.
