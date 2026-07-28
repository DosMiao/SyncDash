#!/usr/bin/env bash
#
# SyncDash Builder — macOS port of builder.bat. Double-click it in Finder, or run
# ./builder.command.
#
# The digit items line up with builder.bat one for one: the same number does the
# same thing. [A] and [V] are macOS-only letter keys with no .bat counterpart —
# nothing on macOS installs a bundle for you, so [A] does the drag-to-Applications
# step itself.
#
# Node policy: dist/ is a committed artifact (Tauri embeds it at compile time), so
# [2] / [3] / [4] are pure cargo and need no Node. [1] Dev and [A] App DO need it —
# tauri runs beforeBuildCommand (`npm run build`) on both paths.
#
# Platform analogs vs the .bat:
#   - process kill ....... pkill -f by FULL PATH   (vs taskkill /T by exe name)
#   - port free .......... lsof -ti tcp:PORT       (vs Get-NetTCPConnection)
#   - "[6] Kill + unlock exe" -> "[6] Kill + installed": macOS has no exe write
#     lock (cargo relinks over a running binary happily), so :wait_unlocked has
#     no analog. What actually runs behind your back here is the copy in
#     /Applications, which the target-dir sweep cannot match.
set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REL="${PROJECT_DIR}/target/release"
DBG="${PROJECT_DIR}/target/debug"
APP="${REL}/syncdash-desktop"
CLI="${REL}/syncdash"
BUNDLE="${REL}/bundle"
VITE_PORT="5173"
# Must equal src-tauri/tauri.conf.json `identifier`. install_to_applications
# refuses to replace a bundle whose Info.plist reports anything else, so a drift
# here turns every reinstall into a hard error.
BUNDLE_ID="com.dosmiao.syncdash"
# Finder-launched shells do not inherit cargo's bin dir.
export PATH="$HOME/.cargo/bin:$PATH"

# ---- ANSI colors (plain if not a tty) ----
# Dim is CDM (not CD) to mirror builder.bat, where %CD% collides with cmd's
# dynamic current-directory variable.
if [ -t 1 ]; then
  C0=$'\033[0m';  CB=$'\033[96m'; CT=$'\033[1;93m'; CG=$'\033[92m'
  CY=$'\033[93m'; CR=$'\033[91m'; CW=$'\033[97m';   CDM=$'\033[90m'
else
  C0= CB= CT= CG= CY= CR= CW= CDM=
fi

# ---- Clickable (cmd+click) link to a built artifact; plain path if not a tty ----
# OSC-8 hyperlink (Terminal.app on Big Sur+ and iTerm2). macOS paths are already
# absolute POSIX, so the URI is simply file://<abspath>. \033 = ESC, \033\\ = ST.
print_link() {
  local p="$1"
  if [ -t 1 ]; then
    printf '  \033]8;;file://%s\033\\>> %s\033]8;;\033\\\n' "$p" "$p"
  else
    printf '  >> %s\n' "$p"
  fi
}

# ---- Timing helpers (BSD `date`; locale-independent, second-resolution) --------
PT0=0; TT0=0; EL_FMT=""
phase_start() { PT0=$(date +%s); printf '\n  %s[%s]%s  %sSTART%s  %s%s%s\n' "$CDM" "$(date +%H:%M:%S)" "$C0" "$CG" "$C0" "$CW" "$1" "$C0"; }
phase_end()   { local el=$(( $(date +%s) - PT0 )); EL_FMT="$(printf '%dm %02ds' $((el/60)) $((el%60)))"; printf '  %s[%s]%s  %sEND%s    %s  %s| elapsed%s %s%s%s\n' "$CDM" "$(date +%H:%M:%S)" "$C0" "$CB" "$C0" "$1" "$CDM" "$C0" "$CW" "$EL_FMT" "$C0"; }
total_start() { TT0=$(date +%s); }
total_end()   { local el=$(( $(date +%s) - TT0 )); printf '\n  %s[%s]%s  %sTOTAL%s  %selapsed%s %s%dm %02ds%s\n' "$CDM" "$(date +%H:%M:%S)" "$C0" "$CT" "$C0" "$CDM" "$C0" "$CW" $((el/60)) $((el%60)) "$C0"; }

# ---- Artifact size as "12.3" MB (one decimal, matching the .bat's :size_mb) ----
size_mb() {
  local b
  b="$(stat -f%z "$1" 2>/dev/null)" || { echo "?"; return 0; }
  printf '%d.%d' $((b / 1048576)) $(( (b % 1048576) * 10 / 1048576 ))
}

