# Brancher le serveur MCP dans Claude Desktop

Le serveur parle MCP en **stdio** (aucun port réseau). Claude Desktop le lance lui-même.

## 1. Construire le binaire Windows (depuis WSL, dans le dépôt)

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mcp-server
```

Résultat : `target/x86_64-pc-windows-gnu/release/mcp-server.exe` (autonome : aucune DLL
hors Windows). Sur un Windows avec la chaîne Rust MSVC, l'équivalent natif est
`cargo build --release -p mcp-server` → `target\release\mcp-server.exe`.

## 2. Copier le binaire hors du dépôt (à lancer par vous)

```bash
mkdir -p /mnt/d/ConanMod/bin
cp target/x86_64-pc-windows-gnu/release/mcp-server.exe /mnt/d/ConanMod/bin/
```

## 3. Bloc à ajouter dans `claude_desktop_config.json`

Fichier : `%APPDATA%\Claude\claude_desktop_config.json` (Claude Desktop, menu Paramètres →
Développeur → Modifier la configuration). Ajouter l'entrée sous `mcpServers` :

```json
{
  "mcpServers": {
    "conan-exiles": {
      "command": "D:\\ConanMod\\bin\\mcp-server.exe",
      "args": [],
      "env": {
        "MOD_ROOT": "D:\\Program Files\\CEUE5Devkit\\UE4\\Content\\Mods\\test",
        "CSV_ROOT": "D:\\ConanMod\\csv",
        "SCHEMA_ROOT": "D:\\ConanMod\\schemas"
      }
    }
  }
}
```

Puis **terminer TOUS les processus Claude** (Gestionnaire des tâches → onglet Processus, fin de tâche sur chaque processus « Claude »), puis relancer Claude Desktop. Fermer la fenêtre ne suffit pas : l'application reste en arrière-plan et ne relit pas la configuration.

- `MOD_ROOT` : dossier du mod, **lecture seule** (obligatoire).
- `CSV_ROOT` : dossier où les CSV sont écrits (défaut `D:\ConanMod\csv`), jamais sous `MOD_ROOT`.
- `SCHEMA_ROOT` : dossier des schémas `.toml`, en lecture seule (optionnel). Sans lui, ou s'il est
  invalide, le serveur démarre mais **toute écriture est refusée**. Voir [MODE_EMPLOI.md](MODE_EMPLOI.md).
- Le serveur refuse de démarrer, avec un message sur stderr, si `MOD_ROOT` est absent ou
  invalide, ou si `CSV_ROOT` est situé sous `MOD_ROOT`.

## Outils exposés

| Outil | Rôle |
|---|---|
| `list_mod_files` | Liste les fichiers de `CSV_ROOT` (défaut) ou du mod (`source: "mod"`, lecture seule). Jamais `.backup` ni `.<nom>.tmp-*`. |
| `read_datatable` | Lit un CSV (en-tête, lignes, format), pagination `offset` / `limit`. |
| `list_schemas` | Liste les schémas de `SCHEMA_ROOT` et leur validité. |
| `describe_schema` | En-tête attendu, types, obligatoires, valeurs d'enum d'une table. |
| `validate_rows` | Valide des lignes (ou le CSV existant) contre le schéma, sans rien écrire. |
| `write_datatable` | Écrit un CSV sous `CSV_ROOT`, **validé par schéma**. Aperçu (diff + rapport) par défaut ; écriture réelle seulement avec `dry_run=false` ET `confirm=true`. Sauvegarde dans `.backup` avant tout remplacement. |

Mode d'emploi complet : [MODE_EMPLOI.md](MODE_EMPLOI.md). L'import CSV → DataTable reste manuel
dans le Devkit.
