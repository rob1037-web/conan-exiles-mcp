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
Serveur `rmcp` en stdio, configuré par trois variables d'environnement : `MOD_ROOT` (lecture seule, obligatoire), `CSV_ROOT` (écriture, défaut `D:\ConanMod\csv`) et `SCHEMA_ROOT` (schémas, lecture seule, optionnel). Les erreurs sont des erreurs d'outil `ERREUR <code>: <message>`. Mode d'emploi : `docs/MODE_EMPLOI.md`.

| Outil | Statut | Rôle |
|---|---|---|
| `list_mod_files` | Phase 2 (fait) | Liste les fichiers de `CSV_ROOT` par défaut ; `source:"mod"` pour le dossier du mod, en lecture seule. Jamais `.backup` ni `.<nom>.tmp-*` |
| `read_datatable` | Phase 2 (fait) | Lit une DataTable CSV (en-tête, lignes, format) pour enrichir ou éviter les doublons. `CSV_ROOT` par défaut ; `source:"mod"` en lecture seule |
| `list_schemas` | Phase 3a (fait) | Liste les schémas de `SCHEMA_ROOT` et leur validité (un schéma malformé est signalé avec fichier et champ) |
| `describe_schema` | Phase 3a (fait) | Renvoie l'en-tête CSV attendu, les colonnes, types, obligatoires, bornes et valeurs d'enum d'une table |
| `validate_rows` | Phase 3a (fait) | Valide des lignes fournies (`[clé, valeurs…]`) contre un schéma ; sans lignes, valide le CSV existant dans `CSV_ROOT` (diagnostic de corruption, sans réparation). Rapport structuré complet : ligne, clé, colonne, valeur, règle |
| `add_row` | Phase 3b (fait) | Ajoute une ligne **en fin de table** (clé inexistante, casse ignorée ; toutes les colonnes du schéma fournies). Dry-run par défaut, `dry_run=false` ET `confirm=true` pour écrire |
| `update_row` | Phase 3b (fait) | Fusionne des champs **partiels** dans une ligne existante (clé existante, casse ignorée ; colonne clé non modifiable). Sans changement de valeur : « AUCUN CHANGEMENT », ni écriture ni sauvegarde |
| `delete_row` | Phase 3b (fait) | Supprime une ligne existante ; le dry-run affiche la ligne qui serait supprimée. Outil séparé pour une approbation distincte |
| `write_datatable` | Phase 3a (fait) | Écrit un CSV sous `CSV_ROOT` à partir d'un **contenu CSV complet** pour une `table` donnée, **toujours validé par le schéma de la table**. `dry_run` vaut true par défaut (aperçu = diff par clé + rapport de validation) ; toute écriture réelle exige `dry_run=false` ET `confirm=true` explicites et un contenu valide ; sauvegarde dans `.backup` avant remplacement. Aucun mode « forcer » |

Décisions :
- **Pas de schéma valide = pas d'écriture.** Si `SCHEMA_ROOT` est absent ou invalide, le serveur démarre, la lecture fonctionne, toute écriture (dry-run compris) est refusée (`SchemaRootUnavailable`). Même refus si le schéma de la table est absent (`SchemaMissing`) ou malformé (`SchemaMalformed`).
- **Format de schéma : TOML conservé** (pas de JSON), un fichier `<table>.toml` par DataTable dans `SCHEMA_ROOT`, `name` identique au nom de la table. Champs : `name`, `description`, `key_column` (en-tête de la colonne clé dans le CSV exporté, `---` dans les exports Devkit ; **obligatoire**, y compris dans le parseur du noyau), puis `[[fields]]` **ordonnés** (`name`, `type` = string|int|float|bool|enum|asset_ref|raw, `required`, `values` pour enum, bornes). Voir `schemas/DT_Purge_Nordheimer.toml`.
- **Correspondance exacte des en-têtes** (noms, casse et ordre, colonne clé comprise) : tout écart est une *dérive de schéma* qui bloque l'écriture (`SchemaDrift`). Contrôles de valeurs : types, obligatoires, enums, unicité des clés (casse ASCII ignorée). Tous les écarts sont listés.
- `write_datatable` prend un contenu CSV complet. Les opérations par ligne passent par `add_row` / `update_row` / `delete_row` (Phase 3b) : elles lisent le CSV existant, **refusent tout (`SourceInvalid`) s'il est invalide, sans réparation**, revalident la table entière, conservent l'ordre des lignes et réutilisent le chemin d'écriture de `write_datatable` (`CsvStore::write_with_schema` : format Devkit, `.backup`, diff par valeurs, écriture atomique). Le dry-run et l'écriture réelle renvoient le même diff. Hors périmètre : lots, renommage de clé, réparation de CSV, génération de schémas.
- Le format JSON (CSV/JSON) n'est pas géré à ce stade.
- Les schémas ne sont jamais créés ni modifiés par l'outil (pas de génération automatique, pas de réparation de CSV).

### Migration des schémas (faite)
`schemas/lightning_rod_loot.toml` et `schemas/exemple.toml` ont reçu `key_column = "---"` : un seul format, une seule règle (`SchemaError::MissingKeyColumn` si absent). Tous les schémas versionnés se chargent dans un `SCHEMA_ROOT` (test).

