#!/usr/bin/env python3
"""Smoke Fase 41: parser shell quote-aware (RICOSTRUITO passo 2).

Lo smoke41.py citato nel commit WIP e' andato perso (mai committato): questo
file lo ricostruisce da descrizione commit + scope Fase 41. Harness identico a
test-shell.py (QEMU + sendkey + seriale), 19 check semantici + fase di sonda
KEYMAP che auto-calibra i nomi QEMU (validita' da reply monitor, consegna da
eco quotato nel guest).

Fix driver coperto qui (indagine completa): pc-keyboard 0.7 mappa 0x2B su
Oem7 ma Us104Key non lo gestisce -> usertty usa Us104Fix (Oem7 = \\ / |).
Senza, '\\' e '|' non arrivano mai (nomi QEMU validi ma byte persi).

Uso: python3 scripts/smoke41.py [--probe-only]
"""
import socket, subprocess, sys, time, os, re

KERNEL = "target/x86_64-unknown-none/release/velordor-kernel"
SERIAL = "/tmp/velordor-smoke41-serial.log"
MON = "/tmp/velordor-smoke41-mon.sock"
FAT = "userland/fs/fat.img"
FAT2 = "userland/fs/fat2.img"

KEYMAP = {" ": "spc", ".": "dot", "-": "minus", "/": "slash", "&": "shift-7",
          ">": "shift-dot", "<": "shift-comma", "!": "shift-1"}
KEYMAP.update({chr(c): "shift-%s" % chr(c).lower() for c in range(ord("A"), ord("Z") + 1)})

# Candidati in ordine di preferenza (il primo valido vince, verificato live).
CANDIDATES = {
    "'": ["apostrophe"],
    '"': ["shift-apostrophe"],
    "\\": ["backslash"],
    "|": ["shift-backslash"],
    ";": ["semicolon"],
    "$": ["shift-4"],
    "*": ["shift-8", "asterisk"],
    "?": ["shift-slash"],
    "#": ["shift-3"],
    "~": ["shift-grave_accent", "shift-backquote"],
    "=": ["equal"],
    "_": ["shift-minus"],
    ":": ["shift-semicolon"],
    "{": ["shift-bracket_left", "shift-bracket-left"],
    "}": ["shift-bracket_right", "shift-bracket-right"],
    "`": ["grave_accent", "backquote"],
}

QEMU_PROC = [None]

def qemu_alive():
    return QEMU_PROC[0] is not None and QEMU_PROC[0].poll() is None

def mon_raw(cmd):
    """Una transazione HMP con retry (fix flakiness ConnectionRefused)."""
    last = None
    for _ in range(5):
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(5)
            s.connect(MON)
            s.sendall(cmd.encode() + b"\n")
            time.sleep(0.05)
            try:
                data = s.recv(65536)
            except Exception:
                data = b""
            s.close()
            return data.decode(errors="replace")
        except OSError as e:
            last = e
            if not qemu_alive():
                raise RuntimeError("QEMU morto durante '%s': %s" % (cmd, e))
            time.sleep(0.5)
    raise RuntimeError("monitor irraggiungibile per '%s': %s" % (cmd, last))

def send_mon(cmd, sleep=0.12):
    mon_raw(cmd)
    time.sleep(sleep)

def sendkey_valid(name):
    r = mon_raw("sendkey %s" % name)
    return "invalid parameter" not in r

def calibrate():
    """Risolvi i nomi QEMU una volta sola; ritorna n. di tasti validi digitati
    (da cancellare con backspace prima di partire)."""
    typed = 0
    for ch, names in CANDIDATES.items():
        hit = None
        for n in names:
            if sendkey_valid(n):
                hit = n
                break
        if hit is None:
            print("info KEYMAP: nessun nome valido per %r (saltato)" % ch)
            continue
        KEYMAP[ch] = hit
        # Solo i char che servono allo smoke/limiti digitano qui (tutti tranne
        # quelli gia' noti): l'eco sporca la riga corrente, cancellata dopo.
        if ch not in (" ", ".", "-", "/", "&", ">", "<"):
            typed += 1
        print("info KEYMAP: %r -> %s" % (ch, hit))
    return typed

def type_text(text, sleep=0.18):
    for i, ch in enumerate(text):
        send_mon("sendkey %s" % KEYMAP.get(ch, ch), sleep=sleep)
        if (i + 1) % 12 == 0:
            time.sleep(1.0)

def read_log():
    try:
        with open(SERIAL, "rb") as f:
            return f.read()
    except FileNotFoundError:
        return b""

def count_prompts(data):
    return data.count(b"$ ")

