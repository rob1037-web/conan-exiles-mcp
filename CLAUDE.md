# Projet : serveur MCP Rust pour Conan Exiles Devkit

## Vision
Robin ne s'occupe plus que des **Blueprints**. Tout le reste (données, configuration, création de fichiers) passe par Claude via ce serveur MCP.
Usage type : dans Claude, « génère 20 armes nordheimer ». Le serveur valide les données et écrit le fichier prêt à importer dans le dossier du mod. Robin réimporte dans le Devkit et branche ses Blueprints.

## Contexte technique (à lire en premier)
- Cible : Devkit Conan Exiles (éditeur Unreal modifié, binaire). **Aucun code natif dans le Devkit** : pas de plugin C++, pas de FFI.
- Le serveur tourne **à côté** de l'éditeur. Il ne touche qu'à des fichiers texte que le Devkit importe (DataTables en CSV/JSON, fichiers de config).
- **Le cerveau est le client MCP (Claude).** Le serveur n'appelle aucun LLM. Son rôle : être déterministe, valider et écrire.
- Rust stable, edition 2021. SDK MCP officiel : crate `rmcp`, transport **stdio**.
- **Plateforme** : la cible finale est un **binaire Windows natif**, lancé par Claude Desktop sous Windows, avec `MOD_ROOT` en chemin Windows (lettre de lecteur, UNC). Le développement se fait dans **WSL** : le code de `core` doit rester portable et ne rien supposer de Linux (séparateurs, casse, liens symboliques, jonctions). Les tests spécifiques à Windows s'exécutent côté Windows.
- **Fidélité des formats** : chaque valeur garde sa représentation texte d'origine en plus de sa valeur typée. Aucune normalisation (flottants, booléens, guillemets) : le round-trip doit être exact à l'octet.
- Générique par schéma : le serveur ne connaît aucun type de données en dur. Il lit des schémas TOML. Ajouter un nouveau type = ajouter un fichier de schéma, zéro code.

## Outils MCP exposés
Serveur `rmcp` en stdio, configuré par `MOD_ROOT` (lecture seule, obligatoire) et `CSV_ROOT` (écriture, défaut `D:\ConanMod\csv`). Les erreurs sont des erreurs d'outil `ERREUR <code>: <message>`.

| Outil | Statut | Rôle |
|---|---|---|
| `list_mod_files` | Phase 2 (fait) | Liste les fichiers de `CSV_ROOT` par défaut ; `source:"mod"` pour le dossier du mod, en lecture seule. Jamais `.backup` ni `.<nom>.tmp-*` |
| `read_datatable` | Phase 2 (fait) | Lit une DataTable CSV (en-tête, lignes, format) pour enrichir ou éviter les doublons. `CSV_ROOT` par défaut ; `source:"mod"` en lecture seule |
| `write_datatable` | Phase 2 (fait) | Écrit un CSV sous `CSV_ROOT` à partir d'un **contenu CSV complet**. `dry_run` vaut true par défaut (aperçu = diff) ; toute écriture réelle, création comprise, exige `dry_run=false` ET `confirm=true` explicites ; sauvegarde dans `.backup` avant remplacement. Valide la syntaxe CSV, pas encore les colonnes contre un schéma |
| `list_schemas` | Phase 3 | Liste les schémas disponibles |
| `describe_schema` | Phase 3 | Renvoie les champs, types et contraintes d'un schéma, avec 2-3 lignes d'exemple |
| `validate_rows` | Phase 3 | Valide des lignes contre un schéma et renvoie les erreurs ligne par ligne, sans rien écrire |

Décisions :
- Les outils de schéma (`list_schemas`, `describe_schema`, `validate_rows`) arrivent en Phase 3, via une variable d'environnement **optionnelle** `SCHEMA_ROOT` (dossier des schémas TOML). Sans elle, le serveur fonctionne comme aujourd'hui, sans ces outils.
- `write_datatable` prend un contenu CSV complet. Les opérations par ligne (ajouter, modifier, supprimer des lignes) sont prévues en Phase 3. La validation par schéma de `write_datatable` ("refuse tout si une seule ligne est invalide") arrive avec `SCHEMA_ROOT`.
- Le format JSON (CSV/JSON) n'est pas géré à ce stade.

