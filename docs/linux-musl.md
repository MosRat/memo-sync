# Linux musl Build and Test

The preferred production server artifact is a musl-linked Linux binary built with `cargo-zigbuild`.

## Windows Cross Compile

```powershell
rustup target add x86_64-unknown-linux-musl
cargo zigbuild -p memo-server --release --target x86_64-unknown-linux-musl
```

This avoids depending on a full Linux build container for normal release work.

## WSL Tests

```powershell
wsl.exe -d Ubuntu -- bash -lc "cd /mnt/f/WorkSpace/Rust/memo-sync && CARGO_TARGET_DIR=/tmp/memo-sync-target cargo test -p memo-core -p memo-server"
```

Using a Linux-side `CARGO_TARGET_DIR` avoids occasional DrvFs permission warnings when Windows and WSL both touch `target/`.

If WSL does not have Rust installed yet:

```bash
curl https://sh.rustup.rs -sSf | sh
. "$HOME/.cargo/env"
rustup target add x86_64-unknown-linux-gnu
```

## Runtime

```bash
./target/x86_64-unknown-linux-musl/release/memo-server --bind 0.0.0.0:7373 --database /var/lib/memo-sync/memo.sqlite
```

The server uses Tokio, Axum, Rustls, SQLite WAL, bounded pools, and `mimalloc` on Linux.

Quick health check from WSL:

```bash
target/x86_64-unknown-linux-musl/release/memo-server --bind 127.0.0.1:17373 --database /tmp/memo-sync.sqlite &
curl -fsS http://127.0.0.1:17373/health
```
