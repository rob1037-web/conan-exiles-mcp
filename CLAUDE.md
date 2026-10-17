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

## Outils MCP exposés (MVP)
| Outil | Rôle |
|---|---|
| `list_schemas` | Liste les schémas disponibles |
| `describe_schema` | Renvoie les champs, types et contraintes d'un schéma, avec 2-3 lignes d'exemple |
| `read_datatable` | Lit une DataTable existante du mod (pour enrichir ou éviter les doublons) |
| `validate_rows` | Valide des lignes contre un schéma et renvoie les erreurs ligne par ligne, sans rien écrire |
| `write_datatable` | Valide puis écrit le CSV/JSON. Refuse tout si une seule ligne est invalide |
| `list_mod_files` | Liste les fichiers de données du mod |

## Règles de sécurité (non négociables)
- **Sandbox** : le serveur n'écrit que sous `MOD_ROOT` (variable d'env ou fichier de config). Tout chemin qui en sort (`..`, chemin absolu, lien symbolique) est refusé.
- **Jamais d'écrasement silencieux** : un fichier existant est sauvegardé dans `.backup/<horodatage>/` avant d'être réécrit.
- **Dry-run par défaut** : `write_datatable` renvoie d'abord un aperçu (diff). L'écriture réelle demande `confirm: true`.
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
- [ ] Choisir **une** première DataTable de test (type d'item, recette…)
- [ ] L'exporter depuis le Devkit en CSV **et** JSON → `fixtures/`
- [ ] Identifier le dossier du mod qui servira de `MOD_ROOT`
- [ ] Optionnel : lancer le Devkit en ligne de commande avec `-help` / `-run=ImportAssets` pour savoir si les commandlets existent (piste pour automatiser le réimport plus tard)

### Phase 1 — `core` sans MCP
- [ ] Workspace Cargo avec les 2 crates
- [ ] Parser de schéma TOML (string, int, float, bool, enum, référence d'asset, champ requis/optionnel, bornes)
- [ ] Validation d'une ligne, avec messages d'erreur précis
- [ ] Lecture/écriture CSV au format exact des DataTables Unreal (colonne `Name` en tête)
- [ ] Test de round-trip : relire le fixture, le réécrire, obtenir un résultat identique à l'octet près
- [ ] Sandbox de chemins + tests d'évasion (`..`, absolu, symlink)

### Phase 2 — Serveur MCP
- [ ] Serveur `rmcp` en stdio avec les 6 outils
- [ ] Dry-run + diff + `confirm`, sauvegarde automatique
- [ ] Branchement dans Claude Desktop (config `mcpServers`) et premier appel réel

### Phase 3 — Boucle réelle
- [ ] Générer 20 lignes via Claude, réimporter dans le Devkit, vérifier en jeu
- [ ] Chaque casse à l'import → nouveau test
- [ ] Unicité des `Name`, vérification des références d'assets existantes

### Phase 4 — Plus tard
- [ ] Autres formats de config texte du mod
- [ ] Réimport automatique si les commandlets sont disponibles
- [ ] Outil `create_schema_from_fixture` : déduire un schéma depuis un export