fail_build() {
  echo
  echo "  ${CR}===== BUILD FAILED =====${C0}"
  total_end
}

# ---- Prerequisites -------------------------------------------------------------
# Only [1] and [A] reach here — everything else builds from the committed dist/.
# node_modules existing is not the same as node_modules being current: pulling a
# commit that adds a dependency leaves the directory in place but incomplete, and
# the build then fails a long way from the cause. npm install touches
# node_modules, so comparing mtimes is enough to notice.
need_node() {
  command -v npm >/dev/null 2>&1 || {
    echo
    echo "  ${CR}npm not found.${C0} Install Node: https://nodejs.org"
    return 1
  }
  if [ ! -d "${PROJECT_DIR}/node_modules" ]; then
    echo
    echo "  ${CY}[Setup]${C0} First build, running npm install ..."
    ( cd "${PROJECT_DIR}" && npm install ) || return 1
  elif [ "${PROJECT_DIR}/package.json" -nt "${PROJECT_DIR}/node_modules" ]; then
    echo
    echo "  ${CY}[Setup]${C0} Dependencies changed, running npm install ..."
    ( cd "${PROJECT_DIR}" && npm install ) || return 1
  fi
  [ -d "${PROJECT_DIR}/node_modules" ] || return 1
  return 0
}

need_cargo() {
  command -v cargo >/dev/null 2>&1 || {
    echo
    echo "  ${CR}cargo not found.${C0} Install Rust: https://rustup.rs"
    return 1
  }
  return 0
}

# dist/ is committed on purpose: Tauri embeds it at compile time and the pure
# cargo tiers never regenerate it.
need_dist() {
  [ -d "${PROJECT_DIR}/dist" ] && return 0
  echo
  echo "  ${CR}dist/ is missing.${C0} It is a committed artifact — run ${CW}npm run build${C0}"
  echo "  once, commit dist/, then pull. ([1] and [A] regenerate it via tauri.)"
  return 1
}

# ---- Kill the desktop shell (silently) ------------------------------------------
# The GUI shell is a viewer over the library: it holds no lock of its own and
# relaunches in a second, so it goes without asking. Matched by FULL PATH — the
# short process name is kernel-truncated at 16 bytes, which "syncdash-desktop"
# exactly hits, so name matching is not reliable here.
free_desktop() {
  echo
  echo "  ${CY}[Kill]${C0} Freeing syncdash-desktop ..."
  local hit=0
  pkill -f "${APP}"                  >/dev/null 2>&1 && hit=1
  pkill -f "${DBG}/syncdash-desktop" >/dev/null 2>&1 && hit=1
  # The tauri dev supervisor, anchored to THIS checkout's node_modules. A bare
  # `pkill -f "tauri dev"` would take down another project's dev server — three
  # Tauri apps share this machine.
  pkill -f "${PROJECT_DIR}/node_modules/.*tauri" >/dev/null 2>&1 && hit=1
  if [ "${hit}" -eq 1 ]; then
    sleep 1
    echo "    Freed syncdash-desktop + children"
  else
    echo "    Nothing running"
  fi
}

# ---- Kill the CLI (only after asking) -------------------------------------------
# The CLI is not a shell: a live syncdash may be halfway through an apply, holding
# the root heartbeat lock and writing files. Killing that behind the user's back
# is not a build step's business, so this one asks, and a timeout leaves it
# running — the build failing is the cheaper of the two outcomes. (CLAUDE.md:45.)
# pgrep -x matches the process name EXACTLY, so it never catches syncdash-desktop.
free_cli() {
  local pids ans
  pids="$(pgrep -x syncdash 2>/dev/null)"
  [ -z "${pids}" ] && return 0

  echo
  local p
  for p in ${pids}; do
    echo "    syncdash  PID ${p}  since $(ps -o lstart= -p "${p}" 2>/dev/null | sed 's/^ *//')"
  done
  echo "  ${CY}[Warn]${C0} A syncdash is running — it may be mid-apply."
  ans=""
  read -t 10 -n 1 -r -p "   Kill it? [y/N]  (default = N in 10s): " ans
  echo
  case "${ans}" in
    [Yy])
      kill -9 ${pids} >/dev/null 2>&1
      sleep 1
      echo "    Killed syncdash"
      ;;
    *)
      echo "    Left running — it holds no lock cargo needs on macOS, but an apply is still in flight."
      ;;
  esac
  return 0
}

