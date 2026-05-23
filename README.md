# Memo Sync

[![CI](https://github.com/MosRat/memo-sync/actions/workflows/ci.yml/badge.svg)](https://github.com/MosRat/memo-sync/actions/workflows/ci.yml)
[![Release](https://github.com/MosRat/memo-sync/actions/workflows/release.yml/badge.svg)](https://github.com/MosRat/memo-sync/actions/workflows/release.yml)

Memo Sync is a Tauri 2 multi-platform memo tool with a React web client and a Rust sync server.

## Shape

- `memo-core`: shared data model, filters, hybrid logical clock, and deterministic operation merge logic.
- `memo-server`: Axum + Tokio sync service backed by SQLite WAL and an append-only operation log.
- `memo-desktop`: Tauri 2 client with tray, global shortcuts, clipboard capture, local SQLite storage, and a self-drawn unified title bar.
- `apps/desktop/src`: React/TypeScript UI that can run as both desktop frontend and web client shell.

## Development

```powershell
pnpm install --dir apps/desktop
cargo test --workspace
pnpm --dir apps/desktop dev
cargo tauri dev --config apps/desktop/src-tauri/tauri.conf.json
```

## Server

```powershell
cargo run -p memo-server -- --bind 127.0.0.1:7373 --database memo-server.sqlite
```

Linux servers can use the one-command installer. It installs the release binary, writes an environment file, and creates a systemd service when systemd is available:

```bash
curl -fsSL https://raw.githubusercontent.com/MosRat/memo-sync/main/scripts/install-memo-server-linux.sh \
  | sudo bash -s -- install --tag latest --bind 127.0.0.1:7373
```

See [docs/server-deployment.md](docs/server-deployment.md) for systemd, non-root, offline, reverse-proxy, upgrade, uninstall, and backup notes.

For static Linux builds from Windows, use `cargo-zigbuild`:

```powershell
rustup target add x86_64-unknown-linux-musl
cargo install cargo-zigbuild
cargo zigbuild -p memo-server --release --target x86_64-unknown-linux-musl
```

The binary is written to:

```text
target/x86_64-unknown-linux-musl/release/memo-server
```

## Linux/WSL Verification

```powershell
wsl.exe -d Ubuntu -- bash -lc "cd /mnt/f/WorkSpace/Rust/memo-sync && CARGO_TARGET_DIR=/tmp/memo-sync-target cargo test -p memo-core -p memo-server"
wsl.exe -d Ubuntu -- bash -lc "cd /mnt/f/WorkSpace/Rust/memo-sync && cargo run -p memo-server -- --bind 127.0.0.1:7373"
```

Use `cargo zigbuild` for the deployable musl artifact and WSL for Linux runtime behavior checks.

Temporary repositories are local scratch spaces: their memos are purged on next app startup and never enter the outgoing sync log.

## CI and Release

GitHub Actions runs a fast validation pass on every push and pull request to `main`:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --no-deps -- -D warnings
cargo clippy -p memo-render --no-default-features --all-targets --no-deps -- -D warnings
pnpm --dir apps/desktop test
```

Run the `CI` workflow manually with the `full` profile when you need full Rust tests, the production frontend build, and an aarch64 Android APK with the lightweight mobile renderer profile.

Create a prerelease from the Actions UI by running the `Release` workflow with a tag such as `v0.1.0`. Pushing a `v*` tag also triggers the same workflow:

```powershell
git tag v0.1.0
git push origin v0.1.0
```

Release artifacts include Windows installer packages, a signed Android APK, a Linux `memo-server` archive, and `SHA256SUMS.txt`.
