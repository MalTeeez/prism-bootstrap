#!/usr/bin/env bash
#
# Bring up Xvfb, record the screen, launch Minecraft into it, and once the
# title screen is reached close the game, then finish the recording, then
# tear down the screen. The clip brackets the whole session.
#
# Run this from the SAME directory your original launch command used
# (the one with ../launch.env and ../launch.argv relative to it).
#
set -uo pipefail

# ---- config -------------------------------------------------------------
DISPLAY_NUM=99
RES=854x480                    # used for both Xvfb -screen and ffmpeg -video_size
DEPTH=24
FPS=5
LOG=logs/log.txt               # game output (drives the quiet-detection)
XVFB_LOG=logs/xvfb.log         # Xvfb's own output
OUT=startup.mp4                # fragmented mp4: moov up front, viewable while writing
QUIET_SECONDS=15               # log silent this long => startup finished
START_TIMEOUT=180              # give up waiting after this many seconds
REC_MAX=600                    # hard cap on recording length (safety net)
FONT=/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf  # for the timestamp overlay
# -------------------------------------------------------------------------

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
cleanup() {
  # game -> recording (finalize while Xvfb still alive) -> screen
  [ -n "$GAME_PGID" ] && kill -TERM -- "-$GAME_PGID" 2>/dev/null
  stop_ffmpeg
  [ -n "$XVFB_PID" ] && kill -TERM "$XVFB_PID" 2>/dev/null
  [ -n "$TAIL_PID" ] && kill "$TAIL_PID" 2>/dev/null
}
trap cleanup INT TERM

# ---- 1. start Xvfb ourselves (no -auth => no cookie needed) --------------
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

# ---- 2. start recording the screen (before the game launches) -----------
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

# ---- 3. launch the game in its own session / process group --------------
# setsid => $! is the PGID, so `kill -- -$PGID` cleans up java + any children.
setsid bash -c 'env $(grep -v "^\\s*#" ../launch.env | xargs) "$(head -1 ../launch.argv)" @<(tail -n +2 ../launch.argv)' >"$LOG" 2>&1 &
GAME_PGID=$!
echo "launched game (process group $GAME_PGID) -> logging to $LOG"

# mirror the game log to stdout for live observation (file still drives detection)
tail -n +1 -F "$LOG" 2>/dev/null &
TAIL_PID=$!

# ---- 4. wait until the log goes quiet (menu) or the game exits ----------
start=$(date +%s); last_change=$start; last_size=0
while :; do
  now=$(date +%s)
  size=$(stat -c %s "$LOG" 2>/dev/null || echo 0)
  [ "$size" != "$last_size" ] && { last_size=$size; last_change=$now; }
  if ! kill -0 -- "-$GAME_PGID" 2>/dev/null; then
    echo "game closed on its own"; break
  fi
  if [ $((now - last_change)) -ge "$QUIET_SECONDS" ]; then
    echo "log quiet for ${QUIET_SECONDS}s - menu reached, closing game"; break
  fi
  if [ $((now - start)) -ge "$START_TIMEOUT" ]; then
    echo "timed out after ${START_TIMEOUT}s - closing game anyway"; break
  fi
  sleep 1
done

# ---- 5. close the game and wait for it to fully exit --------------------
if kill -0 -- "-$GAME_PGID" 2>/dev/null; then
  kill -TERM -- "-$GAME_PGID" 2>/dev/null
  for _ in $(seq 1 20); do
    kill -0 -- "-$GAME_PGID" 2>/dev/null || break
    sleep 0.5
  done
  kill -KILL -- "-$GAME_PGID" 2>/dev/null
fi
echo "game closed"

# ---- 6. finish the recording (after the game is closed) -----------------
stop_ffmpeg
echo "recording stopped"

# ---- 7. tear down the screen --------------------------------------------
kill -TERM "$XVFB_PID" 2>/dev/null
for _ in $(seq 1 10); do
  kill -0 "$XVFB_PID" 2>/dev/null || break
  sleep 0.5
done
kill -KILL "$XVFB_PID" 2>/dev/null

# stop mirroring the log
[ -n "$TAIL_PID" ] && kill "$TAIL_PID" 2>/dev/null

trap - INT TERM
echo "done - video at $OUT"