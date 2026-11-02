# Stage 1 spikes (throwaway, measured)

Results: `results/*.json`, screenshots `results/png/`. Report: `../docs/spike-report.md`.
All binaries accept `--out <file.json>`; renderers also `--items N`, `--frames N`, `--png <dir>`.

```bash
cargo run --release -p render-vello -- --retained --out results/vello_retained.json   # GPU, 100k nodes
cargo run --release -p render-vello -- --cpu --out results/vello_cpu_4k.json          # vello_cpu 4K export
cargo run --release -p render-skia  -- --retained --out results/skia_retained.json    # Skia GL (downloads prebuilt Skia)
cargo run --release -p render-skia  -- --cpu --out results/skia_raster_4k.json
cargo run --release -p text   -- --out results/text.json      # Parley vs cosmic-text
cargo run --release -p layout -- --out results/layout.json    # Taffy, 9.1k nodes
cargo run --release -p script -- --out results/script.json    # rquickjs
cargo run --release -p shell-native -- --auto --novsync --out results/shell_native.json
```

Tauri shell (needs `wasm-pack` and `cargo install tauri-cli`):

```bash
cd shell-tauri/canvas && wasm-pack build --release --target web --out-dir ../dist/pkg --no-typescript --no-pack
cd ../.. && cargo build --release -p shell-tauri
SHELL_TAURI_OUT=results/shell_tauri.json target/release/shell-tauri --auto
```

Other flags: `--no-cull` (Vello crashes at 100k: wgpu buffer binding limit), shell `--items N`.
License check: `cargo deny check licenses`.
