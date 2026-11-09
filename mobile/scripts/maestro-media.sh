#!/usr/bin/env bash
# The photos Maestro's `addMedia` seeds into a test device (#1769). LOCAL MAC ONLY.
#
#   mobile/scripts/maestro-media.sh reset <adb-serial|simulator-udid>
#   mobile/scripts/maestro-media.sh watch <adb-serial>      (runs until killed)
#
# `reset` clears what earlier flows seeded; `watch` keeps the Android photo
# picker current while a flow seeds more. maestro-app-id.sh's `maestro` wrapper
# runs both around every Android flow that uses `addMedia`; maestro-run.sh and
# the two-client scripts run `reset` once on iOS. Nothing else needs to call
# them.
#
# Why `reset`: `addMedia` adds its fixture to the device library on every run
# and never removes it.
#  - Android names a repeat `media-viewer (7).png` and stops numbering at 32
#    copies. Every later `addMedia` of that fixture then fails with a bare gRPC
#    UNKNOWN, before the app is involved. The app's "Save to photos" also adds a
#    `<number>.png` to DCIM/ on each media-viewer run.
#  - iOS has no cap, but the library grows by every photo of every run (400+
#    on the gate iPhone by 1.0.32).
#
# Why `watch`: on Android the photo picker keeps its own copy of the library
# (picker.db), which MediaProvider updates when it is told something changed.
# `addMedia` writes through the Maestro driver's ContentResolver, and the row
# it leaves is completed a moment later with no such notice, so the picker
# never lists the new photo (a picker opened after it still shows "No photos
# yet"). Until #1769 the flows passed anyway, because they picked an OLDER copy
# of the same fixture that an earlier run had left behind. `watch` asks
# MediaProvider to sync the picker whenever MediaStore's newest image or video
# generation moves, about once a second; an open picker redraws on that sync.
#
# Android reset: deletes the fixtures (and their numbered copies) and the
# app's DCIM saves through MediaStore, syncs the picker, then checks the
# picker's database against MediaStore. If the picker still lists something
# MediaStore no longer holds (it once kept showing deleted copies as tiles that
# could not be selected, until a reboot), it gets a full resync instead: drop
# its sync state and sync again. That check needs a root adbd or `su`, which
# the userdebug emulator images have. A few seconds.
#
# iOS reset: no command deletes a simulator photo, so the library itself is
# reset. The simulator is shut down, its DCIM/ and PhotoData/ are removed, and
# it is booted again; Photos makes a new, empty library on boot. The
# simulator's stock sample photos go too, which no flow uses. It costs a
# reboot, so it runs once per run, before the simulator is first used.
set -euo pipefail

USAGE="usage: maestro-media.sh reset <adb-serial|simulator-udid> | watch <adb-serial>"
CMD="${1:?$USAGE}"
DEVICE="${2:?$USAGE}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"        # mobile/
FIXTURES="$HERE/.maestro/fixtures"
UUID_RE='^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$'

reset_ios() {
  local media="$HOME/Library/Developer/CoreSimulator/Devices/$DEVICE/data/Media" before
  [ -d "$media" ] || { echo "no simulator data at $media" >&2; exit 1; }
  before="$(find "$media/DCIM" -type f 2>/dev/null | wc -l | tr -d ' ')"
  if [ "$before" = "0" ]; then
    echo "==> media reset ($DEVICE): library already empty"
    return
  fi
  if xcrun simctl list devices | grep -F "($DEVICE)" | grep -q "(Booted)"; then
    xcrun simctl shutdown "$DEVICE"
  fi
  rm -rf "$media/DCIM" "$media/PhotoData"
  xcrun simctl boot "$DEVICE"
  xcrun simctl bootstatus "$DEVICE" -b >/dev/null
  echo "==> media reset ($DEVICE): removed the photo library ($before files)"
}

ADB=(adb -s "$DEVICE")
PICKER_DB=/data/data/com.google.android.providers.media.module/databases/picker.db
PICKER_PREFS=/data/data/com.google.android.providers.media.module/shared_prefs/picker_sync_prefs.xml

