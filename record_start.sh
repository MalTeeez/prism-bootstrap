#!/usr/bin/env bash

#
# Run this from the SAME directory your original launch command used
# (the one with ../launch.env and ../launch.argv relative to it).
#
set -uo pipefail

# --- config ---
DISPLAY_NUM=99
RES=854x480                    # used for both Xvfb -screen and ffmpeg -video_size
DEPTH=24
FPS=5
LOG=headless.log               # game output (mirrored to stdout)
XVFB_LOG=logs/xvfb.log         # Xvfb's own output
OUT=startup.mp4                # fragmented mp4: moov up front, viewable while writing
READY_FILE="./.worldloaded.headlessnh"  # when this appears the game is ready to shut down
WIN_NAME="Minecraft"           # window-title pattern (regex/substring) for xdotool focus
WAIT_FOR_QUIET=false           # also treat a silent log as "done"
QUIET_SECONDS=15               # for WAIT_WITH_QUIET, fallback for ready file not present
START_TIMEOUT=240              # give up waiting after this many seconds
GRACE_SECONDS=10               # wait this long for an orderly quit before signalling
REC_MAX=600                    # hard cap on recording length (safety net)
FONT=/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf  # for the timestamp overlay
# ---

mkdir -p logs

FFMPEG_PID=""
GAME_PGID=""
XVFB_PID=""
TAIL_PID=""

stop_ffmpeg() {
  if [ -n "$FFMPEG_PID" ] && kill -0 "$FFMPEG_PID" 2>/dev/null; then
    kill -INT "$FFMPEG_PID" 2>/dev/null; wait "$FFMPEG_PID" 2>/dev/null
  fi
}
# Send a WM_DELETE_WINDOW ClientMessage. 
# GLFW should deliver it to Minecraft as a normal quit, and it works with no WM. 
# Needs python3-xlib;.
close_window_nicely() {
  local wid
  wid=$(xdotool search --onlyvisible --name "$WIN_NAME" 2>/dev/null | head -1) || return 1
  [ -n "$wid" ] || return 1
  python3 - "$wid" <<'PY' 2>/dev/null || return 1
import sys
from Xlib import X, display
from Xlib.protocol import event
d = display.Display()
w = d.create_resource_object('window', int(sys.argv[1], 0))
w.send_event(event.ClientMessage(
    window=w,
    client_type=d.intern_atom('WM_PROTOCOLS'),
    data=(32, [d.intern_atom('WM_DELETE_WINDOW'), X.CurrentTime, 0, 0, 0])))
d.flush()
PY
}

cleanup() {
  # game -> recording (finalize while Xvfb still alive) -> screen
  [ -n "$GAME_PGID" ] && kill -TERM -- "-$GAME_PGID" 2>/dev/null
  stop_ffmpeg
  [ -n "$XVFB_PID" ] && kill -TERM "$XVFB_PID" 2>/dev/null
  [ -n "$TAIL_PID" ] && kill "$TAIL_PID" 2>/dev/null
}
trap cleanup INT TERM

# --- 1. start Xvfb for a normal screen imitation ---
Xvfb ":$DISPLAY_NUM" -screen 0 "${RES}x${DEPTH}" -nolisten tcp >"$XVFB_LOG" 2>&1 &
XVFB_PID=$!

SOCK="/tmp/.X11-unix/X$DISPLAY_NUM"
for _ in $(seq 1 60); do
  [ -S "$SOCK" ] && break
  kill -0 "$XVFB_PID" 2>/dev/null || { echo "Xvfb failed to start (see $XVFB_LOG)"; exit 1; }
  sleep 0.5
done
[ -S "$SOCK" ] || { echo "display :$DISPLAY_NUM never appeared"; cleanup; exit 1; }
sleep 1   # small grace so the server is accepting connections

export DISPLAY=":$DISPLAY_NUM"
echo "Xvfb up on :$DISPLAY_NUM (pid $XVFB_PID)"

# --- 2. start recording the screen (before the game launches) (with system time) ---
ffmpeg -nostdin -loglevel warning -y \
  -f x11grab -draw_mouse 0 -framerate "$FPS" -video_size "$RES" -i "$DISPLAY" \
  -vf "drawtext=fontfile=$FONT:text='%{localtime\:%F %T}':x=10:y=10:fontsize=24:fontcolor=white:box=1:boxcolor=black@0.6:boxborderw=8" \
  -t "$REC_MAX" -c:v libx264 -pix_fmt yuv420p -g 1 -r "$FPS" \
  -movflags +frag_keyframe+empty_moov+default_base_moof -f mp4 "$OUT" &
