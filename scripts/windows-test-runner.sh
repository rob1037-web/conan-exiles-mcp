#!/usr/bin/env bash
# Runner cargo pour la cible x86_64-pc-windows-gnu, lancé depuis WSL.
# Copie l'exécutable sous %TEMP% (disque C:, NTFS) et l'y exécute : un répertoire
# courant sous \\wsl$ casserait les chemins Windows, et les tests créent des jonctions NTFS.
#
# Les tests qui ne peuvent pas s'exécuter faute de droits (ex. symlinks Windows sans mode
# développeur) affichent une ligne "SKIPPED <test>: <raison>" et retournent normalement,
# donc libtest les compte "passed". Le runner les en retire et affiche un compte séparé :
#     Windows: X OK, Y ignorés
set -euo pipefail
# Chemin absolu avant tout changement de dossier : `cargo run` le passe en relatif.
exe="$(realpath "$1")"
shift
cd /mnt/c
win_temp="$(cmd.exe /C 'echo %TEMP%' 2>/dev/null | tr -d '\r')"
dest="$(wslpath "$win_temp")/conan-core-tests"
mkdir -p "$dest"
cp -f "$exe" "$dest/"
# DLL du runtime mingw si l'exécutable en dépend (selon la variante threads de GCC).
for dll in libgcc_s_seh-1.dll libwinpthread-1.dll libstdc++-6.dll; do
    found="$(find /usr/lib/gcc/x86_64-w64-mingw32 /usr/x86_64-w64-mingw32/lib -name "$dll" 2>/dev/null | head -n1 || true)"
    [ -n "$found" ] && cp -f "$found" "$dest/"
done
cd "$dest"

# --nocapture est nécessaire pour voir les lignes SKIPPED ; il ne concerne que les binaires
# de test (dossier deps/), jamais les exemples ou binaires lancés par `cargo run`.
args=("$@")
case "$exe" in
    */deps/*)
        case " ${args[*]:-} " in *" --nocapture "*) ;; *) args+=(--nocapture) ;; esac
        ;;
esac

log="$(mktemp)"
trap 'rm -f "$log"' EXIT
set +e
"./$(basename "$exe")" "${args[@]}" 2>&1 | tee "$log"
code=${PIPESTATUS[0]}
set -e

passed="$(grep -E '^test result: ' "$log" | sed -E 's/.* ([0-9]+) passed.*/\1/' | paste -sd+ - | bc 2>/dev/null || true)"
passed="${passed:-0}"
skipped="$(grep -c "^SKIPPED " "$log" || true)"
if [ "$passed" -gt 0 ] || [ "$skipped" -gt 0 ]; then
    echo
    echo "Windows: $((passed - skipped)) OK, $skipped ignorés"
    grep '^SKIPPED ' "$log" | sed -E 's/^SKIPPED ([^:]+): /  ignoré: \1 : /' || true
fi
exit "$code"