media_query() {  # <projection> <where> [sort]: rows of images and videos
  local q="content query --uri content://media/external/file --projection $1"
  q="$q --where \"media_type IN (1,3)${2:+ AND ($2)}\""
  if [ -n "${3:-}" ]; then
    q="$q --sort \"$3\""
  fi
  "${ADB[@]}" shell "$q" | tr -d '\r'
}
count_media() { media_query _id "" | grep -c '^Row:' || true; }
sync_picker() { "${ADB[@]}" shell "content call --uri content://media/ --method sync_providers" >/dev/null; }

watch_android() {
  local last="" gen
  # Ends with the script that started it, even one killed mid-flow.
  while kill -0 "$PPID" 2>/dev/null; do
    gen="$(media_query generation_modified "" "generation_modified DESC" | head -1)"
    if [ "$gen" != "$last" ]; then
      sync_picker || true
      last="$gen"
    fi
    sleep 1
  done
}

reset_android() {
  # A fixture `name.ext` is stored as `name.ext`, then `name (1).ext` … `name (31).ext`.
  # The app's "Save to photos" writes DCIM/<media id>.<ext>.
  local where="(relative_path = 'DCIM/' AND _display_name GLOB '[0-9]*.*'"
  where="$where AND rtrim(substr(_display_name, 1, instr(_display_name, '.') - 1), '0123456789') = '')"
  local f name stem ext
  for f in "$FIXTURES"/*; do
    [ -f "$f" ] || continue
    name="$(basename "$f")"; stem="${name%.*}"; ext="${name##*.}"
    where="$where OR _display_name = '$name' OR _display_name LIKE '$stem (%).$ext'"
  done

  # Root commands (the picker's database) run directly under a root adbd, else via su.
  local root=()
  if [ "$("${ADB[@]}" shell id -u | tr -d '\r')" = "0" ]; then
    root=(sh -c)
  elif [ "$("${ADB[@]}" shell su 0 id -u 2>/dev/null | tr -d '\r')" = "0" ]; then
    root=(su 0 sh -c)
  fi
  as_root() { "${ADB[@]}" shell "${root[@]}" "\"$1\""; }
  count_picker() {  # on-device items the photo picker lists
    as_root "sqlite3 $PICKER_DB 'select count(*) from media where local_id is not null'" | tr -d '\r'
  }

  local before after picker
  before="$(count_media)"
  "${ADB[@]}" shell "content delete --uri content://media/external/file --where \"$where\""
  sync_picker
  # The picker app keeps its grid in memory; a fresh process reads it anew.
  "${ADB[@]}" shell am force-stop com.google.android.photopicker 2>/dev/null || true
  after="$(count_media)"

  if [ ${#root[@]} -eq 0 ]; then
    echo "==> media reset ($DEVICE): $before -> $after items (no root: picker not checked)"
    return
  fi
  picker="$(count_picker)"
  if [ "$picker" != "$after" ]; then
    echo "    picker lists $picker items, MediaStore holds $after: full picker resync"
    # Without its sync state MediaProvider treats the next sync as a first one
    # and rebuilds the picker's rows from MediaStore. Stop it first, or it
    # writes the old state back.
    as_root "am force-stop com.google.android.providers.media.module; rm -f $PICKER_PREFS"
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      sync_picker
      picker="$(count_picker)"
      [ "$picker" = "$after" ] && break
      sleep 1
    done
    if [ "$picker" != "$after" ]; then
      echo "!! photo picker still lists $picker items against MediaStore's $after" >&2
      exit 1
    fi
  fi
  echo "==> media reset ($DEVICE): $before -> $after items, picker in sync"
}

case "$CMD" in
  reset)
    if [[ "$DEVICE" =~ $UUID_RE ]]; then reset_ios; else reset_android; fi ;;
  watch)
    if [[ "$DEVICE" =~ $UUID_RE ]]; then
      echo "watch is Android-only: iOS adds the photo to the library at once" >&2; exit 1
    fi
    watch_android ;;
  *) echo "$USAGE" >&2; exit 1 ;;
esac
