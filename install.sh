#!/bin/sh
# Intoxicated installer and updater, for FreeBSD on amd64.
#
#   fetch -qo - https://raw.githubusercontent.com/neoppix/intoxicated/freebsd-port/install.sh | sh
#
# Run it as your own user, from a terminal. It checks what the system lacks,
# shows the commands that need root and asks before running them (the password
# goes to sudo, doas or su), clones or updates the checkout, builds it,
# installs the launcher and the desktop entry, and fetches the Roblox build.
# Running it again updates an install; `intoxicated --update` does the same.
# README.md has the same steps done by hand.
#
#   INTOXICATED_REPO   where the checkout goes (default ~/intoxicated)
#   INTOXICATED_YES=1  answer yes to every question
#   PREFIX             where the launcher goes (default /usr/local)
#   INTOXICATED_GIT_URL  clone from somewhere else than GitHub
#
# Nothing runs until main, on the last line, so a download cut short does
# nothing at all.

REPO_URL="${INTOXICATED_GIT_URL:-https://github.com/neoppix/intoxicated}"
BRANCH=freebsd-port
PREFIX="${PREFIX:-/usr/local}"
REPO="${INTOXICATED_REPO:-$HOME/intoxicated}"
APKDIR="${XDG_CACHE_HOME:-$HOME/.cache}/intoxicated/apk"

# Built, run, or loaded at run time by the client and the launcher.
PACKAGES="git rust cmake pkgconf libepoll-shim libX11 libXi libXinerama libXrandr libxkbcommon vulkan-loader jq"
# For roblox.com's Play button: the link handler and the desktop database.
DESKTOP_PACKAGES="xdg-utils desktop-file-utils"

case ":$PATH:" in
  *:/usr/local/bin:*) ;;
  *) PATH="$PATH:/usr/local/sbin:/usr/local/bin" ;;
esac

step() { printf '\n==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }
die() { printf 'intoxicated-install: %s\n' "$*" >&2; exit 1; }

# Questions are read from the terminal: through a pipe, stdin is this script.
has_tty() { ( : </dev/tty ) 2>/dev/null; }

# ask QUESTION DEFAULT: DEFAULT is y or n, taken on an empty answer.
ask() {
  [ "${INTOXICATED_YES:-}" = 1 ] && return 0
  if ! has_tty; then
    printf 'intoxicated-install: no terminal to ask "%s"; INTOXICATED_YES=1 answers yes\n' "$1" >&2
    return 1
  fi
  case $2 in y) hint='[Y/n]' ;; *) hint='[y/N]' ;; esac
  printf '%s %s ' "$1" "$hint" >/dev/tty
  read -r answer </dev/tty || answer=
  case ${answer:-$2} in [Yy]*) return 0 ;; *) return 1 ;; esac
}

# Root commands are gathered into a script, shown, and run in one go, so the
# password is asked for once per batch.
BATCH=""
queue() { BATCH="$BATCH$*
"; }

run_batch() {
  [ -n "$BATCH" ] || return 0
  printf '\nThese commands need root:\n\n'
  printf '%s' "$BATCH" | sed 's/^/    # /'
  printf '\n'
  ask "Run them now?" y || die "nothing was run as root; README.md lists the same steps"
  script=$(mktemp -t intoxicated-install) || die "mktemp failed"
  { printf 'set -e\n'; printf '%s' "$BATCH"; } > "$script"
  if [ "$(id -u)" = 0 ]; then
    /bin/sh "$script"
  elif command -v sudo >/dev/null 2>&1; then
    sudo /bin/sh "$script"
  elif command -v doas >/dev/null 2>&1; then
    doas /bin/sh "$script"
  else
    note "(no sudo or doas: su asks for the root password)"
    su root -c "/bin/sh $script"
  fi
  status=$?
  rm -f "$script"
  BATCH=""
  [ $status = 0 ] || die "a root command failed (status $status); see above"
}

installed() { pkg info -e "$1" >/dev/null 2>&1; }

# Vendor ids of the display controllers, one per line: 10de NVIDIA, 1002 AMD,
# 8086 Intel.
gpu_vendors() {
  pciconf -l 2>/dev/null | awk '/class=0x03/ {
    for (i = 1; i <= NF; i++) if ($i ~ /^vendor=0x/) print substr($i, 10)
  }'
}

