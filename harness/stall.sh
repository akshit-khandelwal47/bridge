#!/bin/bash
# Stall test for ComplyEaze/bridge#1426. Run from the bridge checkout: stall.sh <log dir> <drip.py>.
# Every command's output goes to a file in the log dir; only bounded summaries are printed.
set -uo pipefail
logs=$1; drip=$2; mkdir -p "$logs"
failures=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; failures=$((failures + 1)); }
control() { echo "CONTROL  $*"; }

# The install lines exactly as the checked-out commit's ci.yml has them.
step=$(awk '/- name: Install Playwright browsers and system dependencies/ {f = 1}
  f && /^        run: \|/ {r = 1; next} r && /^      - / {exit} r {print substr($0, 11)}' .github/workflows/ci.yml)
conf_line=$(printf '%s\n' "$step" | grep -F 'sudo tee /etc/apt/apt.conf.d/99-bridge-lock-timeout')
cmd_line=$(printf '%s\n' "$step" | grep -F 'install-deps chromium webkit' | sed -E 's/^ *if //; s/; then$//')
echo "committed apt config line: $conf_line"
echo "committed install line:    $cmd_line"
case "$cmd_line" in
  'sudo timeout --kill-after=10 600 "$(command -v node)" node_modules/@playwright/test/cli.js install-deps chromium webkit') ;;
  *) echo "the install line is not the one under test"; exit 1 ;;
esac
short_cmd=${cmd_line/ 600 / 60 }
echo "under test, with the limit lowered to 60 s: $short_cmd"
eval "$conf_line"

{ apt-get --version | head -1; apt-config dump | grep -iE '^(Acquire::Retries|Acquire::https?::Timeout|Dpkg::Use-Pty|DPkg::Lock::Timeout) ' || echo 'none of the keys is set';
  echo '--- apt-mirrors.txt'; cat /etc/apt/apt-mirrors.txt; echo '--- ubuntu.sources'; cat /etc/apt/sources.list.d/ubuntu.sources; } > "$logs/apt-config.txt" 2>&1
cat "$logs/apt-config.txt" | head -40

# Survivors: any apt, apt method, dpkg, Playwright or hook process, and any holder of apt's or dpkg's locks.
locks=(/var/lib/apt/lists/lock /var/lib/dpkg/lock /var/lib/dpkg/lock-frontend /var/cache/apt/archives/lock)
survivors() {  # $1 = file to write; returns 0 when something survived
  {
    echo "--- processes"; pgrep -a -f 'apt-get|/usr/lib/apt/methods/|/usr/bin/dpkg|install-deps|sleep 301' || true
    echo "--- lock holders"; sudo fuser -v "${locks[@]}" 2>&1 || true
  } > "$1"
  pgrep -f 'apt-get|/usr/lib/apt/methods/|/usr/bin/dpkg|install-deps|sleep 301' > /dev/null && return 0
  sudo fuser "${locks[@]}" > /dev/null 2>&1 && return 0
  return 1
}
clean_up() { sudo pkill -KILL -f 'apt-get|/usr/lib/apt/methods/|/usr/bin/dpkg|install-deps|sleep 301' || true; sleep 2; }
timed() {  # timed <log> <command…>: runs, records status and seconds
  local log=$1; shift; local start=$SECONDS
  eval "$@" > "$log" 2>&1; status=$?; elapsed=$((SECONDS - start))
}

survivors "$logs/0-before.txt" && { fail "something apt-related is running before the test"; cat "$logs/0-before.txt"; }

# Point the Ubuntu mirror list at the drip server only.
sudo cp /etc/apt/apt-mirrors.txt "$logs/apt-mirrors.orig"
python3 -I "$drip" > "$logs/drip.txt" 2>&1 &
drip_pid=$!
sleep 1
printf 'http://127.0.0.1:8080/ubuntu/\tpriority:1\n' | sudo tee /etc/apt/apt-mirrors.txt > /dev/null

# A (control): apt's own timeout does not end a drip-fed update.
sudo apt-get update > "$logs/A-apt-update.txt" 2>&1 &
sleep 120
if pgrep -x apt-get > /dev/null; then control "A: apt-get update still running after 120 s on a drip-fed mirror, with apt's own timeout in effect"
else control "A: apt-get update ended by itself within 120 s (see A-apt-update.txt)"; fi
tail -n 12 "$logs/A-apt-update.txt"
clean_up
survivors "$logs/A-after-cleanup.txt" && { fail "A: cleanup left a survivor"; cat "$logs/A-after-cleanup.txt"; }

