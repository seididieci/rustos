#!/usr/bin/env python3
"""Prova di stabilita' UUID/LABEL al reorder dei dischi (Fase 16d).

Due boot completi con suite (RUN_TESTS=1):
  1. ordine normale   (fat.img=sda, fat2.img=sdb)
  2. ordine invertito (SWAP_DRIVES=1: fat2.img=sda, fat.img=sdb)

In entrambi asserisce dal log seriale:
  - t36 PASS (mount UUID=/LABEL= + by-path + listing),
  - testfat PASS 6/6 (/fat montato per UUID=4F4C4556),
  - la riga identita' di userdisk assegna uuid=C0FFEE01 alla lettera attesa
    (sdb nel run 1, sda nel run 2: le lettere cambiano, le chiavi no).

Uso: python3 scripts/test-uuid-reorder.py  (esce 0 se tutto PASS)
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
U2 = "C0FFEE01"
TIMEOUT = 240


def boot(log_path, swap):
    env = dict(os.environ, RUN_TESTS="1", SCHED_DEBUG="0")
    if swap:
        env["SWAP_DRIVES"] = "1"
    with open(log_path, "w") as log:
        try:
            subprocess.run(
                ["./run.sh"], cwd=ROOT, env=env,
                stdout=log, stderr=subprocess.STDOUT, timeout=TIMEOUT,
            )
        except subprocess.TimeoutExpired:
            # Atteso: run.sh lancia QEMU che gira per sempre (kill da timeout).
            pass
    with open(log_path) as f:
        return f.read()


def check(name, log, letter):
    ok = True

    def has(pat):
        return re.search(pat, log) is not None

    if not has(r"\[testfat\] PASS 6/6"):
        print(f"[{name}] MANCA testfat PASS 6/6 (/fat per UUID)")
        ok = False
    if not has(r"t36 UUID/LABEL \+ discovery stabile: PASS"):
        print(f"[{name}] MANCA t36 PASS")
        ok = False
    m = re.search(r"\[userdisk\] (sd[a-z]): handle=0x[0-9a-f]+ uuid=" + U2, log)
    if not m:
        print(f"[{name}] MANCA riga identita' uuid={U2}")
        ok = False
    elif m.group(1) != letter:
        print(f"[{name}] uuid={U2} su {m.group(1)}, atteso {letter}")
        ok = False
    else:
        print(f"[{name}] uuid={U2} su {letter} come atteso")
    if re.search(r"FAIL|PANIC|#.*FAULT", log):
        # Filtra i FAIL attesi nei nomi dei test negativi? No: qualunque
        # FAIL/PANIC/FAULT nel log e' un fallimento (i test negativi non
        # stampano queste parole nei loro rami di successo).
        bad = [l for l in log.splitlines()
               if re.search(r"FAIL|PANIC|#.*FAULT", l)]
        print(f"[{name}] {len(bad)} righe FAIL/PANIC/FAULT:")
        for l in bad[:10]:
            print(f"[{name}]   {l}")
        ok = False
    return ok


def main():
    ok = True
    log1 = "/tmp/reorder-normal.log"
    print("== boot ordine normale ==")
    log = boot(log1, swap=False)
    ok &= check("normal", log, "sdb")
    log2 = "/tmp/reorder-swapped.log"
    print("== boot ordine invertito (SWAP_DRIVES=1) ==")
    log = boot(log2, swap=True)
    ok &= check("swapped", log, "sda")
    print("REORDER-TEST " + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
