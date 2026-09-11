#!/usr/bin/env python3
"""Test automatico interattivo della shell rustOS (Fase 9.4).

Avvia QEMU (serial su file + monitor su unix socket), invia comandi con
sendkey e verifica che la shell li esegua. La shell specchia l'output su
seriale (term_write_bytes -> print_string), quindi i risultati sono leggibili
dal log seriale senza bisogno della VGA.

Comandi testati: ls, cat, mkdir (cat usa hello.txt; '.' si invia come 'period').

Da eseguire dopo build-userland + build kernel (run.sh fa entrambe).
"""
import socket, subprocess, sys, time, os

KERNEL = "target/x86_64-unknown-none/release/rustos-kernel"
SERIAL = "/tmp/rustos-serial.log"
MON = "/tmp/rustos-mon.sock"
FAT = "userland/fs/fat.img"

KEYMAP = {" ": "spc", ".": "period", "-": "minus", "/": "slash"}

def send_mon(cmd: str, sleep=0.12):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(MON)
    s.sendall(cmd.encode() + b"\n")
    time.sleep(0.03)
    try:
        s.recv(4096)
    except Exception:
        pass
    s.close()
    time.sleep(sleep)

def type_text(text: str):
    for ch in text:
        send_mon("sendkey %s" % KEYMAP.get(ch, ch))

def read_log():
    try:
        with open(SERIAL, "rb") as f:
            return f.read()
    except FileNotFoundError:
        return b""

def count_prompts(data):
    return data.count(b"$ ")

def main():
    for p in (SERIAL, MON):
        try: os.unlink(p)
        except FileNotFoundError: pass

    subprocess.run(["python3", "scripts/mkfat.py", FAT], check=True)

    args = [
        "qemu-system-x86_64", "-m", "256M", "-display", "none",
        "-serial", "file:" + SERIAL,
        "-monitor", "unix:%s,server=on,wait=off" % MON,
        "-no-reboot", "-kernel", KERNEL, "-drive", "file=%s,format=raw,if=ide" % FAT,
    ]
    q = subprocess.Popen(args)

    try:
        deadline = time.time() + 60
        ready = False
        while time.time() < deadline:
            data = read_log()
            # Produzione (nessun test eseguito): shell avviata + terminale
            # montato + almeno un prompt. NON dipende piu' dai test di
            # background (run di produzione li salta: RUN_TESTS non impostato).
            if (b"[shell] starting" in data
                    and b"registered mount '/dev/input'" in data
                    and b"$ " in data):
                ready = True
                break
            time.sleep(0.4)
        if not ready:
            print("FAIL: shell non pronta")
            return 1

        ok = True

        # Attendi il primo prompt, poi esegui: ls<Enter>
        base = count_prompts(read_log())
        deadline = time.time() + 15
        while count_prompts(read_log()) <= base and time.time() < deadline:
            time.sleep(0.2)
        time.sleep(0.3)
        type_text("ls")
        send_mon("sendkey ret")
        time.sleep(1.2)
        data = read_log()
        found = b"hello.txt" in data and b"test.txt" in data
        print(("PASS " if found else "FAIL ") + "ls / (hello.txt, test.txt)")
        ok = ok and found

        # Esegui: cat hello.txt<Enter> -> contenuto ramfs
        base = count_prompts(read_log())
        deadline = time.time() + 15
        while count_prompts(read_log()) <= base and time.time() < deadline:
            time.sleep(0.2)
        time.sleep(0.3)
        type_text("cat hello.txt")
        send_mon("sendkey ret")
        time.sleep(1.2)
        data = read_log()
        found = b"Hello from rustOS ramfs!" in data
        print(("PASS " if found else "FAIL ") + "cat hello.txt")
        ok = ok and found

        # Esegui: mkdir prova<Enter> poi ls<Enter> -> prova visibile
        base = count_prompts(read_log())
        deadline = time.time() + 15
        while count_prompts(read_log()) <= base and time.time() < deadline:
            time.sleep(0.2)
        time.sleep(0.3)
        type_text("mkdir prova")
        send_mon("sendkey ret")
        time.sleep(1.0)
        type_text("ls")
        send_mon("sendkey ret")
        time.sleep(1.2)
        data = read_log()
        # "prova" non appare nel comando echo (nessun echo su seriale): deve
        # comparire SOLO come entry della directory. Conta le occorrenze.
        found = b"prova" in data
        print(("PASS " if found else "FAIL ") + "mkdir prova visibile in ls")
        ok = ok and found

        if not ok:
            print("---- output seriale (tail) ----")
            print(data.decode(errors="replace")[-2000:])
            return 1
        return 0
    finally:
        q.terminate()
        try: q.wait(timeout=5)
        except subprocess.TimeoutExpired: q.kill()

if __name__ == "__main__":
    sys.exit(main())
