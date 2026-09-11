#!/usr/bin/env bash
# Build dei binari della TEST SUITE in modalita' freestanding.
#
# testland/ contiene ogni binario NON "ad uso utente": demo e coppia
# server/client IPC storiche, i test ramfs/fat (usertestfs/usertestfat),
# gli strumenti di stress (hogheap, devreader) e la suite di regressione
# completa (usertests + helper client/spin).
#
# Prodotto: testland/build/*.bin, incluso nel kernel via `include_bytes!`
# (user_binary.rs) e spawnato da init o dall'orchestratore usertests.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/build_common.sh

BUILD="testland/build"
TARGET_DIR="$BUILD/target"
export CARGO_TARGET_DIR="$TARGET_DIR"

build_one testland/demo    testland/demo/src/demo.ld         userdemo.bin      userdemo
build_one testland/testfs  testland/testfs/src/testfs.ld     usertestfs.bin    usertestfs
build_one testland/testfat testland/testfat/src/testfat.ld   usertestfat.bin   usertestfat
build_one testland/hogheap testland/hogheap/src/hogheap.ld   userhogheap.bin   userhogheap
build_one testland/devreader testland/devreader/src/devreader.ld userdevreader.bin userdevreader
build_one testland/usertests testland/usertests/src/usertests.ld usertests.bin usertests
build_one testland/usertest-client testland/usertest-client/src/client.ld usertestcli.bin usertestcli
build_one testland/usertest-spin testland/usertest-spin/src/spin.ld usertestspin.bin usertestspin
build_one testland/utcbstest testland/utcbstest/src/utcbstest.ld utcbstest.bin utcbstest
