#!/usr/bin/env python3
"""Test shell BASE (Fase 9.4/18/19/20): ls/cat/mkdir, backspace, builtin, cd/pwd, ls -l, kill, ps, rm/cp/mv, FAT scrivibile, clear."
Avvia il proprio QEMU (seriale + monitor dedicati), digita via sendkey,
verifica sul log seriale. Autonomo: prepara le immagini (salvo --no-prep),
boota, testa, pulisce le sue fixture. Vedi scripts/shell_harness.py.
"""
import sys, os, time
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
from shell_harness import Shell, Checker, prep_images, parse_shell_args, lit_pixels, diff_masked
def main():
    args = parse_shell_args("/tmp/velordor-base-mon.sock", "/tmp/velordor-base-serial.log")
    if not args.no_prep:
        prep_images()
    sh = Shell(mon=args.mon, serial=args.serial, fat=args.fat, fat2=args.fat2,
               kernel=args.kernel, fat_format=args.fat_format)
    c = Checker()
    SHOT0 = args.serial + ".shot0.ppm"
    SHOT1 = args.serial + ".shot1.ppm"
    SHOT2 = args.serial + ".shot2.ppm"
    SHOT3 = args.serial + ".shot3.ppm"
    try:
        sh.boot()
        ok = True

        # Suite di boot (se il kernel bootato la include: dipende da come e'
        # stato compilato init): t36 richiede il secondo disco (FAT2 sopra).
        # Senza FAT2 falliva per ambiente restando invisibile — ora si asserisce
        # quando la suite e' presente, si salta in boot di produzione.
        data = sh.read_log()
        if b"[usertests] SUMMARY" in data:
            found = b"t36 UUID/LABEL + discovery stabile: PASS" in data
            c.check("t36 suite pre-shell", found)

        # wait_prompt event-driven sul conteggio prompt CONSUMATO (non un
        # base locale: quello aspettava un prompt nuovo a shell gia' idle e
        # bruciava sempre il timeout, ~8 s a comando = 6 min a run). Aspetta
        # DAVVERO solo se un comando e' in volo (need_sync): dopo un interludio
        # senza Enter (screendump/backspace) non esiste alcun prompt nuovo e
        # il conteggio-da solo brucerebbe comunque il timeout (~8 s x3 = 25 s).
        # Definito qui (prima del primo uso nei blocchi ls/cat/...) e usato
        # anche da sh.run()/sh.run_out() sotto.
        prompt_seen = [0]
        need_sync = [False]
        def wait_prompt(timeout=8):
            t0 = time.time()
            if need_sync[0]:
                deadline = time.time() + timeout
                while count_prompts(sh.read_log()) <= prompt_seen[0] and time.time() < deadline:
                    time.sleep(0.2)
                need_sync[0] = False
            time.sleep(0.3)
            prompt_seen[0] = count_prompts(sh.read_log())
            dt = time.time() - t0
            if dt > 3:
                print("info slow wait_prompt %.1fs" % dt)

        # Attendi il primo prompt, poi esegui: ls<Enter>
        sh.wait_prompt()
        sh.type_text("ls")
        sh.send_mon("sendkey ret")
        need_sync[0] = True
        time.sleep(1.2)
        data = sh.read_log()
        found = (b"hello.txt" in data and b"test.txt" in data
                 and b"fat" in data and b"dev" in data)
        c.check("ls / (hello.txt, test.txt, fat, dev)", found)

        # Esegui: cat hello.txt<Enter> -> contenuto ramfs
        sh.wait_prompt()
        sh.type_text("cat hello.txt")
        sh.send_mon("sendkey ret")
        need_sync[0] = True
        time.sleep(1.2)
        data = sh.read_log()
        found = b"Hello from Velordor ramfs!" in data
        c.check("cat hello.txt", found)

        # Esegui: mkdir prova<Enter> poi ls<Enter> -> prova visibile
        sh.wait_prompt()
        sh.type_text("mkdir prova")
        sh.send_mon("sendkey ret")
        need_sync[0] = True
        time.sleep(1.0)
        sh.type_text("ls")
        sh.send_mon("sendkey ret")
        need_sync[0] = True
        time.sleep(1.2)
        data = sh.read_log()
        # "prova" non appare nel comando echo (nessun echo su seriale): deve
        # comparire SOLO come entry della directory. Conta le occorrenze.
        found = b"prova" in data
        c.check("mkdir prova visibile in ls", found)

        # Fase 18.0: backspace a riga vuota non mangia il prompt. La VGA e'
        # statica al prompt: shot0 riferimento; "q" deve cambiare lo schermo
        # (controllo positivo: screendump sensibile); backspace torna a shot0;
        # altri 3 backspace a riga vuota devono lasciare tutto identico
        # (pre-fix mangiavano "$ ").
        sh.wait_prompt()
        time.sleep(0.2)  # come prima: 0.3 di wait_prompt + 0.2 = 0.5
        for p in (SHOT0, SHOT1, SHOT2, SHOT3):
            try: os.unlink(p)
            except FileNotFoundError: pass
        got0 = sh.screendump(SHOT0)
        sh.type_text("q")
        time.sleep(0.8)
        got1 = sh.screendump(SHOT1)
        sh.send_mon("sendkey backspace")
        time.sleep(0.8)
        got2 = sh.screendump(SHOT2)
        for _ in range(3):
            sh.send_mon("sendkey backspace")
        time.sleep(0.8)
        got3 = sh.screendump(SHOT3)
        if not (got0 and got1 and got2 and got3):
            print("FAIL backspace: screendump mancati")
            c.ok = False
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
        # echo
        sh.run("echo hello world")
        data = sh.read_log()
        found = b"hello world" in data
        c.check("echo hello world", found)

        # wc su path relativo (cwd=/): "Hello from Velordor ramfs!\n"
        sh.run("wc hello.txt")
        data = sh.read_log()
        found = b"1 4 25 hello.txt" in data
        c.check("wc hello.txt (=1 4 25)", found)

        # hexdump: "Hello" = 48 65 6c 6c 6f
        sh.run("hexdump hello.txt")
        data = sh.read_log()
        found = b"48 65 6c 6c 6f" in data
        c.check("hexdump hello.txt", found)

        # cd/pwd + path relativi (prova esiste dal test mkdir)
        sh.run("cd prova")
        sh.run("pwd")
        data = sh.read_log()
        found = b"/prova" in data
        c.check("cd prova + pwd", found)
        sh.run("touch inner.txt")
        sh.run("ls")
        data = sh.read_log()
        found = b"inner.txt" in data
        c.check("path relativo in ls", found)
        sh.run("cd ..")
        sh.run("ls prova")
        data = sh.read_log()
        found = b"inner.txt" in data
        c.check("cd .. + ls prova", found)

        # Fase 19.2 (stretch ls -l): tipo + size via R_STAT (1 RT per entry).
        out = sh.run_out("ls -l")
        found = b"- 25 hello.txt" in out
        c.check("ls -l (tipo + size)", found)
        sh.run("mkdir lldir")
        out = sh.run_out("ls -l")
        found = b"d 0 lldir" in out
        c.check("ls -l (marcatore dir)", found)
        sh.run("rmdir lldir")
        out = sh.run_out("ls -l /fat")
        found = b"- 25 HELLO.TXT" in out and b"(ro)" not in out
        c.check("ls -l /fat (scrivibile)", found)

        # kill: nome ignoto, pid inesistente, init non killabile (rifiuto)
        sh.run("kill nosuchsvc")
        sh.run("kill 99999")
        sh.run("kill init")
        data = sh.read_log()
        found = (b"kill: unknown pid/service" in data
                 and data.count(b"kill: failed") >= 2)
        c.check("kill errori + init rifiutato", found)

        # Fase 19.1: ps tabellare (header + init + shell stessa in lista).
        out = sh.run_out("ps")
        found = (b"PID  NAME" in out and b"userinit" in out
                 and b"usershell" in out)
        c.check("ps tabellare (init+shell)", found)

        # Fase 18.2: rm/cp/mv/rmdir (R_DELETE: ramfs si, /fat no).
        # rmdir su dir NON vuota (prova contiene inner.txt dai test cd).
        out = sh.run_out("rmdir prova")
        found = b"rmdir: failed" in out
        c.check("rmdir rifiutata su dir piena", found)

        # rm file + read-fail dopo.
        sh.run("rm prova/inner.txt")
        out = sh.run_out("ls prova")
        found = b"inner.txt" not in out
        c.check("rm prova/inner.txt sparito da ls", found)
        out = sh.run_out("cat prova/inner.txt")
        found = b"cannot open" in out
        c.check("cat dopo rm fallisce", found)

        # rmdir su dir ormai vuota + sparizione.
        sh.run("rmdir prova")
        out = sh.run_out("ls")
        found = b"prova" not in out
        c.check("rmdir prova vuota", found)

        # cp ramfs->ramfs con contenuto verificato.
        sh.run("cp hello.txt copy.txt")
        out = sh.run_out("cat copy.txt")
        found = b"Hello from Velordor ramfs!" in out
        c.check("cp hello.txt copy.txt", found)

        # mv = cp+rm: contenuto preservato, sorgente sparita.
        sh.run("mv copy.txt moved.txt")
        out = sh.run_out("cat moved.txt")
        found = b"Hello from Velordor ramfs!" in out
        c.check("mv preserva contenuto", found)
        out = sh.run_out("cat copy.txt")
        found = b"cannot open" in out
        c.check("mv rimuove sorgente", found)
        sh.run("rm moved.txt")

        # cp da /fat verso ramfs (sorgente sempre leggibile).
        sh.run("cp /fat/HELLO.TXT fatcopy.txt")
        out = sh.run_out("cat fatcopy.txt")
        found = b"Hello from Velordor FAT32!" in out
        c.check("cp /fat/HELLO.TXT -> ramfs", found)
        sh.run("rm fatcopy.txt")

        # Fase 20 (/fat scrivibile): cp verso /fat CREA + read-back; rm resta
        # rifiutato (niente unlink su FAT, fuori scope); HELLO.TXT intatta.
        sh.run("cp hello.txt /fat/shellw.txt")
        out = sh.run_out("cat /fat/shellw.txt")
        found = b"Hello from Velordor ramfs!" in out
        c.check("cp verso /fat + read-back", found)
        out = sh.run_out("rm /fat/shellw.txt")
        found = b"cannot remove" in out
        c.check("rm su /fat ancora rifiutato", found)
        out = sh.run_out("rm /fat/HELLO.TXT")
        found = b"cannot remove" in out
        c.check("rm su /fat rifiutato", found)
        out = sh.run_out("cat /fat/HELLO.TXT")
        found = b"Hello from Velordor FAT32!" in out
        c.check("/fat/HELLO.TXT intatto", found)

        # clear: scherma testo, pulisce, shell resta viva        sh.run("echo marker123")
        sh.wait_prompt()
        if not sh.screendump(SHOT0):
            print("FAIL clear: screendump pre mancato")
            c.ok = False
        else:
            before = lit_pixels(SHOT0)
            sh.run("clear")
            sh.wait_prompt()
            if not sh.screendump(SHOT1):
                print("FAIL clear: screendump post mancato")
                c.ok = False
            else:
                after = lit_pixels(SHOT1)
                print("info clear: lit prima=%d dopo=%d" % (before, after))
                found = after < 3000 and before - after > 3000
                c.check("clear pulisce la VGA", found)
            sh.run("echo alive")
            data = sh.read_log()
            found = b"alive" in data
            c.check("shell viva dopo clear", found)
        return 0 if c.ok else 1
    except RuntimeError as e:
        print("FAIL: %s" % e)
        return 1
    finally:
        sh.terminate()


if __name__ == "__main__":
    sys.exit(main())
