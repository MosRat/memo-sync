#!/usr/bin/env bash
set -Eeuo pipefail

APP_NAME="memo-server"
SERVICE_NAME="memo-server"
DEFAULT_REPO="MosRat/memo-sync"
DEFAULT_BIND="127.0.0.1:7373"
DEFAULT_INSTALL_DIR="/usr/local/bin"
DEFAULT_CONFIG_DIR="/etc/memo-sync"
DEFAULT_DATA_DIR="/var/lib/memo-sync"
DEFAULT_USER="memo-sync"
DEFAULT_GROUP="memo-sync"

ACTION="install"
REPO="$DEFAULT_REPO"
TAG="latest"
BIND="$DEFAULT_BIND"
DATABASE=""
INSTALL_DIR="$DEFAULT_INSTALL_DIR"
CONFIG_DIR="$DEFAULT_CONFIG_DIR"
DATA_DIR="$DEFAULT_DATA_DIR"
RUN_USER="$DEFAULT_USER"
RUN_GROUP="$DEFAULT_GROUP"
BINARY_PATH=""
DOWNLOAD_URL=""
BUILD_FROM_SOURCE="auto"
SYSTEMD_MODE="auto"
DRY_RUN="false"

log() {
  printf '[memo-server] %s\n' "$*"
}

die() {
  printf '[memo-server] error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
Install or manage the Memo Sync server on Linux.

Usage:
  install-memo-server-linux.sh [install|upgrade|uninstall|start|stop|restart|status] [options]

Options:
  --repo OWNER/REPO          GitHub repository for release downloads.
  --tag TAG                 Release tag to install. Use "latest" for latest release.
  --download-url URL        Direct URL for a memo-server Linux archive or binary.
  --binary PATH             Install an already-built memo-server binary.
  --build-from-source       Build from the current repository with cargo.
  --no-build-from-source    Do not build from source when download/binary is missing.
  --bind HOST:PORT          Bind address. Default: 127.0.0.1:7373.
  --database PATH           SQLite database path.
  --install-dir PATH        Binary install directory. Default: /usr/local/bin.
  --config-dir PATH         Environment file directory. Default: /etc/memo-sync.
  --data-dir PATH           Server data directory. Default: /var/lib/memo-sync.
  --user USER               Service user. Default: memo-sync.
  --group GROUP             Service group. Default: memo-sync.
  --no-systemd              Install files only; do not create or manage a systemd unit.
  --dry-run                 Print actions without changing the machine.
  -h, --help                Show this help.

Examples:
  sudo ./scripts/install-memo-server-linux.sh --tag v0.1.0
  sudo ./scripts/install-memo-server-linux.sh --binary ./memo-server --bind 0.0.0.0:7373
  ./scripts/install-memo-server-linux.sh --build-from-source --no-systemd --data-dir "$HOME/.local/share/memo-sync"
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    install|upgrade|uninstall|start|stop|restart|status)
      ACTION="$1"
      shift
      ;;
    --repo)
      REPO="${2:?missing value for --repo}"
      shift 2
      ;;
    --tag)
      TAG="${2:?missing value for --tag}"
      shift 2
      ;;
    --download-url)
      DOWNLOAD_URL="${2:?missing value for --download-url}"
      shift 2
      ;;
    --binary)
      BINARY_PATH="${2:?missing value for --binary}"
      shift 2
      ;;
    --build-from-source)
      BUILD_FROM_SOURCE="yes"
      shift
      ;;
    --no-build-from-source)
      BUILD_FROM_SOURCE="no"
      shift
      ;;
    --bind)
      BIND="${2:?missing value for --bind}"
      shift 2
      ;;
    --database)
      DATABASE="${2:?missing value for --database}"
      shift 2
      ;;
    --install-dir)
      INSTALL_DIR="${2:?missing value for --install-dir}"
      shift 2
      ;;
    --config-dir)
      CONFIG_DIR="${2:?missing value for --config-dir}"
      shift 2
      ;;
    --data-dir)
      DATA_DIR="${2:?missing value for --data-dir}"
      shift 2
      ;;
    --user)
      RUN_USER="${2:?missing value for --user}"
      shift 2
      ;;
    --group)
      RUN_GROUP="${2:?missing value for --group}"
      shift 2
      ;;
    --no-systemd)
      SYSTEMD_MODE="no"
      shift
      ;;
    --dry-run)
      DRY_RUN="true"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

