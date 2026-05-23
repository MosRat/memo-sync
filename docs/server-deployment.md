# Memo Server Linux Deployment

`memo-server` is the lightweight Rust sync backend. It stores metadata and the append-only sync log in SQLite WAL mode. Large attachment blobs can be relayed through memory for mobile-to-mobile handoff; the server does not need to become a permanent object store.

## Release Artifact

Release builds include:

```text
memo-server-x86_64-unknown-linux-gnu.tar.gz
```

This artifact targets common glibc Linux distributions on x86_64, such as Ubuntu, Debian, Fedora, Rocky, AlmaLinux, and recent openSUSE installs.

For Alpine or other musl-only systems, build from source on the target machine or use a container image based on a glibc distribution.

## One-Command Install

On a normal systemd VPS:

```bash
curl -fsSL https://raw.githubusercontent.com/MosRat/memo-sync/main/scripts/install-memo-server-linux.sh \
  | sudo bash -s -- install --tag v0.1.0 --bind 0.0.0.0:7373
```

For the latest GitHub release:

```bash
curl -fsSL https://raw.githubusercontent.com/MosRat/memo-sync/main/scripts/install-memo-server-linux.sh \
  | sudo bash -s -- install --tag latest
```

The installer creates:

- Binary: `/usr/local/bin/memo-server`
- Env file: `/etc/memo-sync/server.env`
- Data directory: `/var/lib/memo-sync`
- systemd unit: `/etc/systemd/system/memo-server.service`
- Service user/group: `memo-sync`

Default environment:

```env
MEMO_BIND=127.0.0.1:7373
MEMO_DATABASE=/var/lib/memo-sync/memo-server.sqlite
RUST_LOG=info,tower_http=info
```

## Firewall and Reverse Proxy

For a private LAN server, bind directly to the LAN interface or all interfaces:

```bash
sudo ./scripts/install-memo-server-linux.sh install --bind 0.0.0.0:7373
```

For an internet-facing server, prefer a reverse proxy with TLS and keep `memo-server` on localhost:

```bash
sudo ./scripts/install-memo-server-linux.sh install --bind 127.0.0.1:7373
```

Minimal Nginx proxy shape:

```nginx
location / {
  proxy_pass http://127.0.0.1:7373;
  proxy_http_version 1.1;
  proxy_set_header Host $host;
  proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
  proxy_set_header X-Forwarded-Proto $scheme;
  proxy_set_header Upgrade $http_upgrade;
  proxy_set_header Connection "upgrade";
}
```

## Operations

Check service status:

```bash
sudo ./scripts/install-memo-server-linux.sh status
curl http://127.0.0.1:7373/health
```

Restart:

```bash
sudo ./scripts/install-memo-server-linux.sh restart
```

Upgrade to a tag:

```bash
sudo ./scripts/install-memo-server-linux.sh upgrade --tag v0.1.1
```

Uninstall service and binary while preserving data:

```bash
sudo ./scripts/install-memo-server-linux.sh uninstall
```

The uninstall action keeps `/var/lib/memo-sync` and `/etc/memo-sync` so accidental service removal does not delete sync data.

## Local Binary Install

If you copied a binary onto the server:

```bash
sudo ./scripts/install-memo-server-linux.sh install --binary ./memo-server --bind 0.0.0.0:7373
```

If you copied the release archive:

```bash
sudo ./scripts/install-memo-server-linux.sh install \
  --download-url "file:///tmp/memo-server-x86_64-unknown-linux-gnu.tar.gz"
```

For fully offline installs, extract the archive first and use `--binary`.

## Source Build Install

From a checked-out repository:

```bash
sudo ./scripts/install-memo-server-linux.sh install --build-from-source
```

Without root or systemd:

```bash
./scripts/install-memo-server-linux.sh install \
  --build-from-source \
  --no-systemd \
  --install-dir "$HOME/.local/bin" \
  --config-dir "$HOME/.config/memo-sync" \
  --data-dir "$HOME/.local/share/memo-sync"
```

Then run manually:

```bash
MEMO_BIND=127.0.0.1:7373 \
MEMO_DATABASE="$HOME/.local/share/memo-sync/memo-server.sqlite" \
"$HOME/.local/bin/memo-server"
```

## Backup

Before moving or upgrading a busy server, stop the service and copy the SQLite files together:

```bash
sudo systemctl stop memo-server
sudo tar -C /var/lib -czf memo-sync-backup.tgz memo-sync
sudo systemctl start memo-server
```

SQLite WAL creates sidecar files such as `memo-server.sqlite-wal` and `memo-server.sqlite-shm`. Back up the whole data directory, not just the main `.sqlite` file.

## Health Response

`GET /health` returns values useful for deployment probes and dashboards:

- `ok`
- `server_sequence`
- `min_available_sequence`
- `protocol_version`
- `attachment_count`
- `attachment_blob_count`
- `attachment_blob_bytes`
- `relay_blob_count`
- `relay_blob_bytes`
- `relay_device_count`

The endpoint also clears expired in-memory relay blobs before reporting relay metrics.
