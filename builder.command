#!/usr/bin/env bash
# SyncDash Builder — macOS. Double-click in Finder, or ./builder.command
# Digit items mirror builder.bat one for one; [A]/[V] are macOS-only.
# [2]/[3]/[4] are pure cargo over the committed dist/; [1] and [A] need Node.
set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REL="${PROJECT_DIR}/target/release"
DBG="${PROJECT_DIR}/target/debug"
APP="${REL}/syncdash-desktop"
CLI="${REL}/syncdash"
BUNDLE="${REL}/bundle"
VITE_PORT="5173"
BUNDLE_ID="com.dosmiao.syncdash"   # must equal tauri.conf.json `identifier`
export PATH="$HOME/.cargo/bin:$PATH"

if [ -t 1 ]; then
  C0=$'\033[0m';  CB=$'\033[96m'; CT=$'\033[1;93m'; CG=$'\033[92m'
  CY=$'\033[93m'; CR=$'\033[91m'; CW=$'\033[97m';   CDM=$'\033[90m'
else
  C0= CB= CT= CG= CY= CR= CW= CDM=
fi

print_link() {
  local p="$1"
  if [ -t 1 ]; then
    printf '  \033]8;;file://%s\033\\>> %s\033]8;;\033\\\n' "$p" "$p"
  else
    printf '  >> %s\n' "$p"
  fi
}

PT0=0; TT0=0; EL_FMT=""
phase_start() { PT0=$(date +%s); printf '\n  %s[%s]%s  %sSTART%s  %s%s%s\n' "$CDM" "$(date +%H:%M:%S)" "$C0" "$CG" "$C0" "$CW" "$1" "$C0"; }
phase_end()   { local el=$(( $(date +%s) - PT0 )); EL_FMT="$(printf '%dm %02ds' $((el/60)) $((el%60)))"; printf '  %s[%s]%s  %sEND%s    %s  %s| elapsed%s %s%s%s\n' "$CDM" "$(date +%H:%M:%S)" "$C0" "$CB" "$C0" "$1" "$CDM" "$C0" "$CW" "$EL_FMT" "$C0"; }
total_start() { TT0=$(date +%s); }
total_end()   { local el=$(( $(date +%s) - TT0 )); printf '\n  %s[%s]%s  %sTOTAL%s  %selapsed%s %s%dm %02ds%s\n' "$CDM" "$(date +%H:%M:%S)" "$C0" "$CT" "$C0" "$CDM" "$C0" "$CW" $((el/60)) $((el%60)) "$C0"; }

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

need_node() {
  command -v npm >/dev/null 2>&1 || {
    echo
    echo "  ${CR}npm not found.${C0} Install Node: https://nodejs.org"
    return 1
  }
  # A stale node_modules fails much later as an unresolved import, which reads
  # like a code error. npm install touches the dir, so mtimes catch it.
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

# dist/ is committed on purpose — the pure cargo paths cannot regenerate it.
need_dist() {
  [ -d "${PROJECT_DIR}/dist" ] && return 0
  echo
  echo "  ${CR}dist/ is missing.${C0} It is a committed artifact — run ${CW}npm run build${C0}"
  echo "  once, commit dist/, then pull. ([1] and [A] regenerate it via tauri.)"
  return 1
}

# Matched by path: the process name is kernel-truncated at 16 bytes, which
# "syncdash-desktop" exactly hits, so name matching is unreliable here.
free_desktop() {
  echo
  echo "  ${CY}[Kill]${C0} Freeing syncdash-desktop ..."
  local hit=0
  pkill -f "${APP}"                  >/dev/null 2>&1 && hit=1
  pkill -f "${DBG}/syncdash-desktop" >/dev/null 2>&1 && hit=1
  # Anchored to THIS checkout: a bare `pkill -f "tauri dev"` would take down
  # another project's dev server — three Tauri apps share this machine.
  pkill -f "${PROJECT_DIR}/node_modules/.*tauri" >/dev/null 2>&1 && hit=1
  if [ "${hit}" -eq 1 ]; then
    sleep 1
    echo "    Freed syncdash-desktop + children"
  else
    echo "    Nothing running"
  fi
}

# A live syncdash may be mid-apply, holding the root heartbeat lock — ask, never
# kill it silently (CLAUDE.md:45). pgrep -x is exact, so it skips -desktop.
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

# Vite runs strictPort: a leftover dev server does not shift to 5174, it refuses
# to start at all.
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

# ditto, not cp -R: it keeps the bundle's xattrs and symlink layout. The
# destination is removed first because ditto MERGES into an existing directory,
# so a file dropped between builds would survive forever in the installed copy.
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
    # Never rm -rf what is not ours. PlistBuddy reads the file directly, where
    # `defaults read` would answer from a cache.
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

# The root package IS the CLI, so -p syncdash-desktop is required — forgetting it
# leaves a stale GUI binary that looks like a code bug (CLAUDE.md:42).
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
  # Detach so it reparents to launchd and survives this window (= `start ""`).
  ( "${APP}" >/dev/null 2>&1 </dev/null & )
}

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

  # The installed copy, never the target-dir one — otherwise the process you use
  # is the build artifact and /Applications holds a cold duplicate.
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

clean_all() {
  free_desktop
  free_cli
  echo
  echo "  ${CY}[Clean]${C0} cargo clean over the workspace, dist/ untouched ..."
  cargo clean --manifest-path "${PROJECT_DIR}/Cargo.toml" \
    && echo "    target/ cleaned" || echo "    ${CR}Clean FAILED${C0}"
}

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

# read accepts ANY character — bail before the kill sweep so a typo cannot tear
# down a running app.
case "${MODE}" in
  [1-7]) ;;
  *) echo "  ${CR}Unknown choice:${C0} ${MODE}"; echo; echo "  ${CB}Done.${C0}"; exit 1 ;;
esac

case "${MODE}" in
  1)
    need_node || { echo; echo "  ${CR}===== CANNOT BUILD =====${C0}"; echo; echo "  ${CB}Done.${C0}"; exit 1; }
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
    # One target/release for the workspace: ask about both binaries up front
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