## Règles de sécurité (non négociables)
- **Sandbox** : le serveur n'écrit que sous `CSV_ROOT` (variable d'env ; défaut `D:\ConanMod\csv`, hors du dossier du mod). Tout chemin qui en sort (`..`, chemin absolu, lien symbolique, jonction) est refusé.
- **`MOD_ROOT` est en lecture seule** (variable d'env, sans défaut) : toute écriture qui se résout sous lui est refusée (`StoreError::ModReadOnly`), et `CSV_ROOT` ne peut pas être situé sous lui. Ne jamais toucher à `modinfo.json` ni `active.txt`. L'import CSV → DataTable reste manuel dans le Devkit.
- **Jamais d'écrasement silencieux** : un fichier existant n'est remplacé qu'avec `confirm: true`, après sauvegarde dans `CSV_ROOT/.backup/<sous-dossier>/<nom>.<horodatage UTC>.<ext>` (si la sauvegarde échoue, rien n'est modifié). Écriture atomique (fichier temporaire du même dossier puis renommage). Les sauvegardes ne sont jamais purgées automatiquement (rétention à décider).
- **Dry-run par défaut** : `write_datatable` renvoie d'abord un aperçu (diff). Via MCP, toute écriture réelle, y compris la création d'un fichier, demande `dry_run=false` ET `confirm=true`, explicites (sans `confirm=true`, erreur `ConfirmRequired`). Dans le noyau, `confirm` conditionne le remplacement d'un fichier existant.
- Les données reçues du client sont non fiables par principe. Tout est validé.

## Règles pour l'IA qui code
- Avance par étapes. Une étape terminée = code qui compile, `cargo clippy` propre, tests qui passent.
- Pas de dépendance ajoutée sans la justifier en une ligne dans le commit.
- Pas de `unwrap()` hors des tests. Erreurs : `thiserror` (lib), `anyhow` (binaire).
- Les erreurs renvoyées au client MCP sont **lisibles par une IA** : champ, ligne, valeur reçue, valeur attendue.
- Si une consigne est ambiguë, **arrête-toi et pose la question**. N'invente rien.

## Architecture
```
crates/
  core/        # (package `conan-core`, pour ne pas masquer le crate `core` de std) schémas, validation, lecture/écriture CSV-JSON, sandbox (aucune dépendance MCP)
  mcp-server/  # binaire : déclaration des outils rmcp, branchés sur core
schemas/       # un .toml par type de DataTable
fixtures/      # exports réels du Devkit pour les tests
```

## Checklist (chef de projet : Robin)

### Phase 0 — Décisions et vérifications (Robin, avant tout code)
- [x] Choisir **une** première DataTable de test (type d'item, recette…)
- [ ] L'exporter depuis le Devkit en CSV **et** JSON → `fixtures/` — CSV fait (`DT_LightningRodLoot_Common.csv`), JSON non fourni
- [ ] Identifier le dossier du mod qui servira de `MOD_ROOT`
- [ ] Optionnel : lancer le Devkit en ligne de commande avec `-help` / `-run=ImportAssets` pour savoir si les commandlets existent (piste pour automatiser le réimport plus tard)

### Phase 1 — `core` sans MCP
- [x] Workspace Cargo avec les 2 crates
- [x] Parser de schéma TOML (string, int, float, bool, enum, référence d'asset, champ requis/optionnel, bornes)
- [x] Validation d'une ligne, avec messages d'erreur précis
- [x] Lecture/écriture CSV au format exact des DataTables Unreal (la première colonne est la clé de ligne ; son en-tête est lu dans le fichier, `---` dans les exports observés, jamais supposé)
- [x] Test de round-trip : relire le fixture, le réécrire, obtenir un résultat identique à l'octet près
- [x] Sandbox de chemins + tests d'évasion (`..`, absolu, symlink)

### Phase 2 — Serveur MCP
- [x] Serveur `rmcp` en stdio avec les outils `list_mod_files`, `read_datatable`, `write_datatable` (les outils de schéma passent en Phase 3)
- [x] Dry-run + diff + `confirm`, sauvegarde automatique, écriture atomique
- [x] Branchement dans Claude Desktop (config `mcpServers`, voir `docs/claude-desktop.md`) et premier appel réel (validé : `list_mod_files` source=mod, `write_datatable` en dry-run)

### Phase 3 — Boucle réelle
- [ ] `SCHEMA_ROOT` optionnel et outils `list_schemas`, `describe_schema`, `validate_rows` ; validation par schéma dans `write_datatable`
- [ ] Opérations par ligne dans `write_datatable` (ajouter, modifier, supprimer des lignes)
- [ ] Générer 20 lignes via Claude, réimporter dans le Devkit, vérifier en jeu
- [ ] Chaque casse à l'import → nouveau test
- [ ] Exporter une DataTable contenant du texte (virgules, guillemets, accents) pour valider les cas limites du CSV (tests actuellement marqués « non validé sur un export Devkit réel »)
- [ ] Unicité des `Name`, vérification des références d'assets existantes

### Phase 4 — Plus tard
- [ ] Autres formats de config texte du mod
- [ ] Réimport automatique si les commandlets sont disponibles
- [ ] Outil `create_schema_from_fixture` : déduire un schéma depuis un export
- [ ] Contraintes entre colonnes (ex. `MinAmount` ≤ `MaxAmount`)
- [ ] Contrôle des références : un `ItemTemplateID` doit exister dans la table des items du jeu
