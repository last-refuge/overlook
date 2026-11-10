#!/usr/bin/env bash
# Overlook installer. A clear view of what matters.
#
#   curl -fsSL https://raw.githubusercontent.com/last-refuge/overlook/main/install.sh | bash
#
# Arch Linux and Omarchy: builds a real package from source and installs it with pacman, so
# pacman can update and remove it. Any other Linux: builds from source into ~/.local with no
# administrator access. Read it first if you like; it is short and does nothing you cannot see.
#
#   --source        build from source even if a prebuilt release is available
#   --prefix DIR    install into DIR without administrator access (default for non-Arch: ~/.local)
#   --ref REF       git branch or tag to build from source (default: main)
#   --yes           do not ask for confirmation
#   --dry-run       show what would happen and change nothing
#   --uninstall     remove Overlook
#   --help          this text
set -Eeuo pipefail

main() {

REPO="last-refuge/overlook"
CLONE_URL="https://github.com/$REPO.git"
PKGNAME="overlook"
API_LATEST="https://api.github.com/repos/$REPO/releases/latest"

# ---------------------------------------------------------------------------------------------
# Presentation
# ---------------------------------------------------------------------------------------------
if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
  TTY=1
  if [[ "${COLORTERM:-}" == *truecolor* || "${COLORTERM:-}" == *24bit* ]]; then
    ACCENT=$'\033[38;2;54;95;115m'; AMBER=$'\033[38;2;255;157;50m'; PINE=$'\033[38;2;83;110;99m'
  else
    ACCENT=$'\033[36m'; AMBER=$'\033[33m'; PINE=$'\033[32m'
  fi
  BOLD=$'\033[1m'; DIM=$'\033[2m'; RESET=$'\033[0m'; HIDE=$'\033[?25l'; SHOW=$'\033[?25h'
else
  TTY=0; ACCENT=''; AMBER=''; PINE=''; BOLD=''; DIM=''; RESET=''; HIDE=''; SHOW=''
fi

banner() {
  printf '\n  %s%sOverlook%s  %s·  A clear view of what matters.%s\n\n' "$ACCENT" "$BOLD" "$RESET" "$DIM" "$RESET"
}
ok()   { printf '  %s✔%s  %s\n' "$PINE" "$RESET" "$*"; }
note() { printf '  %s·%s  %s%s%s\n' "$DIM" "$RESET" "$DIM" "$*" "$RESET"; }
warn() { printf '  %s!%s  %s\n' "$AMBER" "$RESET" "$*"; }
die()  { printf '\n  %s✘%s  %s\n' "$AMBER" "$RESET" "$*" >&2; [[ -n "${LOG:-}" && -s "${LOG:-}" ]] && printf '     %sFull log: %s%s\n' "$DIM" "$LOG" "$RESET" >&2; printf '\n' >&2; exit 1; }

cleanup() { printf '%s' "$SHOW"; [[ -n "${WORK:-}" && -d "$WORK" && "${KEEP_WORK:-0}" == 0 ]] && rm -rf "$WORK"; return 0; }
trap cleanup EXIT
trap 'die "Interrupted."' INT TERM

# Under `curl | bash` this script arrives on stdin, so no command may inherit it.
if ( : </dev/tty ) 2>/dev/null; then TTYIN=/dev/tty; else TTYIN=/dev/null; fi

step() {
  local msg=$1; shift
  if (( DRY )); then note "would: $msg"; return 0; fi
  if (( ! TTY )); then
    printf '  …  %s\n' "$msg"
    "$@" </dev/null >>"$LOG" 2>&1 || { tail -n 15 "$LOG" | sed 's/^/     /' >&2; die "$msg failed."; }
    ok "$msg"
    return 0
  fi
  "$@" </dev/null >>"$LOG" 2>&1 &
  local pid=$! i=0 frames='⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'
  printf '%s' "$HIDE"
  while kill -0 "$pid" 2>/dev/null; do
    printf '\r  %s%s%s  %s' "$ACCENT" "${frames:i++%${#frames}:1}" "$RESET" "$msg"
    sleep 0.1
  done
  printf '%s\r\033[K' "$SHOW"
  if wait "$pid"; then ok "$msg"; else printf '\r\033[K'; tail -n 15 "$LOG" | sed 's/^/     /' >&2; die "$msg failed."; fi
}

usage() {
  cat <<'USAGE'
Overlook installer. A clear view of what matters.

  curl -fsSL https://raw.githubusercontent.com/last-refuge/overlook/main/install.sh | bash

Arch Linux and Omarchy: builds a package from source and installs it with pacman.
Any other Linux: builds from source into ~/.local, no administrator access needed.

  --source        build from source even if a prebuilt release is available
  --prefix DIR    install into DIR without administrator access
  --ref REF       git branch or tag to build from source (default: main)
  --yes           do not ask for confirmation
  --dry-run       show what would happen and change nothing
  --uninstall     remove Overlook
  --help          this text
USAGE
}

# ---------------------------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------------------------
PREFIX=""; REF="main"; ASSUME_YES=0; DRY=0; UNINSTALL=0; FORCE_SOURCE=0
while (( $# )); do
  case "$1" in
    --source) FORCE_SOURCE=1 ;;
    --prefix) shift; [[ $# -gt 0 ]] || die "--prefix needs a directory."; PREFIX=$1 ;;
    --prefix=*) PREFIX=${1#--prefix=} ;;
    --ref) shift; [[ $# -gt 0 ]] || die "--ref needs a branch or tag."; REF=$1 ;;
    --ref=*) REF=${1#--ref=} ;;
    --yes|-y) ASSUME_YES=1 ;;
    --dry-run) DRY=1 ;;
    --uninstall) UNINSTALL=1 ;;
    --help|-h) usage; exit 0 ;;
    *) die "Unknown option $1 (try --help)." ;;
  esac
  shift
