"""Lecture des défauts d'un Blueprint LOT PAR LOT (un `start` par commande) avec ue_spike: si le Devkit tombe,
le dernier lot envoyé est connu. Arrêt au premier échec, sans réessayer. LECTURE SEULE.

usage: chest_batches.py [object_path]   (défaut: le coffre BP_PL_Chest_Medium)
Construire l'exe: cargo build --release --target x86_64-pc-windows-gnu -p mcp-server --example ue_spike --features test-support
Variables: UE_SPIKE_EXE (défaut target/x86_64-pc-windows-gnu/release/examples/ue_spike.exe),
CONAN_VAL_OUT (sorties des lots, défaut <tmp>/conan_val).
"""
import json, subprocess, time, sys, os, tempfile
S = os.environ.get("CONAN_VAL_OUT") or os.path.join(tempfile.gettempdir(), "conan_val")
os.makedirs(S, exist_ok=True)
root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
exe = os.environ.get("UE_SPIKE_EXE", os.path.join(root, "target/x86_64-pc-windows-gnu/release/examples/ue_spike.exe"))
path = sys.argv[1] if len(sys.argv) > 1 else "/Game/Systems/Building/Placeables/BP_PL_Chest_Medium.BP_PL_Chest_Medium"
start = 0; k = 0; total_items = 0
while True:
    params = {"object_path": path, "start": start}
    print(f"--- lot {k}: envoi start={start}", flush=True)
    t0 = time.monotonic()
    p = subprocess.run([exe, "defaults", json.dumps(params)], capture_output=True, text=True, encoding="utf-8")
    dt = time.monotonic() - t0
    out = p.stdout
    if p.returncode != 0 or "ERREUR" in out.split("\n", 1)[0] or "\n" not in out:
        print(f"KO lot {k} start={start} durée={dt:.2f}s code={p.returncode}\n{out[:1500]}\n{p.stderr[:500]}")
        print("### ARRÊT: dernier lot envoyé = start=%d, aucun nouvel essai" % start); sys.exit(1)
    payload = json.loads(out.split("\n", 1)[1])
    open(f"{S}/chest_lot{k}.json", "w", encoding="utf-8").write(json.dumps(payload, ensure_ascii=False, separators=(",", ":")))
    items = payload["items"]; total_items += len(items)
    print(f"OK lot {k} start={start} durée={dt:.2f}s items={len(items)} lisibles={sum('value' in i for i in items)} illisibles={sum('unreadable' in i for i in items)} total={payload['total']} next_index={payload['next_index']} budget_exceeded={payload['budget_exceeded']} elapsed_s={payload['elapsed_s']} dirty={payload['dirty_packages']} warnings={payload['warnings']}", flush=True)
    if payload["next_index"] is None: break
    start = payload["next_index"]; k += 1
print("FIN: éléments lus =", total_items)
