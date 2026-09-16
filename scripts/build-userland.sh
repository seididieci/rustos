#!/usr/bin/env bash
# Build dei binari userspace in modalita' freestanding.
#
# Qui stanno SOLO i servizi utente (userland/): init, console, fs, devfs,
# disk, shell, uptime, kbd, tty — i binari "ad uso utente" dell'OS. I binari della test suite
# (testland/) sono compilati da scripts/build-tests.sh.
#
# Prodotto: userland/build/*.bin, codice raw caricato a USER_CODE dai processi
# user e incluso nel kernel via `include_bytes!` (user_binary.rs).
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/build_common.sh

BUILD="userland/build"
TARGET_DIR="$BUILD/target"
export CARGO_TARGET_DIR="$TARGET_DIR"

# Suite di test: di default init la SALTA (feature `skip_tests`, boot di
# produzione dritto alla shell). Con RUN_TESTS=1 (run-tests.sh) init
# viene compilato con `--no-default-features` ed esegue la suite completa.
if [ "${RUN_TESTS:-0}" = "1" ]; then
    INIT_FEATURES="--no-default-features"
    echo "[build] userinit CON test suite (RUN_TESTS=1)"
else
    INIT_FEATURES=""
    echo "[build] userinit production (test saltati)"
fi

# Bench throughput (Fase P0, scripts/bench.sh): feature `bench` ortogonale a
# skip_tests (il bench gira anche senza suite, mai nel gate).
if [ "${RUN_BENCH:-0}" = "1" ]; then
    INIT_FEATURES="$INIT_FEATURES --features bench"
    echo "[build] userinit CON bench (RUN_BENCH=1)"
fi

build_one userland/init    userland/init/src/init.ld       userinit.bin    userinit $INIT_FEATURES
build_one userland/console userland/console/src/console.ld userconsole.bin userconsole
build_one userland/fs      userland/fs/src/fs.ld           userfs.bin      userfs
build_one userland/devfs   userland/devfs/src/devfs.ld     userdevfs.bin   userdevfs
build_one userland/disk    userland/disk/src/disk.ld       userdisk.bin    userdisk
build_one userland/kbd     userland/kbd/src/kbd.ld         userkbd.bin     userkbd
build_one userland/tty     userland/tty/src/tty.ld         usertty.bin     usertty
build_one userland/uptime  userland/uptime/src/uptime.ld   useruptime.bin  useruptime
build_one userland/shell   userland/shell/src/shell.ld     usershell.bin   usershell