# B (control): the shape #1093 removed, an unprivileged timeout around Playwright, which starts apt through sudo.
timed "$logs/B-unprivileged-timeout.txt" timeout --kill-after=10 45 corepack pnpm exec playwright install-deps chromium webkit
sleep 3
if survivors "$logs/B-survivors.txt"; then control "B: unprivileged timeout returned $status after ${elapsed}s and left survivors:"; else control "B: unprivileged timeout returned $status after ${elapsed}s and left NO survivor"; fi
head -n 20 "$logs/B-survivors.txt"
clean_up

# C: the committed line, limit 60 s, against the drip-fed update.
timed "$logs/C-root-timeout.txt" "$short_cmd"
sleep 3
if [ "$status" = 124 ] || [ "$status" = 137 ]; then pass "C: timed out with status $status after ${elapsed}s"; else fail "C: status $status after ${elapsed}s"; fi
grep -q 'Switching to root user' "$logs/C-root-timeout.txt" && fail "C: Playwright switched to root itself, so it was not root" || pass "C: Playwright ran as root (no 'Switching to root user' line)"
if survivors "$logs/C-survivors.txt"; then fail "C: survivors after the kill:"; head -n 20 "$logs/C-survivors.txt"; clean_up; else pass "C: no apt, method, dpkg or Playwright process and no lock holder after the kill"; fi

# Restore the real mirrors.
kill "$drip_pid"; sudo cp "$logs/apt-mirrors.orig" /etc/apt/apt-mirrors.txt

# D: a kill that lands while dpkg runs (a dpkg pre-invoke hook sleeps), with the committed Dpkg::Use-Pty "0".
echo 'pre-invoke=/bin/sleep 301' | sudo tee /etc/dpkg/dpkg.cfg.d/zz-bridge1426-stall > /dev/null
timed "$logs/D-dpkg-phase.txt" "$short_cmd"
sleep 3
grep -c '^Get:' "$logs/D-dpkg-phase.txt" | sed 's/^/D: Get lines: /'
if [ "$status" = 124 ] || [ "$status" = 137 ]; then pass "D: timed out with status $status after ${elapsed}s"; else fail "D: status $status after ${elapsed}s (the hook should have held dpkg past the limit)"; fi
if survivors "$logs/D-survivors.txt"; then fail "D: survivors with Use-Pty 0:"; head -n 20 "$logs/D-survivors.txt"; clean_up; else pass "D: no survivor with Dpkg::Use-Pty \"0\" (dpkg was killed with the group)"; fi

# E (control): the same with Dpkg::Use-Pty "1", apt's default, written to a file that sorts after the step's.
echo 'Dpkg::Use-Pty "1";' | sudo tee /etc/apt/apt.conf.d/99zz-bridge1426-usepty > /dev/null
timed "$logs/E-dpkg-phase-pty.txt" "$short_cmd"
sleep 3
if survivors "$logs/E-survivors.txt"; then control "E: with Use-Pty 1, status $status after ${elapsed}s, survivors:"; else control "E: with Use-Pty 1, status $status after ${elapsed}s, NO survivor"; fi
head -n 20 "$logs/E-survivors.txt"
clean_up
sudo rm -f /etc/apt/apt.conf.d/99zz-bridge1426-usepty /etc/dpkg/dpkg.cfg.d/zz-bridge1426-stall

# F: what the step does next: finish dpkg, then the committed line with its real 600 s limit.
sudo dpkg --configure -a > "$logs/F-dpkg-configure.txt" 2>&1; echo "F: dpkg --configure -a status $?"
timed "$logs/F-next-attempt.txt" "$cmd_line"
if [ "$status" = 0 ]; then pass "F: the next attempt succeeded in ${elapsed}s"; else fail "F: the next attempt failed with $status after ${elapsed}s"; fi
grep -E 'Could not get lock|Waiting for cache lock' "$logs/F-next-attempt.txt" && fail "F: the next attempt met a held lock" || pass "F: no lock wait or lock error"
grep -E '^Need to get|newly installed|^Fetched' "$logs/F-next-attempt.txt"

echo "failures: $failures"
exit $((failures > 0))
