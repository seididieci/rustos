#!/usr/bin/env bash
# Gate di regressione Velordor: boot con la test suite completa.
#
# `./run.sh` di default e' produzione (init salta i test, shell subito).
# Questo wrapper imposta RUN_TESTS=1 (init compilato con --no-default-features
# ed esegue usertestfs/usertestfat/usertests in sequenza prima della shell)
# e rimanda a run.sh. Righe attese + zero FAIL/PANIC/FAULT:
#   [testfs] PASS 5/5
#   [testfat] PASS 7/7
#   [usertests] PASS 57/57
set -euo pipefail
cd "$(dirname "$0")"
# Diagnostica scheduler/IRQ attiva nei run di test (feature `sched_debug`).
RUN_TESTS=1 SCHED_DEBUG=1 exec ./run.sh "$@"