icd() { ls "$PREFIX"/share/vulkan/icd.d/$1 >/dev/null 2>&1; }

check_system() {
  [ "$(uname -s)" = FreeBSD ] || die "this installer is for FreeBSD"
  case $(uname -m) in
    amd64) ;;
    *) die "Roblox's engine is x86-64 only; this machine is $(uname -m)" ;;
  esac
  [ "$(id -u)" != 0 ] || die "run this as your own user, not root; it asks for the password when it needs root"
  [ -n "$HOME" ] || die "HOME is not set"
}

check_packages() {
  step "Checking packages"
  if ! pkg -N >/dev/null 2>&1; then
    note "pkg is not set up yet"
    queue "env ASSUME_ALWAYS_YES=yes pkg bootstrap"
    missing=" $PACKAGES $DESKTOP_PACKAGES"
  else
    missing=""
    for p in $PACKAGES $DESKTOP_PACKAGES; do
      installed "$p" || missing="$missing $p"
    done
  fi
  if [ -n "$missing" ]; then
    note "missing:$missing"
    queue "pkg install -y$missing"
  else
    note "all there"
  fi
}

check_gpu() {
  step "Checking the GPU"
  vendors=$(gpu_vendors)
  case $vendors in
    *10de*)
      note "NVIDIA"
      if ! pkg info -g 'nvidia-driver*' >/dev/null 2>&1; then
        note "the NVIDIA driver is missing; nvidia-driver suits GeForce 600 and newer"
        note "(older cards need nvidia-driver-470 or -390: install that one by hand instead)"
        queue "pkg install -y nvidia-driver"
      fi
      if ! kldstat -q -n nvidia-modeset; then
        queue "kldload nvidia-modeset"
        note "X has to run on the nvidia driver; if it did not before, restart X afterwards"
      fi
      case " $(sysrc -n kld_list 2>/dev/null) " in
        *" nvidia-modeset "*) ;;
        *) queue "sysrc kld_list+=nvidia-modeset" ;;
      esac
      ;;
    *1002*|*8086*)
      case $vendors in *1002*) name=AMD kmod=amdgpu json='radeon_icd*.json' ;; *) name=Intel kmod=i915kms json='intel_icd*.json' ;; esac
      note "$name"
      if ! icd "$json"; then
        note "no Vulkan driver for it yet: Mesa and the DRM kernel driver"
        queue "pkg install -y drm-kmod mesa-dri"
      fi
      if ! ls /dev/dri/renderD* >/dev/null 2>&1; then
        case " $(sysrc -n kld_list 2>/dev/null) " in
          *" $kmod "*) ;;
          *) queue "sysrc kld_list+=$kmod" ;;
        esac
        note "the $kmod kernel driver is not loaded; it loads at the next boot"
      fi
      if ! id -Gn | tr ' ' '\n' | grep -qx video; then
        queue "pw groupmod video -m $(id -un)"
        note "you are not in the video group, which the GPU device needs; log in again after"
      fi
      ;;
    *)
      note "no NVIDIA, AMD or Intel GPU found; Intoxicated needs a Vulkan driver"
      ;;
  esac
}

clone() {
  step "Cloning into $REPO"
  git clone --recursive --branch "$BRANCH" "$REPO_URL" "$REPO" || die "git clone failed"
}

checkout() {
  if [ ! -e "$REPO" ]; then
    clone
    return
  fi
  [ -e "$REPO/.git" ] ||
    die "$REPO exists and is not a git checkout; move it, or set INTOXICATED_REPO"
  step "Updating $REPO"
  git -C "$REPO" rev-parse '@{u}' >/dev/null 2>&1 ||
    die "the branch checked out in $REPO follows no remote branch; update it by hand"
  git -C "$REPO" fetch --quiet || die "git fetch failed in $REPO"
  if ! git -C "$REPO" merge-base HEAD '@{u}' >/dev/null 2>&1; then
    old="$REPO.old-$(date +%Y%m%d-%H%M%S)"
    note "$REPO is a clone from before the history restart of 2026-11-23 and cannot update."
    note "Your profile and the Roblox build live outside it and are kept."
    ask "Move it to $old and clone again?" y || die "left $REPO as it is"
    mv "$REPO" "$old" || die "could not move $REPO"
    clone
    return
  fi
  git -C "$REPO" merge --ff-only --quiet '@{u}' ||
    die "local changes or commits in $REPO block the update; commit, stash or drop them and run this again"
  git -C "$REPO" submodule update --init --recursive --quiet || die "git submodule update failed in $REPO"
  note "at $(git -C "$REPO" log -1 --format='%h %s')"
}