# ---- Free the Vite port --------------------------------------------------------
# Vite runs strictPort, so a dev server left behind by a closed terminal does not
# shift to 5174 — it refuses to start at all.
free_port() {
  local pids
  pids="$(lsof -ti "tcp:${VITE_PORT}" 2>/dev/null)"
  if [ -n "${pids}" ]; then
    kill -9 ${pids} >/dev/null 2>&1
    echo "    Freed port ${VITE_PORT}"
  else
    echo "    Port ${VITE_PORT} free"
  fi
}

# ---- Quit the copy installed in /Applications ----------------------------------
free_installed() {
  local dst="/Applications/SyncDash.app"
  if [ ! -d "${dst}" ]; then
    echo "    No installed copy in /Applications"
    return 0
  fi
  if pkill -f "${dst}/" >/dev/null 2>&1; then
    sleep 1
    echo "    Quit the installed /Applications copy"
  else
    echo "    Installed copy not running"
  fi
}

# ---- Install a freshly built .app into /Applications ---------------------------
# ditto rather than cp -R: it is the platform copier and keeps the bundle's
# extended attributes and symlink layout intact. The destination is REMOVED first
# because ditto merges into an existing directory, and a file dropped between
# builds would otherwise survive forever inside the installed copy.
APP_INSTALLED=""
install_to_applications() {
  local src="$1" name dst have_id
  name="$(basename "$src")"
  dst="/Applications/${name}"
  APP_INSTALLED=""

  if [ ! -w /Applications ]; then
    echo "  ${CR}[Install] ERROR: /Applications is not writable by this account.${C0}"
    return 1
  fi

  if [ -e "$dst" ]; then
    # Never rm -rf something that is not ours: an occupied destination is only
    # replaceable when its Info.plist carries our identifier. PlistBuddy reads
    # the file directly, where `defaults read` would answer from a cache.
    have_id="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "${dst}/Contents/Info.plist" 2>/dev/null)"
    if [ "$have_id" != "${BUNDLE_ID}" ]; then
      echo "  ${CR}[Install] ERROR: ${dst} exists and is not ${BUNDLE_ID}${C0}"
      echo "  ${CDM}(found \"${have_id:-no readable Info.plist}\") — move it aside by hand, then retry.${C0}"
      return 1
    fi
    if pkill -f "${dst}/" >/dev/null 2>&1; then
      echo "  ${CY}[Install]${C0} Quitting the installed copy ..."
      sleep 1
    fi
    rm -rf "$dst" || { echo "  ${CR}[Install] ERROR: could not remove ${dst}${C0}"; return 1; }
  fi

  ditto "$src" "$dst" || { echo "  ${CR}[Install] ERROR: ditto failed -> ${dst}${C0}"; return 1; }
  APP_INSTALLED="$dst"
  echo "  ${CG}[Install]${C0} ${CW}${name}${C0} -> ${CW}/Applications${C0}"
  print_link "$dst"
  return 0
}

# ---- Build steps (mirror the .bat's :step_desktop / :step_cli) -------------------
# A bare `cargo build --release` builds the CLI only — the root package IS the
# CLI, so -p syncdash-desktop is required and forgetting it leaves a stale GUI
# binary that looks like a code bug. (CLAUDE.md:42.)
step_desktop() {
  phase_start "$1 CARGO - syncdash-desktop, release"
  ( cd "${PROJECT_DIR}" && cargo build --release -p syncdash-desktop ) || return 1
  phase_end "$1 CARGO - syncdash-desktop, release"
  return 0
}

step_cli() {
  phase_start "$1 CARGO - syncdash, release"
  ( cd "${PROJECT_DIR}" && cargo build --release -p syncdash ) || return 1
  phase_end "$1 CARGO - syncdash, release"
  return 0
}

report() {
  local artifact="$1" headline="$2"
  if [ ! -x "${artifact}" ]; then
    echo "  ${CR}ERROR: cargo reported success but $(basename "${artifact}") is missing${C0}"
    return 1
  fi
  echo
  echo "  ${CG}===== ${headline} =====${C0}  ${CDM}$(size_mb "${artifact}") MB${C0}"
  print_link "${artifact}"
  return 0
}