### Phase 4b : valeurs par défaut des Blueprints (lecture seule)
`ue_get_defaults` et `ue_diff_defaults` ne modifient rien dans le Devkit (gabarits Python figés, test statique). `ue_diff_defaults` a deux modes exclusifs : deux Blueprints (`object_path_b`) ou des valeurs proposées (`proposed`, dry-run de la future écriture 4c).
- **Précision des flottants** : Python ne voit qu'un `float`; `real(float)` ou `real(double)` vient de la broche du Blueprint (champ `precision`). `real(float)` : la valeur proposée est arrondie en **f32** (`proposed_stored`) puis comparée exactement; si l'arrondi change les chiffres saisis, `not_representable: true` + `warning` (`16777217` → `16777216`; `0.3` sans alerte; informatif, `valid` n'en tient pas compte). `real(double)` : comparaison **exacte**. Précision inconnue (composants) : tolérance 1e-6.
- `raw` = `repr` Python d'un scalaire, **pas** un `export_text()` (en 4a, `export_text()` n'a été appelé que sur le type de la variable, jamais sur une valeur). Ne pas s'en servir pour écrire.
- Noms de variables : adressage par nom exact, casse et espaces compris, aucune normalisation.

### Format du fichier écrit
`write_datatable` remet toujours le contenu reçu au **format des exports du Devkit** avant comparaison et écriture (`Table::to_devkit_format`) : en-tête et clés sans guillemets, valeurs entre guillemets, flottants à six décimales (jamais d'arrondi), retour final. **Encodage, BOM et fins de ligne** : ceux du fichier existant s'il existe (le Devkit exporte en UTF-16 LE avec BOM dès qu'un texte n'est pas ASCII; détection par le BOM `FF FE` / `EF BB BF`, sinon UTF-8 strict; réécriture à l'octet près), sinon, pour un nouveau fichier, UTF-8 sans BOM et LF. `read_datatable` et les rapports de dry-run indiquent l'encodage; UTF-16 BE n'est pas pris en charge. Le diff compare les **valeurs après parsing CSV**, pas le texte brut : les guillemets ne sont jamais une modification (signalés à part, `quoting_changed`).

### Vérification dans le Devkit
Importer le CSV comme **nouvelle DataTable dans le dossier du mod**, row struct de la table, options d'import décochées, puis supprimer l'asset. Ne jamais réimporter une table de base du jeu (assets verrouillés). Voir `docs/MODE_EMPLOI.md`.

## Règles de sécurité (non négociables)
- **Sandbox** : le serveur n'écrit que sous `CSV_ROOT` (variable d'env ; défaut `D:\ConanMod\csv`, hors du dossier du mod). Tout chemin qui en sort (`..`, chemin absolu, lien symbolique, jonction) est refusé.
- **`SCHEMA_ROOT` est en lecture seule** (variable d'env, optionnelle) : même sandbox anti-évasion que les autres racines, aucune méthode d'écriture (`SchemaStore`), toute écriture qui se résout sous lui est refusée (`SchemaReadOnly`), et `CSV_ROOT` ne peut pas être situé sous lui.
- **`MOD_ROOT` est en lecture seule** (variable d'env, sans défaut) : toute écriture qui se résout sous lui est refusée (`StoreError::ModReadOnly`), et `CSV_ROOT` ne peut pas être situé sous lui. Ne jamais toucher à `modinfo.json` ni `active.txt`. L'import CSV → DataTable reste manuel dans le Devkit.
- **Jamais d'écrasement silencieux** : un fichier existant n'est remplacé qu'avec `confirm: true`, après sauvegarde dans `CSV_ROOT/.backup/<sous-dossier>/<nom>.<horodatage UTC>.<ext>` (si la sauvegarde échoue, rien n'est modifié). Écriture atomique (fichier temporaire du même dossier puis renommage). Les sauvegardes ne sont jamais purgées automatiquement (rétention à décider).
- **Dry-run par défaut** : `write_datatable` renvoie d'abord un aperçu (diff). Via MCP, toute écriture réelle, y compris la création d'un fichier, demande `dry_run=false` ET `confirm=true`, explicites (sans `confirm=true`, erreur `ConfirmRequired`). Dans le noyau, `confirm` conditionne le remplacement d'un fichier existant.
- **Validation obligatoire** : aucune écriture via MCP sans schéma valide ; le contenu est validé contre le schéma dans tous les cas (dry-run compris), l'écriture réelle est refusée si la validation échoue.
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
schemas/       # schémas TOML versionnés, un par DataTable (à copier dans SCHEMA_ROOT) + exemples
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
- [x] Phase 3a : `SCHEMA_ROOT` optionnel, outils `list_schemas`, `describe_schema`, `validate_rows`, validation par schéma obligatoire dans `write_datatable` (table de test `DT_Purge_Nordheimer`)
- [x] Phase 3b (close, recette validée dans Claude Desktop et le Devkit) : outils `add_row`, `update_row`, `delete_row`
- [x] Phase 4a (close, fusionnée dans `main`) : pont en lecture seule `ue_status`, `ue_search_assets`, `ue_describe_asset`, `ue_blueprint_variables`
- [ ] Phase 4b (branche `phase-4b-ue-defaults`) : `ue_get_defaults`, `ue_diff_defaults` (deux Blueprints ou valeurs proposées) — code et tests faits, recette Devkit validée (sections 2 à 4, lecture et round-trip du CSV UTF-16 réel) sauf le réimport 7.4 et `Timeout` (voir `docs/phase-4b-test.md`)
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