done
WORK=$(mktemp -d "${TMPDIR:-/tmp}/overlook-install.XXXXXX")
LOG="$WORK/install.log"

# ---------------------------------------------------------------------------------------------
# What kind of machine is this?
# ---------------------------------------------------------------------------------------------
IS_ARCH=0; IS_OMARCHY=0; DISTRO="this Linux system"
if [[ -r /etc/os-release ]]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  DISTRO=${PRETTY_NAME:-${NAME:-$DISTRO}}
  [[ "${ID:-}" == arch || "${ID_LIKE:-}" == *arch* ]] && IS_ARCH=1
fi
if [[ -d /usr/share/omarchy ]] || command -v omarchy >/dev/null 2>&1; then IS_OMARCHY=1; IS_ARCH=1; fi
[[ -n "$PREFIX" ]] && IS_ARCH=0   # an explicit prefix is always a plain user install

confirm() {
  (( ASSUME_YES )) && return 0
  [[ -r /dev/tty ]] || return 0
  local reply; printf '  %s?%s  %s [Y/n] ' "$ACCENT" "$RESET" "$1" >/dev/tty; read -r reply </dev/tty || true
  [[ -z "$reply" || "$reply" =~ ^[Yy] ]]
}

# ---------------------------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------------------------
if (( UNINSTALL )); then
  banner
  # One kept running in the background (or started at login) would outlive its program.
  # Only a version that knows --quit is asked: an older one would start instead.
  if command -v overlook >/dev/null 2>&1 && overlook --help 2>/dev/null | grep -q -- '--quit'; then
    step "Quit the running Overlook" overlook --quit
  fi
  AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/ai.lastrefuge.Overlook.desktop"
  if [[ -e $AUTOSTART ]]; then step "Stop starting Overlook at login" rm -f "$AUTOSTART"; fi
  if (( IS_ARCH )) && pacman -Qi "$PKGNAME" >/dev/null 2>&1; then
    note "Removing the package needs administrator access."
    step "Remove the overlook package" sudo pacman -Rns --noconfirm "$PKGNAME"
  else
    P=${PREFIX:-$HOME/.local}
    step "Remove Overlook from $P" rm -f "$P/bin/overlook" "$P/share/applications/ai.lastrefuge.Overlook.desktop" \
      "$P/share/metainfo/ai.lastrefuge.Overlook.metainfo.xml" "$P/share/icons/hicolor/scalable/apps/ai.lastrefuge.Overlook.svg" \
      "$P"/share/icons/hicolor/*/apps/ai.lastrefuge.Overlook.png
  fi
  note "Your mail, events and settings in ~/.local/share/overlook were left alone."
  note "Sign-in tokens stay in your keyring until you remove the account inside Overlook, or delete them with your keyring tool."
  ok "Overlook removed."; printf '\n'
  exit 0
fi

# ---------------------------------------------------------------------------------------------
# Plan
# ---------------------------------------------------------------------------------------------
banner
# A prebuilt release is used when one exists for this machine; otherwise we build.
ASSET_URL=""; ASSET_NAME=""; SUMS_URL=""; LATEST_TAG=""
if (( ! FORCE_SOURCE )) && [[ "$(uname -m)" == x86_64 ]]; then
  if json=$(curl -fsSL -H 'Accept: application/vnd.github+json' "$API_LATEST" 2>/dev/null); then
    LATEST_TAG=$(printf '%s' "$json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
    if (( IS_ARCH )); then want='x86_64.pkg.tar.zst'; else want='x86_64-linux.tar.gz'; fi
    ASSET_URL=$(printf '%s' "$json" | grep -o '"browser_download_url": *"[^"]*' | sed 's/.*"//' | grep -- "$want" | grep -v -- "-debug-" | head -n1 || true)
    SUMS_URL=$(printf '%s' "$json" | grep -o '"browser_download_url": *"[^"]*' | sed 's/.*"//' | grep -- '/SHA256SUMS$' | head -n1 || true)
    ASSET_NAME=${ASSET_URL##*/}
  fi