if [[ -z "$DATABASE" ]]; then
  DATABASE="$DATA_DIR/memo-server.sqlite"
fi

BIN_PATH="$INSTALL_DIR/$APP_NAME"
ENV_FILE="$CONFIG_DIR/server.env"
SYSTEMD_UNIT="/etc/systemd/system/$SERVICE_NAME.service"

run() {
  if [[ "$DRY_RUN" == "true" ]]; then
    printf '[dry-run]'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

is_root() {
  [[ "${EUID:-$(id -u)}" -eq 0 ]]
}

has_systemd() {
  [[ "$SYSTEMD_MODE" != "no" ]] \
    && command -v systemctl >/dev/null 2>&1 \
    && [[ -d /run/systemd/system ]]
}

maybe_sudo() {
  if is_root; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    die "this action needs root privileges; re-run as root or install sudo"
  fi
}

run_root() {
  if [[ "$DRY_RUN" == "true" ]]; then
    printf '[dry-run-root]'
    printf ' %q' "$@"
    printf '\n'
  else
    maybe_sudo "$@"
  fi
}

download_file() {
  local url="$1"
  local out="$2"
  if command -v curl >/dev/null 2>&1; then
    run curl -fsSL "$url" -o "$out"
  elif command -v wget >/dev/null 2>&1; then
    run wget -q "$url" -O "$out"
  else
    die "curl or wget is required for downloads"
  fi
}

latest_release_url() {
  printf 'https://github.com/%s/releases/latest/download/memo-server-x86_64-unknown-linux-gnu.tar.gz' "$REPO"
}

tagged_release_url() {
  printf 'https://github.com/%s/releases/download/%s/memo-server-x86_64-unknown-linux-gnu.tar.gz' "$REPO" "$TAG"
}

install_binary_from_archive_or_file() {
  local source="$1"
  local work_dir="$2"
  local candidate=""

  case "$source" in
    *.tar.gz|*.tgz)
      need_cmd tar
      run tar -xzf "$source" -C "$work_dir"
      candidate="$(find "$work_dir" -type f -name "$APP_NAME" -perm -u+x | head -n 1)"
      ;;
    *.gz)
      candidate="$work_dir/$APP_NAME"
      run gzip -dc "$source" > "$candidate"
      run chmod +x "$candidate"
      ;;
    *)
      candidate="$source"
      ;;
  esac

  [[ -n "$candidate" && -f "$candidate" ]] || die "could not find $APP_NAME in downloaded artifact"
  run_root install -m 0755 -D "$candidate" "$BIN_PATH"
}

build_from_source() {
  need_cmd cargo
  [[ -f Cargo.toml ]] || die "source build must be run from the repository root"
  log "building $APP_NAME from source"
  run cargo build -p memo-server --release
  local built="target/release/$APP_NAME"
  [[ -f "$built" ]] || die "build completed but $built was not found"
  run_root install -m 0755 -D "$built" "$BIN_PATH"
}

