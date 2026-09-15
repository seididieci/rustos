#!/usr/bin/env python3
"""Test automatico interattivo della shell Velordor (Fase 9.4).

Avvia QEMU (serial su file + monitor su unix socket), invia comandi con
sendkey e verifica che la shell li esegua. La shell specchia l'output su
seriale (term_write_bytes -> print_string), quindi i risultati sono leggibili
dal log seriale senza bisogno della VGA.

Comandi testati: ls, cat, mkdir (cat usa hello.txt; '.' si invia come 'dot').
Fase 18.0: backspace a riga vuota non mangia il prompt (verifica via
screendump QEMU: eco visibile, cancel ripristina, backspace a vuoto = 0
byte diversi sull'ultima riga a meno del cursore).

Da eseguire dopo build-userland + build kernel (run.sh fa entrambe).
"""
import socket, subprocess, sys, time, os

KERNEL = "target/x86_64-unknown-none/release/velordor-kernel"
SERIAL = "/tmp/velordor-serial.log"
MON = "/tmp/velordor-mon.sock"
FAT = "userland/fs/fat.img"
# Secondo disco (stessa fixture di run.sh: UUID C0FFEE01, label SECOND):
# senza, t36 fallisce per ambiente (mount UUID impossibile) — osservato come
# FAIL nascosto perche' l'harness non controllava le righe della suite.
FAT2 = "userland/fs/fat2.img"

# Nomi sendkey VERIFICATI su QEMU 10.2.2 (il monitor risponde
# "invalid parameter" ai nomi ignoti — e lo script lo ignorerebbe in
# silenzio): 'period' NON esiste (il punto e' 'dot'), le MAIUSCOLE non
# esistono (si mandano come combo 'shift-x', verificato: 'H' invalido,
# 'shift-h' ok).
KEYMAP = {" ": "spc", ".": "dot", "-": "minus", "/": "slash"}
KEYMAP.update({chr(c): "shift-%s" % chr(c).lower() for c in range(ord("A"), ord("Z") + 1)})

SHOT0 = "/tmp/velordor-shot0.ppm"
SHOT1 = "/tmp/velordor-shot1.ppm"
SHOT2 = "/tmp/velordor-shot2.ppm"
SHOT3 = "/tmp/velordor-shot3.ppm"

def screendump(path: str):
    try: os.unlink(path)
    except FileNotFoundError: pass
    send_mon("screendump %s" % path, sleep=0.6)
    deadline = time.time() + 10
    while time.time() < deadline:
        if os.path.exists(path) and os.path.getsize(path) > 1000:
            time.sleep(0.3)
            return True
        time.sleep(0.2)
    return False

def load_ppm(path: str):
    with open(path, "rb") as f:
        data = f.read()
    assert data[:2] == b"P6", "screendump non PPM"
    # Header: P6 + commenti + "W H" + maxval, poi byte raw RGB.
    parts = []
    i = 2
    while len(parts) < 3:
        while data[i:i+1].isspace():
            i += 1
        if data[i:i+1] == b"#":
            while data[i:i+1] != b"\n":
                i += 1
            continue
        j = i
        while not data[j:j+1].isspace():
            j += 1
        parts.append(data[i:j])
        i = j
    while data[i:i+1].isspace():
        i += 1
    w, h = int(parts[0]), int(parts[1])
    return w, h, data[i:i + w * h * 3]

def lit_pixels(path: str):
    """Byte accesi nello screendump (testo bianco su nero), escluse le
    ultime 3 scanline (cursore lampeggiante)."""
    w, h, p = load_ppm(path)
    row = w * 3
    lit = 0
    for y in range(h - 3):
        off = y * row
        for k in range(row):
            if p[off + k] != 0:
                lit += 1
    return lit