fi
if (( IS_ARCH )); then
  if (( IS_OMARCHY )) && [[ "$DISTRO" != *marchy* ]]; then DISTRO="$DISTRO with Omarchy"; fi
  if [[ -n "$ASSET_URL" ]]; then
    note "$DISTRO: installing the prebuilt package for Overlook ${LATEST_TAG#v} with pacman."
  else
    note "$DISTRO: building a package from source and installing it with pacman."
  fi
  note "You may be asked for your password for pacman only."
else
  P=${PREFIX:-$HOME/.local}
  if [[ -n "$ASSET_URL" ]]; then
    note "$DISTRO: installing the prebuilt Overlook ${LATEST_TAG#v} into $P (no administrator access needed)."
  else
    note "$DISTRO: building from source into $P (no administrator access needed)."
  fi
fi
if (( ! IS_ARCH )) && ! command -v chromium >/dev/null 2>&1 && ! command -v chromium-browser >/dev/null 2>&1 && ! command -v google-chrome >/dev/null 2>&1; then
  note "Message bodies require Chromium (installed separately on non-Arch systems). Set OVERLOOK_BROWSER_BIN to a compatible executable if it is not on PATH."
fi
(( DRY )) && note "Dry run: nothing will be changed."
confirm "Continue?" || { note "Nothing changed."; exit 0; }