install_binary() {
  local temp_dir
  temp_dir="$(mktemp -d)"

  run_root mkdir -p "$INSTALL_DIR"
  if [[ "$DRY_RUN" == "true" ]]; then
    log "would install $APP_NAME to $BIN_PATH"
    rm -rf "$temp_dir"
    return
  fi

  if [[ -n "$BINARY_PATH" ]]; then
    [[ -f "$BINARY_PATH" ]] || die "binary not found: $BINARY_PATH"
    run_root install -m 0755 -D "$BINARY_PATH" "$BIN_PATH"
    rm -rf "$temp_dir"
    return
  fi

  if [[ -n "$DOWNLOAD_URL" || "$TAG" != "source" ]]; then
    local url="$DOWNLOAD_URL"
    if [[ -z "$url" ]]; then
      if [[ "$TAG" == "latest" ]]; then
        url="$(latest_release_url)"
      else
        url="$(tagged_release_url)"
      fi
    fi
    local downloaded="$temp_dir/memo-server-artifact"
    case "$url" in
      *.tar.gz|*.tgz) downloaded="$downloaded.tar.gz" ;;
      *.gz) downloaded="$downloaded.gz" ;;
    esac
    log "downloading $url"
    if download_file "$url" "$downloaded"; then
      install_binary_from_archive_or_file "$downloaded" "$temp_dir"
      rm -rf "$temp_dir"
      return
    fi
    if [[ "$BUILD_FROM_SOURCE" == "no" ]]; then
      die "download failed and source build is disabled"
    fi
    log "download failed; falling back to source build"
  fi

  if [[ "$BUILD_FROM_SOURCE" == "no" ]]; then
    die "no binary source was provided"
  fi
  rm -rf "$temp_dir"
  build_from_source
}

write_env_file() {
  run_root mkdir -p "$CONFIG_DIR" "$DATA_DIR"
  if is_root && [[ "$RUN_USER" != "root" ]]; then
    if ! getent group "$RUN_GROUP" >/dev/null 2>&1; then
      run groupadd --system "$RUN_GROUP"
    fi
    if ! id -u "$RUN_USER" >/dev/null 2>&1; then
      run useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin --gid "$RUN_GROUP" "$RUN_USER"
    fi
    run chown -R "$RUN_USER:$RUN_GROUP" "$DATA_DIR"
  fi
  local temp_file
  temp_file="$(mktemp)"
  cat > "$temp_file" <<EOF
MEMO_BIND=$BIND
MEMO_DATABASE=$DATABASE
RUST_LOG=info,tower_http=info
EOF
  run_root install -m 0640 -D "$temp_file" "$ENV_FILE"
  rm -f "$temp_file"
}

write_systemd_unit() {
  local temp_file
  temp_file="$(mktemp)"
  cat > "$temp_file" <<EOF
[Unit]
Description=Memo Sync server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=$ENV_FILE
ExecStart=$BIN_PATH
Restart=on-failure
RestartSec=3
User=$RUN_USER
Group=$RUN_GROUP
WorkingDirectory=$DATA_DIR
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=true
ReadWritePaths=$DATA_DIR

[Install]
WantedBy=multi-user.target
EOF
  run_root install -m 0644 -D "$temp_file" "$SYSTEMD_UNIT"
  rm -f "$temp_file"
  run_root systemctl daemon-reload
  run_root systemctl enable "$SERVICE_NAME"
}

install_service() {
  install_binary
  write_env_file
  if has_systemd; then
    write_systemd_unit
    run_root systemctl restart "$SERVICE_NAME"
    log "installed and started $SERVICE_NAME"
    log "check status with: systemctl status $SERVICE_NAME"
  else
    log "systemd is unavailable or disabled; installed files only"
    log "run manually with: MEMO_BIND=$BIND MEMO_DATABASE=$DATABASE $BIN_PATH"
  fi
}

uninstall_service() {
  if has_systemd && [[ -f "$SYSTEMD_UNIT" ]]; then
    run_root systemctl disable --now "$SERVICE_NAME" || true
    run_root rm -f "$SYSTEMD_UNIT"
    run_root systemctl daemon-reload
  fi
  run_root rm -f "$BIN_PATH"
  log "removed binary and systemd unit; preserved data in $DATA_DIR and config in $CONFIG_DIR"
}

service_action() {
  local verb="$1"
  if has_systemd; then
    run_root systemctl "$verb" "$SERVICE_NAME"
  else
    die "systemd is unavailable; manage the foreground process manually"
  fi
}

case "$ACTION" in
  install|upgrade)
    install_service
    ;;
  uninstall)
    uninstall_service
    ;;
  start|stop|restart|status)
    service_action "$ACTION"
    ;;
  *)
    die "unsupported action: $ACTION"
    ;;
esac
