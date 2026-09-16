#!/usr/bin/env bash
# Inietta i binari servizi/test in /bin e /test su fat.img (Fase 21).
# Single source of truth usata da run.sh e scripts/test-shell.py: DOPO mkfat
# (che rigenera l'immagine) e DOPO build-userland/build-tests. Fail-loud
# (set -e + || exit): senza binari init fallisce loud a boot.
#
# Nomi DESTINAZIONE in 8.3 (il FAT non ha LFN e mcopy troncherebbe in
# USERCO~1.BIN, irrisolvibili dal nostro reader): si spella il prefisso
# "user". I nomi display (ps) restano lunghi: viaggiano in SpawnMeta.
set -euo pipefail
cd "$(dirname "$0")/.."

IMG="userland/fs/fat.img"
mmd -i "$IMG" ::/bin ::/test || exit 1
mcopy -i "$IMG" userland/build/userconsole.bin ::/bin/console.bin || exit 1
mcopy -i "$IMG" userland/build/useruptime.bin  ::/bin/uptime.bin  || exit 1
mcopy -i "$IMG" userland/build/userdevfs.bin   ::/bin/devfs.bin   || exit 1
mcopy -i "$IMG" userland/build/userkbd.bin     ::/bin/kbd.bin     || exit 1
mcopy -i "$IMG" userland/build/usertty.bin     ::/bin/tty.bin     || exit 1
mcopy -i "$IMG" userland/build/usershell.bin   ::/bin/shell.bin   || exit 1
mcopy -i "$IMG" testland/build/usertestfs.bin   ::/test/testfs.bin   || exit 1
mcopy -i "$IMG" testland/build/usertestfat.bin  ::/test/testfat.bin  || exit 1
mcopy -i "$IMG" testland/build/usertests.bin    ::/test/tests.bin    || exit 1
mcopy -i "$IMG" testland/build/usertestcli.bin  ::/test/testcli.bin  || exit 1
mcopy -i "$IMG" testland/build/usertestspin.bin ::/test/testspin.bin || exit 1
mcopy -i "$IMG" testland/build/utcbstest.bin    ::/test/cbstest.bin  || exit 1
mcopy -i "$IMG" testland/build/userhogheap.bin  ::/test/hogheap.bin  || exit 1
mcopy -i "$IMG" testland/build/userdevreader.bin ::/test/devreadr.bin || exit 1
mcopy -i "$IMG" testland/build/userdemo.bin     ::/test/demo.bin     || exit 1
mcopy -i "$IMG" testland/build/userbench.bin    ::/test/bench.bin    || exit 1
echo "[inject] servizi in /bin + /test su $IMG"