def main():
    probe_only = "--probe-only" in sys.argv
    for p in (SERIAL, MON):
        try: os.unlink(p)
        except FileNotFoundError: pass

    subprocess.run(["python3", "scripts/mkfat.py", FAT], check=True)
    subprocess.run(["python3", "scripts/mkfat.py", FAT2,
                    "--serial", "C0FFEE01", "--label", "SECOND",
                    "--marker", "second disk marker"], check=True)
    subprocess.run(["bash", "scripts/inject-bins.sh"], check=True)

    args = ["qemu-system-x86_64", "-m", "256M", "-display", "none",
            "-serial", "file:" + SERIAL,
            "-monitor", "unix:%s,server=on,wait=off" % MON,
            "-no-reboot", "-kernel", KERNEL,
            "-drive", "file=%s,format=raw,if=ide" % FAT,
            "-drive", "file=%s,format=raw,if=ide" % FAT2]
    if os.path.exists("/dev/kvm"):
        args[1:1] = ["-accel", "kvm"]
    q = subprocess.Popen(args)
    QEMU_PROC[0] = q
    try:
        deadline = time.time() + 60
        ready = False
        while time.time() < deadline:
            data = read_log()
            if (b"[shell] starting" in data
                    and b"registered mount '/dev/input'" in data
                    and b"$ " in data):
                ready = True
                break
            if not qemu_alive():
                print("FAIL: QEMU morto prima della shell")
                print(read_log().decode(errors="replace")[-2000:])
                return 1
            time.sleep(0.4)
        if not ready:
            print("FAIL: shell non pronta")
            return 1

        prompt_seen = [0]
        need_sync = [False]
        def wait_prompt(timeout=8):
            t0 = time.time()
            if need_sync[0]:
                deadline = time.time() + timeout
                while count_prompts(read_log()) <= prompt_seen[0] and time.time() < deadline:
                    time.sleep(0.2)
                need_sync[0] = False
            time.sleep(0.3)
            prompt_seen[0] = count_prompts(read_log())

        def run(cmd, sleep=1.0):
            wait_prompt()
            type_text(cmd)
            send_mon("sendkey ret")
            time.sleep(sleep)
            need_sync[0] = True

        def run_out(cmd, sleep=1.0):
            wait_prompt()
            mark = len(read_log())
            type_text(cmd)
            send_mon("sendkey ret")
            time.sleep(sleep)
            need_sync[0] = True
            return read_log()[mark:]

        wait_prompt()
        # Sonda KEYMAP: digita un char per nome valido, poi cancella tutto.
        typed = calibrate()
        if probe_only:
            print("PROBE-ONLY ok")
            return 0
        for _ in range(typed):
            send_mon("sendkey backspace", sleep=0.1)
        send_mon("sendkey ret")
        time.sleep(0.8)
        need_sync[0] = True
        wait_prompt()

        # Consegna \\ e | (il bug driver): eco quotato, niente espansione.
        fails = []
        def check(name, cond):
            print(("PASS " if cond else "FAIL ") + name)
            if not cond:
                fails.append(name)

        out = run_out("echo '\\'")
        check("consegna backslash", b"\\" in out)
        out = run_out("echo '|'")
        check("consegna pipe", b"|" in out)

        # Semantica Fase 41 (19 check totali con le 2 consegne sopra).
        out = run_out("echo 'a b'")
        check("single-quote letterale", b"a b" in out)
        out = run_out("echo '>'")
        check("redirect in quote non interpretato", b">" in out)
        out = run_out('echo "a   b"')
        check("double-quote raggruppa", b"a   b" in out)
        out = run_out("echo a\\ b")
        check("escape spazio", b"a b" in out)
        out = run_out("echo a\\;b")
        check("escape punto-e-virgola", b"a;b" in out)
        out = run_out("# commento puro")
        check("commento riga intera", b"$ " in read_log())
        out = run_out("echo hi # trailing")
        check("commento trailing", b"hi" in out)
        run("export SMOKE41=ok")
        out = run_out("echo $SMOKE41")
        check("export + $VAR", b"ok" in out)
        out = run_out("echo ${SMOKE41}!")
        check("${VAR}", b"ok!" in out)
        out = run_out("echo pre$UNSET41Xpost")
        lines = out.split(b"\n")
        # `pre` letterale resta, la variabile unset sparisce: riga == "pre".
        check("$UNSET vuoto", b"UNSET41X" not in out
              and len(lines) > 0 and lines[0].strip() == b"pre")
        run("cat missing41")
        out = run_out("echo $?")
        check("$? dopo errore (=1)", b"1" in out)
        out = run_out("echo ok && echo yes2")
        check("&& corto", b"ok" in out and b"yes2" in out)
        out = run_out("cat missing41 && echo no41")
        check("&& short-circuit", b"no41" not in out)
        out = run_out("cat missing41 || echo yes41")
        check("|| scatta", b"yes41" in out)
        out = run_out("echo ok41 || echo no41b")
        check("|| salta a successo", b"ok41" in out and b"no41b" not in out)
        out = run_out("echo a; echo b41")
        check("punto-e-virgola", b"b41" in out)
        out = run_out("echo a | echo b")
        check("pipe rifiutata (Fase 42)", b"pipe non supportata" in out)

        print("smoke41: %d/21 PASS" % (21 - len(fails)))
        if fails:
            print("FAIL: " + ", ".join(fails))
            return 1
        return 0
    finally:
        q.terminate()
        try: q.wait(timeout=5)
        except subprocess.TimeoutExpired: q.kill()

if __name__ == "__main__":
    sys.exit(main())