launch_app() {
  [ -x "${APP}" ] || return 0
  echo
  echo "  ${CG}[Run]${C0} Launching ${CW}syncdash-desktop${C0} ..."
  # macOS analog of the .bat's `start "" <exe>`: detach via a background subshell
  # so the binary reparents to launchd and survives this window exiting.
  ( "${APP}" >/dev/null 2>&1 </dev/null & )
}

# ---- [A] Self-use .app: build, install into /Applications, launch the installed --
# The bare binary above has no Dock icon and no bundle identity; this runs the
# tauri bundler, which embeds the frontend and emits a double-click .app
# (Info.plist, id com.dosmiao.syncdash). Needs Node — tauri runs
# beforeBuildCommand (`npm run build`) as part of the bundle.
build_app_self() {
  total_start
  phase_start "SELF-USE APP - tauri build (.app)"
  ( cd "${PROJECT_DIR}" && npx tauri build --bundles app ) || { fail_build; return 1; }
  phase_end "SELF-USE APP - tauri build (.app)"

  local app
  app="$(ls -dt "${BUNDLE}/macos/"*.app 2>/dev/null | head -n1)"
  if [ -z "${app}" ]; then
    echo
    echo "  ${CR}===== APP BUILD FAILED: no .app under ${BUNDLE}/macos =====${C0}"
    total_end; return 1
  fi
  echo
  echo "  ${CG}===== APP BUILD SUCCESS =====${C0}"
  echo "  ${CW}built${C0}"; print_link "${app}"
  echo
  install_to_applications "${app}" || { total_end; return 1; }

  # Launch the INSTALLED copy, never the one still sitting in the target dir —
  # otherwise the process you are using is the build artifact and /Applications
  # holds a cold duplicate.
  echo
  echo "  ${CG}[Run]${C0} Launching ${CW}$(basename "${APP_INSTALLED}")${C0} from /Applications ..."
  open "${APP_INSTALLED}"
  total_end
  return 0
}

reveal_bundle() {
  if [ -d "${BUNDLE}/macos" ]; then
    echo "  ${CG}[Reveal]${C0} Opening ${BUNDLE}/macos ..."
    open "${BUNDLE}/macos"
  else
    echo "  ${CY}[Reveal]${C0} No bundle yet — build one with ${CW}[A]${C0} first."
    print_link "${BUNDLE}"
  fi
}

# ---- [7] Clean: one target/ for the whole workspace ------------------------------
# dist/ is deliberately left alone: it is a committed artifact, and the pure cargo
# tiers have no way to regenerate it.
clean_all() {
  free_desktop
  free_cli
  echo
  echo "  ${CY}[Clean]${C0} cargo clean over the workspace, dist/ untouched ..."
  cargo clean --manifest-path "${PROJECT_DIR}/Cargo.toml" \
    && echo "    target/ cleaned" || echo "    ${CR}Clean FAILED${C0}"
}

# ---- Menu ----
echo
echo "  ${CB}=================================================================${C0}"
echo "  ${CB}    ${CT}S Y N C D A S H    B U I L D E R${C0}"
echo "  ${CB}=================================================================${C0}"
echo "  ${CDM}${PROJECT_DIR}${C0}"
echo
echo "  ${CDM}Build${C0}                                   ${CDM}Run (kill + launch)${C0}"
echo "    ${CG}[1]${C0} ${CW}Dev${C0}          ${CDM}tauri dev, HMR${C0}       ${CG}[R]${C0} ${CW}Desktop${C0}"
echo "    ${CG}[2]${C0} ${CW}Desktop${C0}      ${CDM}cargo, committed dist/${C0}"
echo "    ${CG}[3]${C0} ${CW}CLI${C0}          ${CDM}syncdash, no node${C0}"
echo "    ${CG}[4]${C0} ${CW}All${C0}          ${CDM}[2] + [3]${C0}"
echo "                     ${CDM}2 / 4 auto-launch on success${C0}"
echo
echo "  ${CDM}Bundle${C0} ${CDM}(real .app via tauri bundler — Dock icon + app identity)${C0}"
echo "    ${CG}[A]${C0} ${CW}App Self${C0}     ${CDM}.app -> /Applications${C0}    ${CG}[V]${C0} ${CW}Reveal in Finder${C0}"
echo
echo "  ${CDM}Utility${C0}"
echo "    ${CY}[5]${C0} ${CW}Kill app${C0}    ${CY}[6]${C0} ${CW}Kill + installed${C0}    ${CY}[7]${C0} ${CW}Clean artifacts${C0}    ${CR}[Q]${C0} ${CW}Quit${C0}"
echo
MODE=""
read -t 3 -n 1 -r -p "   Choice [1-7 A V R Q]  (Enter / 3s -> 2 Desktop): " MODE
echo
[ -z "${MODE}" ] && MODE="2"     # timeout / Enter -> default
case "${MODE}" in [Qq]) echo; echo "  ${CB}Done.${C0}"; exit 0;; esac