# ---------------------------------------------------------------------------------------------
# Build requirements
# ---------------------------------------------------------------------------------------------
if [[ -z "$ASSET_URL" ]]; then
need_cmds=(git cargo)
missing=()
for c in "${need_cmds[@]}"; do command -v "$c" >/dev/null 2>&1 || missing+=("$c"); done
if (( IS_ARCH )); then
  pkgs=()
  command -v git >/dev/null 2>&1 || pkgs+=(git)
  command -v cargo >/dev/null 2>&1 || pkgs+=(rustup)
  command -v makepkg >/dev/null 2>&1 || pkgs+=(base-devel)
  for p in vulkan-icd-loader libxkbcommon libxkbcommon-x11 libxcb wayland fontconfig; do pacman -Qi "$p" >/dev/null 2>&1 || pkgs+=("$p"); done
  if (( ${#pkgs[@]} )); then
    note "Installing build requirements: ${pkgs[*]}"
    step "Install build requirements" sudo pacman -S --needed --noconfirm "${pkgs[@]}"
  fi
  if ! rustup show active-toolchain >/dev/null 2>&1; then
    step "Install the stable Rust toolchain" rustup default stable
  fi
elif (( ${#missing[@]} )); then
  die "Missing: ${missing[*]}. Install Rust from https://rustup.rs and git, plus development packages for wayland, libxkbcommon, fontconfig, freetype and a Vulkan driver, then run this again."
fi
fi

# ---------------------------------------------------------------------------------------------
# Fetch: a verified prebuilt release, or the source
# ---------------------------------------------------------------------------------------------
SRC="$WORK/src"
fetch_release_asset() {
  local dest="$WORK/$ASSET_NAME"
  curl -fsSL -o "$dest" "$ASSET_URL" || return 1
  [[ -n "$SUMS_URL" ]] || { echo "no SHA256SUMS in the release" >&2; return 1; }
  local sums expected actual
  sums=$(curl -fsSL "$SUMS_URL") || return 1
  expected=$(printf '%s\n' "$sums" | awk -v n="$ASSET_NAME" '$2==n || $2=="*"n {print tolower($1)}' | head -n1)
  actual=$(sha256sum "$dest" | awk '{print tolower($1)}')
  [[ -n "$expected" && "$expected" == "$actual" ]] || { echo "checksum mismatch for $ASSET_NAME" >&2; rm -f "$dest"; return 1; }
}
if [[ -n "$ASSET_URL" ]]; then
  if (( DRY )); then
    note "would: Download $ASSET_NAME and verify its checksum"
  elif ! fetch_release_asset </dev/null >>"$LOG" 2>&1; then
    warn "The prebuilt release could not be downloaded or verified; building from source instead."
    ASSET_URL=""; ASSET_NAME=""
  else
    ok "Downloaded $ASSET_NAME and verified its checksum"
  fi
fi
if [[ -z "$ASSET_URL" ]]; then
  step "Fetch Overlook ($REF)" git clone --depth 1 --branch "$REF" "$CLONE_URL" "$SRC"
fi

# ---------------------------------------------------------------------------------------------
# Build and install
# ---------------------------------------------------------------------------------------------
if (( IS_ARCH )) && [[ -n "$ASSET_URL" ]]; then
  note "Installing the package needs administrator access."
  step "Install with pacman" sudo pacman -U --noconfirm "$WORK/$ASSET_NAME"
  BIN=/usr/bin/overlook
elif [[ -n "$ASSET_URL" ]]; then
  P=${PREFIX:-$HOME/.local}
  step "Install into $P" bash -c '
    set -e; TAR=$0; P=$1; X=$(mktemp -d)
    tar -xzf "$TAR" -C "$X"; D=$(find "$X" -mindepth 1 -maxdepth 1 -type d | head -n1)
    install -Dm755 "$D/overlook" "$P/bin/overlook"
    install -Dm644 "$D/packaging/ai.lastrefuge.Overlook.desktop" "$P/share/applications/ai.lastrefuge.Overlook.desktop"
    install -Dm644 "$D/packaging/ai.lastrefuge.Overlook.metainfo.xml" "$P/share/metainfo/ai.lastrefuge.Overlook.metainfo.xml"
    install -Dm644 "$D/icons/overlook-app-icon.svg" "$P/share/icons/hicolor/scalable/apps/ai.lastrefuge.Overlook.svg"
    for s in 16 24 32 48 64 128 256 512; do install -Dm644 "$D/icons/overlook-icon-$s.png" "$P/share/icons/hicolor/${s}x${s}/apps/ai.lastrefuge.Overlook.png"; done
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$P/share/applications" || true
    rm -rf "$X"
  ' "$WORK/$ASSET_NAME" "$P"
  BIN="$P/bin/overlook"
  case ":$PATH:" in *":$P/bin:"*) ;; *) warn "$P/bin is not on your PATH. Add it to your shell profile to run \`overlook\` from a terminal." ;; esac
elif (( IS_ARCH )); then
  PKGDIR="$WORK/pkg"
  if (( ! DRY )); then
    mkdir -p "$PKGDIR"
    cp "$SRC/packaging/PKGBUILD" "$PKGDIR/PKGBUILD"
  fi
  # Build from the checkout we just fetched instead of cloning again inside makepkg.
  step "Build the package (this takes a few minutes the first time)" env OVERLOOK_SRC="$SRC" bash -c 'cd "$0" && makepkg -f --noconfirm' "$PKGDIR"
  PKGFILE=""
  (( DRY )) || PKGFILE=$(ls -t "$PKGDIR"/*.pkg.tar.* 2>/dev/null | head -n1)
  (( DRY )) || [[ -n "$PKGFILE" ]] || die "makepkg produced no package."
  note "Installing the package needs administrator access."
  step "Install with pacman" sudo pacman -U --noconfirm "$PKGFILE"
  BIN=/usr/bin/overlook
else
  P=${PREFIX:-$HOME/.local}
  step "Build Overlook (this takes a few minutes the first time)" bash -c 'cd "$0" && cargo build --release --locked -p overlook' "$SRC"
  step "Install into $P" bash -c '
    set -e; SRC=$0; P=$1
    install -Dm755 "$SRC/target/release/overlook" "$P/bin/overlook"
    install -Dm644 "$SRC/packaging/ai.lastrefuge.Overlook.desktop" "$P/share/applications/ai.lastrefuge.Overlook.desktop"
    install -Dm644 "$SRC/packaging/ai.lastrefuge.Overlook.metainfo.xml" "$P/share/metainfo/ai.lastrefuge.Overlook.metainfo.xml"
    install -Dm644 "$SRC/branding/assets/overlook-app-icon.svg" "$P/share/icons/hicolor/scalable/apps/ai.lastrefuge.Overlook.svg"
    for s in 16 24 32 48 64 128 256 512; do install -Dm644 "$SRC/branding/assets/overlook-icon-$s.png" "$P/share/icons/hicolor/${s}x${s}/apps/ai.lastrefuge.Overlook.png"; done
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$P/share/applications" || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q "$P/share/icons/hicolor" || true
  ' "$SRC" "$P"
  BIN="$P/bin/overlook"
  case ":$PATH:" in *":$P/bin:"*) ;; *) warn "$P/bin is not on your PATH. Add it to your shell profile to run \`overlook\` from a terminal." ;; esac
fi

# ---------------------------------------------------------------------------------------------
# Check and finish
# ---------------------------------------------------------------------------------------------
if (( ! DRY )); then
  [[ -x "$BIN" ]] || die "The overlook binary was not installed at $BIN."
fi
printf '\n'
ok "Overlook is installed."
if (( IS_OMARCHY )); then
  note "Press Super+Space and type \"Overlook\", or run \`overlook\`."
else
  note "Find Overlook in your application menu, or run \`overlook\`."
fi
printf '\n  %sConnect Gmail%s\n' "$BOLD" "$RESET"
note "Overlook starts with a sample mailbox. To add your Gmail account you need a Google OAuth client"
note "(type \"Desktop app\") saved as ~/.config/overlook/google-oauth-client.json. The README has the"
note "five-minute walkthrough; then use the account menu at the bottom of the sidebar → Add Gmail account."
printf '\n'

}

main "$@"