def diff_masked(a: str, b: str):
    """Byte diversi tra due screendump, mascherando le ultime 3 scanline
    (cursore hardware lampeggiante: non e' contenuto)."""
    wa, ha, pa = load_ppm(a)
    wb, hb, pb = load_ppm(b)
    if (wa, ha, len(pa)) != (wb, hb, len(pb)):
        return -1
    row = wa * 3
    diff = 0
    for y in range(ha - 3):
        off = y * row
        for k in range(row):
            if pa[off + k] != pb[off + k]:
                diff += 1
    return diff

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

def mon_query(cmd: str) -> str:
    """Interroga il monitor HMP e ritorna la risposta (diagnostica)."""
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(MON)
        s.sendall(cmd.encode() + b"\n")
        time.sleep(0.3)
        try:
            data = s.recv(65536)
        except Exception:
            data = b""
        s.close()
        return data.decode(errors="replace")
    except Exception as e:
        return "<mon query failed: %s>" % e

def type_text(text: str, sleep=0.18):
    # Sleep generoso (era 0.12): a 8 tasti/s il guest perde scancode sotto
    # carico (buffer PS/2 a 1 byte, drain IRQ1+schedule ~40-60ms — overrun).
    # Umano digita piu' piano: e' un artefatto dell'harness, non dell'OS.
    # In piu': pausa di drain ogni 12 tasti (i comandi lunghi troncavano la
    # coda: "cp /fat/HELLO.TXT fatcopy.txt" arrivava come "cp /fat/.").
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
    for p in (SERIAL, MON):
        try: os.unlink(p)
        except FileNotFoundError: pass

    subprocess.run(["python3", "scripts/mkfat.py", FAT], check=True)
    subprocess.run(["python3", "scripts/mkfat.py", FAT2,
                    "--serial", "C0FFEE01", "--label", "SECOND",
                    "--marker", "second disk marker"], check=True)

    # Servizi da disco (Fase 21, stesso script di run.sh): /bin + /test.
    subprocess.run(["bash", "scripts/inject-bins.sh"], check=True)

    args = [
        "qemu-system-x86_64", "-m", "256M", "-display", "none",
        "-serial", "file:" + SERIAL,
        "-monitor", "unix:%s,server=on,wait=off" % MON,
        "-no-reboot", "-kernel", KERNEL, "-drive", "file=%s,format=raw,if=ide" % FAT,
        "-drive", "file=%s,format=raw,if=ide" % FAT2,
    ]
    # KVM se disponibile (host CI senza /dev/kvm resta su TCG): abbatte la
    # tassa di emulazione sui round-trip tastiera (IRQ1→kbd→tty→shell→prompt).
    if os.path.exists("/dev/kvm"):
        args[1:1] = ["-accel", "kvm"]
    q = subprocess.Popen(args)

    # Diagnostica accelerazione (KVM vs TCG): spiega i tempi del run.
    # La risposta contiene l'eco del comando: interessa la coda.
    time.sleep(1.0)
    kvm_info = mon_query("info kvm").replace("\r", "").strip().split("\n")
    kvm_tail = [l for l in kvm_info if "kvm" in l.lower()][-1:]
    print("info accel: %s" % (" | ".join(kvm_tail) if kvm_tail else kvm_info[-1:]))

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

        # Suite di boot (se il kernel bootato la include: dipende da come e'
        # stato compilato init): t36 richiede il secondo disco (FAT2 sopra).
        # Senza FAT2 falliva per ambiente restando invisibile — ora si asserisce
        # quando la suite e' presente, si salta in boot di produzione.
        data = read_log()
        if b"[usertests] SUMMARY" in data:
            found = b"t36 UUID/LABEL + discovery stabile: PASS" in data
            print(("PASS " if found else "FAIL ") + "t36 suite pre-shell")
            ok = ok and found

        # wait_prompt event-driven sul conteggio prompt CONSUMATO (non un
        # base locale: quello aspettava un prompt nuovo a shell gia' idle e
        # bruciava sempre il timeout, ~8 s a comando = 6 min a run). Aspetta
        # DAVVERO solo se un comando e' in volo (need_sync): dopo un interludio
        # senza Enter (screendump/backspace) non esiste alcun prompt nuovo e
        # il conteggio-da solo brucerebbe comunque il timeout (~8 s x3 = 25 s).
        # Definito qui (prima del primo uso nei blocchi ls/cat/...) e usato
        # anche da run()/run_out() sotto.
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
            dt = time.time() - t0
            if dt > 3:
                print("info slow wait_prompt %.1fs" % dt)

        # Attendi il primo prompt, poi esegui: ls<Enter>
        wait_prompt()
        type_text("ls")
        send_mon("sendkey ret")
        need_sync[0] = True
        time.sleep(1.2)
        data = read_log()
        found = (b"hello.txt" in data and b"test.txt" in data
                 and b"fat" in data and b"dev" in data)
        print(("PASS " if found else "FAIL ") + "ls / (hello.txt, test.txt, fat, dev)")
        ok = ok and found

        # Esegui: cat hello.txt<Enter> -> contenuto ramfs
        wait_prompt()
        type_text("cat hello.txt")
        send_mon("sendkey ret")
        need_sync[0] = True
        time.sleep(1.2)
        data = read_log()
        found = b"Hello from Velordor ramfs!" in data
        print(("PASS " if found else "FAIL ") + "cat hello.txt")
        ok = ok and found

        # Esegui: mkdir prova<Enter> poi ls<Enter> -> prova visibile
        wait_prompt()
        type_text("mkdir prova")
        send_mon("sendkey ret")
        need_sync[0] = True
        time.sleep(1.0)
        type_text("ls")
        send_mon("sendkey ret")
        need_sync[0] = True
        time.sleep(1.2)
        data = read_log()
        # "prova" non appare nel comando echo (nessun echo su seriale): deve
        # comparire SOLO come entry della directory. Conta le occorrenze.
        found = b"prova" in data
        print(("PASS " if found else "FAIL ") + "mkdir prova visibile in ls")
        ok = ok and found

        # Fase 18.0: backspace a riga vuota non mangia il prompt. La VGA e'
        # statica al prompt: shot0 riferimento; "q" deve cambiare lo schermo
        # (controllo positivo: screendump sensibile); backspace torna a shot0;
        # altri 3 backspace a riga vuota devono lasciare tutto identico
        # (pre-fix mangiavano "$ ").
        wait_prompt()
        time.sleep(0.2)  # come prima: 0.3 di wait_prompt + 0.2 = 0.5
        for p in (SHOT0, SHOT1, SHOT2, SHOT3):
            try: os.unlink(p)
            except FileNotFoundError: pass
        got0 = screendump(SHOT0)
        type_text("q")
        time.sleep(0.8)
        got1 = screendump(SHOT1)
        send_mon("sendkey backspace")
        time.sleep(0.8)
        got2 = screendump(SHOT2)
        for _ in range(3):
            send_mon("sendkey backspace")
        time.sleep(0.8)
        got3 = screendump(SHOT3)
        if not (got0 and got1 and got2 and got3):
            print("FAIL backspace: screendump mancati")
            ok = False
        else:
            d1 = diff_masked(SHOT0, SHOT1)
            d2 = diff_masked(SHOT0, SHOT2)
            d3 = diff_masked(SHOT0, SHOT3)
            print("info backspace: diff q=%d erase=%d empty=%d" % (d1, d2, d3))
            # La 'q' accende solo i pixel del glifo (~96 byte): soglia bassa
            # ma > 0 (controllo positivo che lo screendump sia sensibile).
            found = d1 > 50 and d2 <= 64 and d3 <= 64
            print(("PASS " if found else "FAIL ")
                  + "backspace: eco ok, cancel ok, prompt intatto a riga vuota")
            ok = ok and found

        # Fase 18.1: builtin (echo/wc/hexdump/cd/pwd/kill/clear).
        def run(cmd: str, sleep=1.0):
            t0 = time.time()
            wait_prompt()
            type_text(cmd)
            send_mon("sendkey ret")
            time.sleep(sleep)
            need_sync[0] = True  # comando in volo: il prossimo wait sincronizza
            dt = time.time() - t0
            if dt > 10:
                print("info slow run %.1fs: %s" % (dt, cmd))

        def run_out(cmd: str, sleep=1.0):
            """Esegue e ritorna SOLO l'output nuovo (coda del log): serve per
            gli assert di assenza (il log cumulativo contiene gia' tutto)."""
            t0 = time.time()
            wait_prompt()
            mark = len(read_log())
            type_text(cmd)
            send_mon("sendkey ret")
            time.sleep(sleep)
            need_sync[0] = True  # come run()
            dt = time.time() - t0
            if dt > 10:
                print("info slow run_out %.1fs: %s" % (dt, cmd))
            return read_log()[mark:]

        # echo
        run("echo hello world")
        data = read_log()
        found = b"hello world" in data
        print(("PASS " if found else "FAIL ") + "echo hello world")
        ok = ok and found

        # wc su path relativo (cwd=/): "Hello from Velordor ramfs!\n"
        run("wc hello.txt")
        data = read_log()
        found = b"1 4 25 hello.txt" in data
        print(("PASS " if found else "FAIL ") + "wc hello.txt (=1 4 25)")
        ok = ok and found

        # hexdump: "Hello" = 48 65 6c 6c 6f
        run("hexdump hello.txt")
        data = read_log()
        found = b"48 65 6c 6c 6f" in data
        print(("PASS " if found else "FAIL ") + "hexdump hello.txt")
        ok = ok and found

        # cd/pwd + path relativi (prova esiste dal test mkdir)
        run("cd prova")
        run("pwd")
        data = read_log()
        found = b"/prova" in data
        print(("PASS " if found else "FAIL ") + "cd prova + pwd")
        ok = ok and found
        run("touch inner.txt")
        run("ls")
        data = read_log()
        found = b"inner.txt" in data
        print(("PASS " if found else "FAIL ") + "path relativo in ls")
        ok = ok and found
        run("cd ..")
        run("ls prova")
        data = read_log()
        found = b"inner.txt" in data
        print(("PASS " if found else "FAIL ") + "cd .. + ls prova")
        ok = ok and found

        # Fase 19.2 (stretch ls -l): tipo + size via R_STAT (1 RT per entry).
        out = run_out("ls -l")
        found = b"- 25 hello.txt" in out
        print(("PASS " if found else "FAIL ") + "ls -l (tipo + size)")
        ok = ok and found
        run("mkdir lldir")
        out = run_out("ls -l")
        found = b"d 0 lldir" in out
        print(("PASS " if found else "FAIL ") + "ls -l (marcatore dir)")
        ok = ok and found
        run("rmdir lldir")
        out = run_out("ls -l /fat")
        found = b"- 25 HELLO.TXT" in out and b"(ro)" not in out
        print(("PASS " if found else "FAIL ") + "ls -l /fat (scrivibile)")
        ok = ok and found

        # kill: nome ignoto, pid inesistente, init non killabile (rifiuto)
        run("kill nosuchsvc")
        run("kill 99999")
        run("kill init")
        data = read_log()
        found = (b"kill: unknown pid/service" in data
                 and data.count(b"kill: failed") >= 2)
        print(("PASS " if found else "FAIL ") + "kill errori + init rifiutato")
        ok = ok and found

        # Fase 19.1: ps tabellare (header + init + shell stessa in lista).
        out = run_out("ps")
        found = (b"PID  NAME" in out and b"userinit" in out
                 and b"usershell" in out)
        print(("PASS " if found else "FAIL ") + "ps tabellare (init+shell)")
        ok = ok and found

        # Fase 18.2: rm/cp/mv/rmdir (R_DELETE: ramfs si, /fat no).
        # rmdir su dir NON vuota (prova contiene inner.txt dai test cd).
        out = run_out("rmdir prova")
        found = b"rmdir: failed" in out
        print(("PASS " if found else "FAIL ") + "rmdir rifiutata su dir piena")
        ok = ok and found

        # rm file + read-fail dopo.
        run("rm prova/inner.txt")
        out = run_out("ls prova")
        found = b"inner.txt" not in out
        print(("PASS " if found else "FAIL ") + "rm prova/inner.txt sparito da ls")
        ok = ok and found
        out = run_out("cat prova/inner.txt")
        found = b"cannot open" in out
        print(("PASS " if found else "FAIL ") + "cat dopo rm fallisce")
        ok = ok and found

        # rmdir su dir ormai vuota + sparizione.
        run("rmdir prova")
        out = run_out("ls")
        found = b"prova" not in out
        print(("PASS " if found else "FAIL ") + "rmdir prova vuota")
        ok = ok and found

        # cp ramfs->ramfs con contenuto verificato.
        run("cp hello.txt copy.txt")
        out = run_out("cat copy.txt")
        found = b"Hello from Velordor ramfs!" in out
        print(("PASS " if found else "FAIL ") + "cp hello.txt copy.txt")
        ok = ok and found

        # mv = cp+rm: contenuto preservato, sorgente sparita.
        run("mv copy.txt moved.txt")
        out = run_out("cat moved.txt")
        found = b"Hello from Velordor ramfs!" in out
        print(("PASS " if found else "FAIL ") + "mv preserva contenuto")
        ok = ok and found
        out = run_out("cat copy.txt")
        found = b"cannot open" in out
        print(("PASS " if found else "FAIL ") + "mv rimuove sorgente")
        ok = ok and found
        run("rm moved.txt")

        # cp da /fat verso ramfs (sorgente sempre leggibile).
        run("cp /fat/HELLO.TXT fatcopy.txt")
        out = run_out("cat fatcopy.txt")
        found = b"Hello from Velordor FAT32!" in out
        print(("PASS " if found else "FAIL ") + "cp /fat/HELLO.TXT -> ramfs")
        ok = ok and found
        run("rm fatcopy.txt")

        # Fase 20 (/fat scrivibile): cp verso /fat CREA + read-back; rm resta
        # rifiutato (niente unlink su FAT, fuori scope); HELLO.TXT intatta.
        run("cp hello.txt /fat/shellw.txt")
        out = run_out("cat /fat/shellw.txt")
        found = b"Hello from Velordor ramfs!" in out
        print(("PASS " if found else "FAIL ") + "cp verso /fat + read-back")
        ok = ok and found
        out = run_out("rm /fat/shellw.txt")
        found = b"cannot remove" in out
        print(("PASS " if found else "FAIL ") + "rm su /fat ancora rifiutato")
        ok = ok and found
        out = run_out("rm /fat/HELLO.TXT")
        found = b"cannot remove" in out
        print(("PASS " if found else "FAIL ") + "rm su /fat rifiutato")
        ok = ok and found
        out = run_out("cat /fat/HELLO.TXT")
        found = b"Hello from Velordor FAT32!" in out
        print(("PASS " if found else "FAIL ") + "/fat/HELLO.TXT intatto")
        ok = ok and found

        # clear: scherma testo, pulisce, shell resta viva        run("echo marker123")
        wait_prompt()
        if not screendump(SHOT0):
            print("FAIL clear: screendump pre mancato")
            ok = False
        else:
            before = lit_pixels(SHOT0)
            run("clear")
            wait_prompt()
            if not screendump(SHOT1):
                print("FAIL clear: screendump post mancato")
                ok = False
            else:
                after = lit_pixels(SHOT1)
                print("info clear: lit prima=%d dopo=%d" % (before, after))
                found = after < 3000 and before - after > 3000
                print(("PASS " if found else "FAIL ") + "clear pulisce la VGA")
                ok = ok and found
            run("echo alive")
            data = read_log()
            found = b"alive" in data
            print(("PASS " if found else "FAIL ") + "shell viva dopo clear")
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
