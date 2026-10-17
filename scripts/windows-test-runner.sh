#!/usr/bin/env bash
# Runner cargo pour la cible x86_64-pc-windows-gnu, lancé depuis WSL.
# Copie l'exécutable sous %TEMP% (disque C:, NTFS) et l'y exécute : un répertoire
# courant sous \\wsl$ casserait les chemins Windows, et les tests créent des jonctions NTFS.
set -euo pipefail
exe="$1"
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
exec "./$(basename "$exe")" "$@"