# ---- Letter keys resolved BEFORE the digit guard --------------------------------
case "${MODE}" in
  [Rr])
    if [ ! -x "${APP}" ]; then
      echo
      echo "  ${CR}[Run] ERROR:${C0} not built yet:"
      print_link "${APP}"
      echo "  Build it with ${CW}[2]${C0} first."
      echo; echo "  ${CB}Done.${C0}"; exit 1
    fi
    free_desktop
    launch_app
    echo; echo "  ${CB}Done.${C0}"; exit 0 ;;
  [Aa])
    need_node || { echo; echo "  ${CR}===== CANNOT BUILD =====${C0}"; echo; echo "  ${CB}Done.${C0}"; exit 1; }
    free_desktop
    echo
    echo "  ${CG}[Build]${C0} SELF-USE APP (.app -> /Applications) ..."
    build_app_self
    echo; echo "  ${CB}Done.${C0}"; exit 0 ;;
  [Vv])
    reveal_bundle
    echo; echo "  ${CB}Done.${C0}"; exit 0 ;;
esac

# Anything else must be a digit mode; bail before the kill sweep on stray keys
# (read accepts ANY character, and a typo should not tear down a running app).
case "${MODE}" in
  [1-7]) ;;
  *) echo "  ${CR}Unknown choice:${C0} ${MODE}"; echo; echo "  ${CB}Done.${C0}"; exit 1 ;;
esac

case "${MODE}" in
  1)
    need_node || { echo; echo "  ${CR}===== CANNOT BUILD =====${C0}"; echo; echo "  ${CB}Done.${C0}"; exit 1; }
    # tauri dev relinks target/debug/syncdash-desktop and Vite holds the port with
    # strictPort, so a leftover instance of either kills the run before it starts.
    free_desktop
    free_port
    echo
    echo "  ${CG}[Dev]${C0} npx tauri dev  ${CDM}vite + tauri dev, this terminal${C0}"
    ( cd "${PROJECT_DIR}" && npx tauri dev )
    ;;
  2)
    need_cargo || { echo; echo "  ${CR}===== CANNOT BUILD =====${C0}"; echo; echo "  ${CB}Done.${C0}"; exit 1; }
    need_dist  || { echo; echo "  ${CB}Done.${C0}"; exit 1; }
    free_desktop
    total_start
    if step_desktop "[1/1]" && report "${APP}" "DESKTOP OK - syncdash-desktop"; then
      total_end
      launch_app
    else
      fail_build
    fi
    ;;
  3)
    need_cargo || { echo; echo "  ${CR}===== CANNOT BUILD =====${C0}"; echo; echo "  ${CB}Done.${C0}"; exit 1; }
    free_cli
    total_start
    if step_cli "[1/1]" && report "${CLI}" "CLI OK - syncdash"; then
      total_end
    else
      fail_build
    fi
    ;;
  4)
    need_cargo || { echo; echo "  ${CR}===== CANNOT BUILD =====${C0}"; echo; echo "  ${CB}Done.${C0}"; exit 1; }
    need_dist  || { echo; echo "  ${CB}Done.${C0}"; exit 1; }
    # One workspace, one target/release: both binaries are asked about up front
    # rather than failing at the second link three minutes in.
    free_desktop
    free_cli
    total_start
    if step_desktop "[1/2]" \
      && step_cli "[2/2]" \
      && report "${APP}" "DESKTOP OK - syncdash-desktop" \
      && report "${CLI}" "CLI OK - syncdash"; then
      total_end
      launch_app
    else
      fail_build
    fi
    ;;
  5)
    free_desktop
    free_port
    free_cli
    ;;
  6)
    free_desktop
    free_port
    free_cli
    free_installed
    ;;
  7)
    clean_all
    ;;
esac

echo
echo "  ${CB}Done.${C0}"
