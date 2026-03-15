# ⚡ Warp

**High-Performance Multi-threaded Download Accelerator for Rust.**

Warp is a sophisticated download manager designed to saturate your bandwidth while remaining respectful of your system's resources. It utilizes a dynamic segmentation strategy with work-stealing to ensure that every CPU core is effectively utilized, even when downloading from servers with varying speeds.

---

## 🚀 Key Features

- **Multi-threaded Acceleration:** Automatically splits files into multiple chunks and downloads them in parallel.
- **Dynamic Resource Balancing:** Continuously monitors system CPU usage and adjusts the number of concurrent workers to maintain system stability.
- **Work-Stealing Architecture:** Large chunks are dynamically split and reassigned to idle workers, ensuring no single thread becomes a bottleneck.
- **Atomic Snapshots (.warp):** Periodically persists download progress to disk. If a download is interrupted, Warp resumes exactly where it left off.
- **Multi-Download Registry:** Manage a persistent list of downloads across terminal sessions.
- **Global Concurrency Control:** A shared semaphore manages the worker pool across multiple concurrent downloads, preventing system exhaustion.

---

## 🛠 Architecture

Warp is built on a modular, asynchronous foundation using **Tokio**:

- **`Engine`**: The high-level orchestrator that manages multiple download managers.
- **`Manager`**: Responsible for a single file download. It pre-allocates disk space, spawns workers, and starts the heartbeat.
- **`Segment`**: Contains the core logic for `Chunk` management and the `download_worker`. Implements the "split-aware" writing logic.
- **`Beat`**: The persistence layer. It captures atomic snapshots of the metadata and serializes them using `bincode`.
- **`Resources`**: A smart utility that uses `sysinfo` to calculate the optimal worker-to-core ratio based on real-time CPU load.
- **`Registry`**: A persistent JSON store (`registry.json`) that tracks your download queue.

---

## 💻 CLI Usage

Warp provides a clean, subcommand-based interface:

### Add a Download
```bash
warp add "https://example.com/large-file.zip" --output "my-file.zip"
```

### List Downloads
```bash
warp list
```
*Displays a table showing IDs, Status (Pending, Downloading, Completed, Error), Target Path, and URL.*

### Run Pending Downloads
```bash
warp run
```
*Starts the engine to process all incomplete downloads in the registry concurrently.*

### Remove a Download
```bash
warp remove <ID>
```

---

## 🔧 Installation

### Local Build
1. Ensure you have the [Rust toolchain](https://rustup.rs/) installed.
2. Clone the repository and build:
   ```bash
   cargo build --release
   ```

### Global Installation (Add to PATH)
To use `warp` from anywhere in your terminal, install it locally:
```bash
cargo install --path .
```
*This places the binary in your `.cargo/bin` directory, which is typically already in your system PATH.*


---

## 🧪 Testing

Warp is built with a "Test-Heavy" philosophy. Every core component (Splitting, Snapshotting, Resource Calculation) is covered by comprehensive unit tests.

Run the test suite:
```bash
cargo test
```

---

## 📜 Technical Details

### Work-Stealing Logic
When a worker finishes its assigned `Chunk`, the `Manager` looks for the largest remaining chunk in the queue. If that chunk is larger than `10MB` (the `MIN_SPLIT_SIZE`), it is split in half. The original worker continues with the first half, and a new worker is spawned (if a slot is available) to steal the second half.

### Resilience
The `.warp` snapshot file is updated atomically every second. Warp writes to a temporary file and uses an atomic `rename` operation to ensure that even a power failure during a write won't corrupt your progress.
