# Conan Exiles Devkit — serveur MCP

[![Version](https://img.shields.io/badge/version-0.1.0-blue)](#versions)
[![Statut](https://img.shields.io/badge/statut-pr%C3%A9--version%20(en%20d%C3%A9veloppement)-orange)](#feuille-de-route)
[![Rust](https://img.shields.io/badge/rust-stable%20%7C%20edition%202021-informational)](https://www.rust-lang.org/)
[![Plateforme](https://img.shields.io/badge/cible-Windows%20(d%C3%A9veloppement%20sous%20WSL)-lightgrey)](#compilation)

Un serveur [MCP](https://modelcontextprotocol.io) écrit en Rust qui permet à Claude de préparer les
**données** d'un mod Conan Exiles (DataTables, configuration) pendant que le modeleur se concentre
sur les **Blueprints**.

> Exemple d'usage : dans Claude, « génère 20 armes nordheimer ». Le serveur valide les lignes contre
> un schéma, écrit le CSV prêt à importer dans un dossier séparé du mod, puis le modeleur le
> réimporte dans le Devkit et branche ses Blueprints.

> **Version 0.1.0 — pré-version.** Le projet est en cours de développement actif : les outils
> peuvent changer de forme d'une version à l'autre (voir la [feuille de route](#feuille-de-route)).
> Projet de fan, **non affilié à Funcom**. Conan Exiles est une marque de Funcom.

## Principes

- **Le cerveau est le client MCP (Claude).** Le serveur n'appelle aucun LLM : il est déterministe,
  il valide et il écrit.
- **Aucun code natif dans le Devkit.** Le serveur tourne à côté de l'éditeur et ne manipule que des
  fichiers texte que le Devkit importe.
- **Générique par schéma.** Aucun type de données n'est codé en dur : chaque DataTable est décrite
  par un fichier TOML. Ajouter un type = ajouter un schéma, zéro code.
- **Pas de schéma valide = pas d'écriture.** Même le dry-run est refusé sans schéma.
- **Fidélité des formats.** Chaque valeur garde sa représentation texte d'origine : le round-trip
  est exact à l'octet, sans normalisation des flottants, booléens ou guillemets.

## Garde-fous de sécurité

| Garde-fou | Effet |
|---|---|
| Sandbox | Le serveur n'écrit que sous `CSV_ROOT`. Tout chemin qui en sort (`..`, chemin absolu, lien symbolique, jonction Windows) est refusé. |
| `MOD_ROOT` en lecture seule | Le dossier du mod n'est jamais modifié. `modinfo.json` et `active.txt` ne sont jamais touchés. |
| `SCHEMA_ROOT` en lecture seule | Les schémas ne sont ni créés ni modifiés par l'outil. |
| Dry-run par défaut | Toute écriture renvoie d'abord un diff par clé. L'écriture réelle exige `dry_run=false` **et** `confirm=true`. |
| Jamais d'écrasement silencieux | Sauvegarde dans `CSV_ROOT/.backup/` avant remplacement, puis écriture atomique (fichier temporaire + renommage). |
| Validation obligatoire | Types, champs obligatoires, énumérations, bornes, unicité des clés, correspondance exacte des en-têtes (dérive de schéma = écriture bloquée). |
| Données non fiables | Tout ce que le client envoie est validé. |

## Outils exposés

Les erreurs sont renvoyées sous la forme `ERREUR <code>: <message>`, lisibles par une IA
(champ, ligne, valeur reçue, valeur attendue).

### Données (CSV)

| Outil | Rôle |
|---|---|
| `list_mod_files` | Liste les fichiers de `CSV_ROOT` (ou du mod avec `source:"mod"`, en lecture seule). |
| `read_datatable` | Lit une DataTable CSV (en-tête, lignes, format). |
| `list_schemas` | Liste les schémas disponibles et leur validité. |
| `describe_schema` | Renvoie l'en-tête attendu, les colonnes, types, bornes et valeurs d'énumération. |
| `validate_rows` | Valide des lignes (ou un CSV existant) contre un schéma, sans rien écrire. |
| `write_datatable` | Écrit un CSV complet, toujours validé par le schéma. |
| `add_row` | Ajoute une ligne en fin de table. |
| `update_row` | Fusionne des champs partiels dans une ligne existante. |
| `delete_row` | Supprime une ligne existante (outil séparé pour une approbation distincte). |

### Pont vers l'éditeur Unreal (lecture seule, expérimental)

| Outil | Rôle |
|---|---|
| `ue_status` | Indique si le Devkit (éditeur ouvert, *Python Remote Execution* activé) est joignable. |
| `ue_search_assets` | Recherche des assets dans l'éditeur. |
| `ue_describe_asset` | Décrit un asset. |
| `ue_blueprint_variables` | Liste les variables d'un Blueprint. |
| `ue_get_defaults` | Lit les valeurs par défaut d'un Blueprint. |
| `ue_diff_defaults` | Compare les valeurs par défaut de deux Blueprints, ou des valeurs **proposées** aux valeurs actuelles d'un Blueprint (dry-run, rien n'est écrit). |

**Comparaison des flottants.** Python ne voit qu'un `float` : la distinction `real(float)` /
`real(double)` vient du type de la broche du Blueprint (champ `precision`).

- `real(float)` : la valeur proposée est **arrondie en float 32 bits** (`proposed_stored`) puis comparée
  exactement. `0.3` est « inchangé » face à `0.30000001192092896`. Si l'arrondi change les chiffres
  saisis (`16777217` devient `16777216`), la réponse porte `not_representable: true`, `shortest_stored`
  et un `warning`. C'est une information : `valid` n'en tient pas compte. Hors plage 32 bits : `type_mismatch`.
- `real(double)` : comparaison exacte, sans tolérance.
- Précision inconnue (valeurs de composants, type inattendu) : tolérance absolue de 1e-6.
- `raw` est le `repr` Python de la valeur, pas un `export_text()` d'Unreal.

Ces outils ne **modifient, n'enregistrent et ne compilent rien**. Voir
[docs/ue-remote-execution.md](docs/ue-remote-execution.md) et [docs/phase-4b/](docs/phase-4b/).

## Installation et configuration

Le serveur parle MCP en **stdio** (aucun port réseau) et est lancé par le client MCP lui-même.
Il se configure par trois variables d'environnement :

| Variable | Rôle | Accès | Défaut |
|---|---|---|---|
| `MOD_ROOT` | Dossier du mod (`...\Content\Mods\<mod>`) | lecture seule | aucun (obligatoire) |
| `CSV_ROOT` | Dossier où les CSV sont écrits | lecture + écriture | `D:\ConanMod\csv` |
| `SCHEMA_ROOT` | Dossier des schémas `.toml` | lecture seule | aucun (optionnel) |

`CSV_ROOT` ne peut pas être situé sous `MOD_ROOT` ni sous `SCHEMA_ROOT`.

Exemple pour Claude Desktop (`claude_desktop_config.json`) :

```json
{
  "mcpServers": {
    "conan-exiles": {
      "command": "D:\\ConanMod\\bin\\mcp-server.exe",
      "env": {
        "MOD_ROOT": "D:\\Program Files\\CEUE5Devkit\\UE4\\Content\\Mods\\test",
        "CSV_ROOT": "D:\\ConanMod\\csv",
        "SCHEMA_ROOT": "D:\\ConanMod\\schemas"
      }
    }
  }
}
```

Détails : [docs/claude-desktop.md](docs/claude-desktop.md) et le mode d'emploi complet
[docs/MODE_EMPLOI.md](docs/MODE_EMPLOI.md).

## Écrire un schéma

Un fichier `<table>.toml` par DataTable, placé dans `SCHEMA_ROOT` :

```toml
name = "DT_Purge_Nordheimer"
description = "Table de purge Nordheimer (struct PurgeEnemyMapRow)"
key_column = "---"

[[fields]]
name = "SourceName"
type = "string"
required = true
```

Types disponibles : `string`, `int`, `float`, `bool`, `enum`, `asset_ref`, `raw`. Des schémas
d'exemple sont fournis dans [schemas/](schemas/).

## Compilation

Prérequis : Rust stable (edition 2021).

```bash
# Tests
cargo test

# Binaire Windows depuis WSL / Linux
cargo build --release --target x86_64-pc-windows-gnu -p mcp-server

# Binaire natif depuis Windows
cargo build --release -p mcp-server
```

Le dépôt est organisé en deux crates :

```
crates/
  core/        conan-core : schémas, validation, lecture/écriture CSV, sandbox (aucune dépendance MCP)
  mcp-server/  binaire : déclaration des outils rmcp, branchés sur core
schemas/       schémas TOML d'exemple, un par DataTable
fixtures/      exports du Devkit utilisés par les tests
docs/          mode d'emploi, intégration Claude Desktop, notes du pont Unreal
```

## Feuille de route

| Phase | Contenu | État |
|---|---|---|
| 1 | Noyau sans MCP : schémas, validation, CSV exact à l'octet, sandbox | Terminé |
| 2 | Serveur MCP stdio, dry-run + diff, sauvegarde, écriture atomique | Terminé |
| 3a | Schémas obligatoires, `list_schemas`, `describe_schema`, `validate_rows` | Terminé |
| 3b | Opérations par ligne : `add_row`, `update_row`, `delete_row` | Close (recette validée dans Claude Desktop et le Devkit) |
| 3 (suite) | Génération de 20 lignes en conditions réelles, réimport dans le Devkit, cas limites du CSV (virgules, guillemets, accents) sur un export réel | **En cours** |
| 4a | Pont de lecture vers l'éditeur Unreal (*Python Remote Execution*) : `ue_status`, `ue_search_assets`, `ue_describe_asset`, `ue_blueprint_variables` | Close (fusionnée dans `main`) |
| 4b | Lecture des valeurs par défaut des Blueprints (`ue_get_defaults`, `ue_diff_defaults`), prérequis de toute écriture | **En cours** : code et tests faits, recette dans le Devkit à faire ([procédure](docs/phase-4b-test.md)) |
| 4c | Écriture des valeurs par défaut dans le Devkit (dry-run issu de `ue_diff_defaults`) | Prévu |
| 4 (suite) | Autres formats de configuration texte du mod | Prévu |
| 4 (suite) | Outil `create_schema_from_fixture` (déduire un schéma d'un export) | Prévu |
| 4 (suite) | Contraintes entre colonnes (ex. `MinAmount` ≤ `MaxAmount`) | Prévu |
| 4 (suite) | Vérification des références d'assets existantes | Prévu |
| 4 (suite) | Réimport automatique dans le Devkit, si les commandlets le permettent | À l'étude |

Hors périmètre actuel : format JSON des DataTables, écriture par lots, renommage de clé, réparation
automatique de CSV.

## Versions

Le projet suit le [versionnement sémantique](https://semver.org/lang/fr/). Tant que la version
majeure est `0`, l'interface des outils peut encore changer.

- **0.1.0** — première version publique : outils CSV complets (lecture, validation, écriture, opérations par ligne) et outils de lecture du pont Unreal (15 outils MCP), encore en cours de stabilisation.

## Limites connues

- Le format JSON des DataTables n'est pas géré.
- Les cas limites du CSV (texte avec virgules, guillemets, accents) restent à valider sur un export
  réel du Devkit.
- Le pont Unreal est en lecture seule et expérimental ; il suppose un éditeur ouvert avec
  *Python Remote Execution* activé.
- L'import CSV → DataTable reste manuel dans le Devkit. Ne jamais réimporter une table de base du
  jeu (assets verrouillés).

## Licence

Aucune licence n'est encore définie : en l'absence de fichier `LICENSE`, tous droits réservés par
défaut. À choisir avant d'accepter des contributions.
