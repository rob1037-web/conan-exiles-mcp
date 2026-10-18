"""Pilote mcp-server.exe en stdio (lecture seule, validation de la phase 4b): appels séquentiels dans UNE
session, durée mesurée (horloge monotone), arrêt au premier KO ou à la première absence de réponse.

usage: mcp_seq.py <tag> '<[["outil",{args}],...]>' [maxchars]
Variables: CONAN_EXE (défaut /mnt/d/ConanMod/bin/mcp-server.exe), MOD_ROOT (défaut: dossier du mod de test),
CONAN_VAL_OUT (dossier des sorties complètes, défaut <tmp>/conan_val).
Les sorties complètes sont écrites dans <CONAN_VAL_OUT>/<tag>_<n>_<outil>.json.
"""
import json, subprocess, os, sys, tempfile, time
S = os.environ.get("CONAN_VAL_OUT") or os.path.join(tempfile.gettempdir(), "conan_val")
os.makedirs(S, exist_ok=True)
tag, calls = sys.argv[1], json.loads(sys.argv[2])
maxc = int(sys.argv[3]) if len(sys.argv) > 3 else 1500
exe = os.environ.get("CONAN_EXE", "/mnt/d/ConanMod/bin/mcp-server.exe")
env = dict(os.environ, MOD_ROOT=os.environ.get("MOD_ROOT", r"D:\Program Files\CEUE5Devkit\UE4\Content\Mods\test"), WSLENV="MOD_ROOT")
p = subprocess.Popen([exe], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True, encoding="utf-8")
def send(m): p.stdin.write(json.dumps(m) + "\n"); p.stdin.flush()
def wait(i, limit):
    t0 = time.monotonic()
    while time.monotonic() - t0 < limit:
        line = p.stdout.readline()
        if not line: return None
        m = json.loads(line)
        if m.get("id") == i: return m
    return None
send({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"validation","version":"0"}}})
wait(1, 20); send({"jsonrpc":"2.0","method":"notifications/initialized"})
send({"jsonrpc":"2.0","id":2,"method":"tools/list"})
tl = wait(2, 20)
names = sorted(t["name"] for t in tl["result"]["tools"])
print(f"### tools/list: {len(names)} outils: {' '.join(names)}")
for k, (n, a) in enumerate(calls):
    i = 10 + k
    t0 = time.monotonic(); send({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":n,"arguments":a}})
    m = wait(i, 100); dt = time.monotonic() - t0
    r = m["result"] if m else {}
    txt = r["content"][0]["text"] if r else "(pas de réponse)"
    open(f"{S}/{tag}_{k}_{n}.json", "w", encoding="utf-8").write(txt)
    print(f"### [{k}] {n} {json.dumps(a, ensure_ascii=False)[:300]} isError={r.get('isError')} durée={dt:.2f}s taille={len(txt)}")
    print(txt[:maxc] + (" […]" if len(txt) > maxc else ""))
    if not m or r.get("isError"):
        print("### ARRÊT: KO, aucune autre commande envoyée"); break
p.stdin.close()