FFMPEG_PID=$!
sleep 2
if ! kill -0 "$FFMPEG_PID" 2>/dev/null; then
  echo "WARNING: ffmpeg exited immediately (check messages above); continuing without a recording"
  FFMPEG_PID=""
else
  echo "recording screen -> $OUT (pid $FFMPEG_PID)"
fi

# --- 3. launch the game in its own session / process group ---
rm -f "$READY_FILE"

# setsid => $! is the PGID, so `kill -- -$PGID` cleans up everything with just one target
setsid bash -c 'env $(grep -v "^\\s*#" ../launch.env | xargs) "$(head -1 ../launch.argv)" @<(tail -n +2 ../launch.argv)' >"$LOG" 2>&1 &
GAME_PGID=$!
echo "launched game (process group $GAME_PGID) -> logging to $LOG"

# mirror the game log to stdout
tail -n +1 -F "$LOG" 2>/dev/null &
TAIL_PID=$!

# give the window X input focus
# --sync blocks until the window maps
if timeout 60 xdotool search --sync --onlyvisible --name "$WIN_NAME" windowfocus 2>/dev/null; then
  echo "focused '$WIN_NAME' window"
else
  echo "WARNING: no '$WIN_NAME' window to focus within 60s"
fi

# --- 4. wait for the ready-file (or game exit / quiet / timeout) ---
start=$(date +%s); last_change=$start; last_size=0
while :; do
  now=$(date +%s)
  # primary signal: the game created the ready-file
  if [ -e "$READY_FILE" ]; then
    echo "ready-file $READY_FILE present -- shutting down"; break
  fi

  size=$(stat -c %s "$LOG" 2>/dev/null || echo 0)
  [ "$size" != "$last_size" ] && { last_size=$size; last_change=$now; }
  if ! kill -0 -- "-$GAME_PGID" 2>/dev/null; then
    echo "game seems to have closed on its own"; break
  fi

  # seondary signal, log was quit for x seconds (if enabled)
  if [ "$WAIT_FOR_QUIET" = true ] && [ $((now - last_change)) -ge "$QUIET_SECONDS" ]; then
    echo "log quiet for ${QUIET_SECONDS}s -- assuming done, closing game"; break
  fi
  if [ $((now - start)) -ge "$START_TIMEOUT" ]; then
    echo "timed out after ${START_TIMEOUT}s -- closing game anyway"; break
  fi
  sleep 1
done

# --- 5. close the game (nicely first, then escalate) ---
if kill -0 -- "-$GAME_PGID" 2>/dev/null; then
  sleep 5
  if close_window_nicely; then
    echo "asked window to close (WM_DELETE_WINDOW); waiting up to ${GRACE_SECONDS}s for save & exit"
  else
    echo "couldn't send nice window-close -- will signal instead"
  fi

  # wait for the orderly quit (world save can take a moment)
  for _ in $(seq 1 $((GRACE_SECONDS * 2))); do
    kill -0 -- "-$GAME_PGID" 2>/dev/null || break
    sleep 0.5
  done

  # still alive? escalate to SIGTERM (JVM shutdown hooks)
  if kill -0 -- "-$GAME_PGID" 2>/dev/null; then
    echo "still running -- sending SIGTERM"
    kill -TERM -- "-$GAME_PGID" 2>/dev/null
    for _ in $(seq 1 20); do
      kill -0 -- "-$GAME_PGID" 2>/dev/null || break
      sleep 0.5
    done
  fi

  # last resort
  if kill -0 -- "-$GAME_PGID" 2>/dev/null; then
    echo "still running -- sending SIGKILL"
    kill -KILL -- "-$GAME_PGID" 2>/dev/null
  fi
fi
echo "game closed"

# --- 6. finish the recording (now that the game is closed) ---
stop_ffmpeg
echo "recording stopped"

# --- 7. tear down the screen ---
kill -TERM "$XVFB_PID" 2>/dev/null
for _ in $(seq 1 10); do
  kill -0 "$XVFB_PID" 2>/dev/null || break
  sleep 0.5
done
kill -KILL "$XVFB_PID" 2>/dev/null

# stop mirroring the log
[ -n "$TAIL_PID" ] && kill "$TAIL_PID" 2>/dev/null

trap - INT TERM
echo "done -- video at $OUT"