# The launcher looks for the checkout at ~/intoxicated; another place is
# written into the installed copy.
launcher_copy() {
  src="$REPO/packaging/freebsd/intoxicated"
  if [ "$REPO" = "$HOME/intoxicated" ]; then
    printf '%s\n' "$src"
    return
  fi
  copy=$(mktemp -t intoxicated-launcher) || die "mktemp failed"
  sed "s|^REPO=\"\${INTOXICATED_REPO:-\$HOME/intoxicated}\"|REPO=\"\${INTOXICATED_REPO:-$REPO}\"|" "$src" > "$copy"
  grep -q "^REPO=\"\${INTOXICATED_REPO:-$REPO}\"" "$copy" || die "could not point the launcher at $REPO"
  chmod 644 "$copy"
  printf '%s\n' "$copy"
}

# wanted MODE SOURCE DESTINATION: add an install line to FILES unless
# DESTINATION already holds SOURCE.
wanted() {
  cmp -s "$2" "$3" || FILES="$FILES
install -m $1 '$2' '$3'"
}

install_files() {
  step "Installing the launcher and the desktop entry"
  launcher=$(launcher_copy)
  FILES=""
  wanted 755 "$launcher" "$PREFIX/bin/intoxicated"
  wanted 644 "$REPO/packaging/freebsd/intoxicated.desktop" "$PREFIX/share/applications/intoxicated.desktop"
  wanted 644 "$REPO/packaging/branding/intoxicated-logo.png" "$PREFIX/share/pixmaps/intoxicated.png"
  if [ -n "$FILES" ]; then
    queue "install -d '$PREFIX/bin' '$PREFIX/share/applications' '$PREFIX/share/pixmaps'$FILES"
    command -v update-desktop-database >/dev/null 2>&1 &&
      queue "update-desktop-database '$PREFIX/share/applications'"
    run_batch
  else
    note "already up to date"
  fi
  [ "$launcher" = "$REPO/packaging/freebsd/intoxicated" ] || rm -f "$launcher"
}

build() {
  step "Building (a first build takes a few minutes)"
  (cd "$REPO" && cargo build --release) || die "the build failed; the output above says why"
}

link_handler() {
  command -v xdg-mime >/dev/null 2>&1 || return 0
  current=$(xdg-mime query default x-scheme-handler/roblox-player 2>/dev/null)
  [ "$current" = intoxicated.desktop ] && return 0
  step "roblox.com's Play button"
  ask "Open roblox-player links (the Play button) in Intoxicated?" y || return 0
  xdg-mime default intoxicated.desktop x-scheme-handler/roblox-player x-scheme-handler/roblox
  note "done"
}

roblox_build() {
  if [ -f "$APKDIR/candidate-0.apk" ] && [ -f "$APKDIR/lib/x86_64/libroblox.so" ]; then
    note "Roblox build already in $APKDIR; intoxicated --update-apk fetches a newer one"
    return 0
  fi
  step "Fetching the Roblox build"
  "$PREFIX/bin/intoxicated" --update-apk || die "could not fetch the Roblox build; intoxicated --update-apk tries again"
}

check_session() {
  [ -e /dev/dsp ] || note "no /dev/dsp: load a sound driver (kldload snd_hda) for sound"
  [ -n "${DISPLAY:-}" ] || note "start Intoxicated from an X11 session (DISPLAY is not set here)"
}

main() {
  check_system
  check_packages
  check_gpu
  run_batch
  checkout
  install_files
  build
  link_handler
  roblox_build
  step "Done"
  check_session
  note "Start it with: intoxicated"
  note "or straight into a game: intoxicated <placeId>, or a roblox.com game link"
  note "Update later with: intoxicated --update"
}

main "$@"
exit
