#!/usr/bin/env bash
# Build dei binari della TEST SUITE in modalita' freestanding.
#
# testland/ contiene ogni binario NON "ad uso utente": demo, i test
# ramfs/fat (usertestfs/usertestfat), gli strumenti di stress (hogheap,
# gli strumenti di stress (hogheap, devreader) e la suite di regressione
# completa (usertests + helper client/spin).
#
# Prodotto: testland/build/*.bin, iniettati in /fat/test via inject-bins.sh
# e spawnati da disco (spawn_image); solo init/disk/fs restano embedded.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/build_common.sh

BUILD="testland/build"
TARGET_DIR="$BUILD/target"
export CARGO_TARGET_DIR="$TARGET_DIR"

# Manifest hash dei servizi (Fase 36): generato da build-userland.sh (che gira
# sempre prima, vedi run.sh). Riesportato qui per i crate test che lo
# includono (usertests t51: peer_info atteso); se manca, fail loud subito.
if [ ! -f "build-meta/service_hashes.rs" ]; then
    echo "[build-tests] ERROR: build-meta/service_hashes.rs mancante (build-userland.sh prima)" >&2
    exit 1
fi
export VELORDOR_SERVICE_HASHES="$(pwd)/build-meta/service_hashes.rs"

build_one testland/demo    testland/demo/src/demo.ld         userdemo.bin      userdemo
build_one testland/testfs  testland/testfs/src/testfs.ld     usertestfs.bin    usertestfs
build_one testland/testfat testland/testfat/src/testfat.ld   usertestfat.bin   usertestfat
build_one testland/hogheap testland/hogheap/src/hogheap.ld   userhogheap.bin   userhogheap
build_one testland/devreader testland/devreader/src/devreader.ld userdevreader.bin userdevreader
build_one testland/usertests testland/usertests/src/usertests.ld usertests.bin usertests
build_one testland/usertest-client testland/usertest-client/src/client.ld usertestcli.bin usertestcli
build_one testland/usertest-spin testland/usertest-spin/src/spin.ld usertestspin.bin usertestspin
build_one testland/utcbstest testland/utcbstest/src/utcbstest.ld utcbstest.bin utcbstest
build_one testland/bench testland/bench/src/bench.ld userbench.bin userbench
