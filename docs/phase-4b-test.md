# Phase 4b — procédure de recette (lecture des valeurs par défaut)

À exécuter par Robin, **Devkit ouvert** (démarrage ~2 minutes). Tout est en **lecture seule**.
Les tests automatiques (faux éditeur) sont verts ; cette recette sert à confirmer le comportement
sur le vrai Devkit, ce qu'aucun test automatique ne peut faire. Les valeurs « attendues » viennent
de la sonde du 18 octobre ([phase-4b/sonde.md](phase-4b/sonde.md)) ; si l'une diffère, le noter
tel quel : c'est une information, pas forcément un bug.

## 0. Ce que font les deux outils

- `ue_get_defaults(object_path, section?, component?, names?, expand?, start?)` : lit les valeurs par
  défaut d'un Blueprint (objet par défaut de sa classe générée). Pour chaque variable : `name` exact,
  `owner`, `type` (type réel de la broche, p. ex. `real(float)` ou `real(double)`), `precision`
  (`float`/`double`, seulement pour les réels), `value` (valeur interprétée), `raw` (valeur brute d'un
  scalaire : `repr` Python, exact). Sans `names` : toutes les variables membres, héritées comprises.
- `ue_diff_defaults` a deux modes, exclusifs :
  - `object_path_b` : compare **deux Blueprints** ;
  - `proposed` = `{nom exact: valeur}` (avec `object_path_a`, ou son alias `asset_path`) : compare des
    valeurs **proposées** aux valeurs actuelles. **Rien n'est écrit.** C'est le dry-run de la future
    écriture (phase 4c).
    Statut par variable : `unchanged`, `modified`, `type_mismatch`, `unknown_variable`,
    `not_comparable`. `valid` vaut `true` seulement si tout est comparable et de bon type.

Ce qu'il faut savoir :

- **float ou double** : Python ne voit qu'un `float`. Seul le champ `type`/`precision` dit si la
  variable est `real(float)` ou `real(double)`. Un `0.3` stocké dans un `real(float)` se lit
  `0.30000001192092896` : `raw` le montre.
  - Pour une variable `real(float)`, `ue_diff_defaults` **arrondit la valeur proposée en float 32 bits**
    (`proposed_stored`) puis la compare **exactement** à la valeur actuelle : `0.3` est `unchanged`,
    `0.3000001` est `modified`, une valeur hors plage 32 bits est `type_mismatch`. Aucune tolérance.
    Si l'arrondi change les chiffres saisis, la ligne porte `not_representable: true`, `shortest_stored`
    et un `warning` : `16777217` devient `16777216` (alerte) ; `0.3` ou `0.3000001` (pas d'alerte) ; la
    valeur exacte copiée depuis `ue_get_defaults` (`0.30000001192092896`) non plus. L'alerte est une
    information : `valid` n'en tient pas compte.
  - Pour un `real(double)`, la comparaison est **exacte** (500 et 500.0 sont la même valeur ;
    500.0000004 est `modified`).
  - Sans précision connue (valeurs de composants, type inattendu), la tolérance absolue de 1e-6
    s'applique encore. Le diff entre deux Blueprints suit la même règle : exact si la précision est connue.
- **`raw` n'est pas un `export_text()`** : c'est le `repr` Python de la valeur (scalaires seulement).
  En 4a, `export_text()` n'a été appelé que sur le **type** de la variable (`get_member_variable_type(...)`,
  voir [phase-4a-test.md](phase-4a-test.md) §6), pas sur sa valeur : aucune preuve qu'il existe pour une
  valeur. `raw` est donc la valeur Python exacte, pas le texte d'export d'Unreal ; les deux peuvent
  différer (p. ex. `True` en Python, `true` en Unreal ; `'x'` avec guillemets Python). Ne pas s'en servir
  pour écrire dans le Devkit.
- **Noms exacts** : `Item Data Table` (avec espaces) s'écrit tel quel. `item data table` est une
  `unknown_variable`. Aucune normalisation.
- Tableaux, sets, maps et structs ne sont **pas dépliés** par défaut ; dans le mode `proposed`, une
  variable de ce genre est `not_comparable`.
- Lecture bornée : 60 variables et 2 s par commande, 12 commandes au plus par appel. Au-delà,
  `complete: false` et `next_index` (rappeler avec `start`).

## 1. Déploiement

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mcp-server
cp target/x86_64-pc-windows-gnu/release/mcp-server.exe /mnt/d/ConanMod/bin/
```

Configuration de Claude Desktop inchangée ([claude-desktop.md](claude-desktop.md)). Terminer **tous**
les processus Claude (Gestionnaire des tâches), relancer, ouvrir le Devkit et attendre la fin du
chargement. Autoriser chaque outil « une fois » (lecture seule).

Avant de commencer : dans le Devkit, *File > Save All* doit ne rien proposer à enregistrer. Si des
assets sont déjà marqués modifiés, les noter : ils ne viennent pas de cette recette.

## 2. Déroulé dans Claude Desktop

### 2.1 Joignabilité

« Le Devkit est-il joignable ? » → `ue_status` : `reachable: true`.

### 2.2 `BP_interactable_relic_datatable` (11 variables propres)

1. « Donne les valeurs par défaut de `/Game/Systems/BP_interactable_relic_datatable`, seulement les
   variables propres » : `ue_get_defaults` avec `names` = les 11 variables vues avec
   `ue_blueprint_variables` (même chemin). Attendu : 11 éléments, `missing_names: []`,
   `complete: true`.
2. Contrôler **au moins** ces trois cas :

| Cas | Variable | Attendu |
|---|---|---|
| float | `ChanceToSpawnShown` | `type: real(double)`, `precision: double`, `value` ≈ 0.3, `raw` = le texte exact lu |
| entier | `MaxAmount` | `type: int`, `value: 1` |
| nom avec espace | `Item Data Table` | `type: object<DataTable>`, `value: /Game/Items/ItemTable.ItemTable` |
| tableau | `PossibleItemIDs` | non déplié : `{"not_expanded":true,"kind":"Array","length":1}` ; avec `expand: ["PossibleItemIDs"]` : `[11103]` |

   Si une variable a un autre type que celui annoncé, c'est le Devkit qui fait foi : noter la valeur.
3. **float contre double** : repérer une variable `real(float)` (le type le dit) et vérifier que
   `precision` vaut `float` et que `raw` montre l'écart de précision (p. ex. `0.30000001192092896`).
   Si ce Blueprint n'en a aucune, en chercher une dans les 244 variables héritées de la boîte aux lettres
   (étape 2.3).

### 2.3 `BP_PL_Mailbox`

`/Game/Mods/test/test/Mailbox/BP_PL_Mailbox` (le chemin avec `test/test` est celui de la sonde).

1. `ue_get_defaults` **sans** `names` : 244 variables, `total: 244`. Plusieurs commandes enchaînées
   (60 par lot) : `commands` ≥ 5 ; `complete: true`.
2. Avec `names: ["MaxHealth", "SmartObjectInteractRange", "InventorySharingAccess"]` :

| Variable | Attendu |
|---|---|
| `MaxHealth` | `int`, 1000 |
| `SmartObjectInteractRange` | `real(double)`, `precision: double`, 500.0 |
| `InventorySharingAccess` | `byte<…>` (énumération), `InventoryShareAccess.ON` |

3. Un nom inexistant (`maxhealth`, minuscules) : listé dans `missing_names`.

### 2.4 Comparaison de valeurs proposées (dry-run)

Sur `BP_PL_Mailbox`, `ue_diff_defaults` avec `asset_path` et :

```json
{"MaxHealth": 1000, "SmartObjectInteractRange": 750, "CanBeDismantled": "oui", "maxhealth": 1}
```

Attendu, **rien n'est écrit** :

| Variable | Statut |
|---|---|
| `MaxHealth` | `unchanged` |
| `SmartObjectInteractRange` | `modified` (500.0 → 750 ; `real(double)`, pas de `proposed_stored`) |
| `CanBeDismantled` | `type_mismatch` (booléen attendu) |
| `maxhealth` | `unknown_variable` |

`valid: false`, `would_change: true`, `read_only: true`. Refaire avec seulement les deux premières :
`valid: true`.

### 2.4 bis `real(float)` : arrondi en float 32 bits

Sur une variable `real(float)` (voir l'étape 2.2, point 3), proposer la valeur déjà lue, écrite
« proprement » (p. ex. `0.3` pour `0.30000001192092896`), puis une valeur très proche (`0.3000001`),
puis `16777217`. Attendu : `unchanged` avec `proposed_stored` = la valeur lue ; `modified` ; puis, pour
`16777217`, `not_representable: true` et `shortest_stored: "16777216"`. Si aucune variable `real(float)`
n'existe dans ces Blueprints, noter que ce cas n'a pas pu être vérifié sur le Devkit. Sur un
`real(double)` : `500.0000004` face à `500.0` doit donner `modified`.

### 2.5 Comparaison de deux Blueprints

`object_path_a` = `BP_PL_Mailbox`, `object_path_b` = `/Game/Mods/test/test/Chest/BP_PL_Chest_Medium`
(chemin à confirmer avec `ue_search_assets`, `name_contains: Chest_Medium`), avec
`names: ["MaxHealth", "DestroyActorOnZeroHealth"]`. Attendu : `MaxHealth` différent (1000 / 5000),
l'autre identique.

## 3. Vérification : aucun asset n'est modifié

1. Dans chaque réponse, `dirty_packages` doit être vide (ou ne contenir que ce qui était déjà
   modifié avant la recette).
2. Dans le Devkit : *File > Save All* ne propose rien de nouveau ; aucun astérisque sur
   `BP_PL_Mailbox` ni `BP_interactable_relic_datatable`.
3. Sur disque : aucun `.uasset` du mod ni `modinfo.json` ne change de date.

## 4. Cas d'erreur à essayer

| Action | Résultat attendu |
|---|---|
| Fermer le Devkit puis `ue_get_defaults` | `ERREUR DevkitNotRunning` |
| `ue_diff_defaults` sans `object_path_b` ni `proposed` | `ERREUR InvalidParameter` (rien n'est envoyé) |
| `ue_diff_defaults` avec les deux | `ERREUR InvalidParameter` |
| `ue_diff_defaults` avec `proposed` et `section` | `ERREUR InvalidParameter` |
| `ue_get_defaults` sur `/Game/Nope/Nope` | `ERREUR PythonError` : asset introuvable |
| Une commande qui dépasse 15 s | `ERREUR Timeout`, puis `EditorBusy` jusqu'à un `ue_status` réussi |

## 5. À signaler si cela arrive

- Une réponse avec `warnings` (API de repli ou obsolète) : copier le texte.
- `budget_exceeded` ou `complete: false` sur une petite sélection de noms.
- Une variable listée avec `unreadable` : noter laquelle et la raison.
- Toute fenêtre ou lenteur dans l'éditeur pendant la lecture.

## 6. Ce que cette recette ne couvre pas

Pour les outils `ue_*` (sections 1 à 5) : écrire une valeur dans le Devkit, créer des dossiers ou des
assets : c'est la phase 4c. La section 7 (CSV) est indépendante et ne touche aux assets qu'à la main, dans `Mods/test`.

## 7. Phase 3 (suite) — CSV réel avec du texte (virgules, guillemets, accents)

Objectif : valider sur un **vrai export du Devkit** les cas limites du CSV, aujourd'hui marqués « non
validé sur un export Devkit réel » dans [csv.rs](../crates/core/src/csv.rs). Cette partie n'utilise pas
les outils `ue_*` ; le Devkit sert à exporter puis à réimporter.

### 7.1 Choisir et exporter la table

1. Prendre une DataTable dont des cellules contiennent **une virgule, un guillemet `"` et un accent**
   (par exemple `Épée, longue` ou `Il dit "non"`). Si aucune table du jeu n'en a, en créer une dans
   `Mods/test` (nouvelle DataTable, même row struct, quelques lignes saisies à la main avec ces
   caractères), de préférence 5 à 10 lignes. Une table de base du jeu ne sert qu'à **exporter**.
2. Content Browser : clic droit sur la table, *Asset Actions > Export…*, format **CSV**, vers
   `CSV_ROOT` (par défaut `D:\ConanMod\csv`) sous le nom `<table>.csv`. Ne pas rouvrir ni
   retoucher le fichier dans un éditeur de texte (cela change les fins de ligne ou l'encodage).
3. Calculer l'empreinte de l'original, à conserver :
   `certutil -hashfile D:\ConanMod\csv\<table>.csv SHA256` (ou `sha256sum` sous WSL).
4. Écrire à la main le schéma `<table>.toml` dans `SCHEMA_ROOT` (voir
   [MODE_EMPLOI.md](MODE_EMPLOI.md) §2) : colonnes de texte en `string`, `key_column` = l'en-tête de
   la première colonne du fichier exporté. L'outil ne génère aucun schéma.

### 7.2 Lecture

Dans Claude Desktop : « Lis la table `<table>` avec `read_datatable` ». Attendu : l'en-tête et **toutes**
les lignes, avec les virgules, guillemets et accents **intacts** dans les valeurs (pas de colonne
décalée, pas de `Ã©` ou de `?` à la place des accents). Noter le format détecté (fins de ligne,
guillemets, BOM). Puis `validate_rows` sans lignes sur la table : `Validation: OK` (sinon, noter
l'erreur : c'est peut-être un cas limite à corriger dans le parseur ou dans le schéma).

### 7.3 Réécriture à l'identique et comparaison octet à octet

1. `write_datatable` avec `table`, `content` = le contenu lu à l'étape 7.2 (sans y toucher) et le chemin
   d'origine, `dry_run` par défaut. Attendu : aucune ligne ajoutée, modifiée ni supprimée ;
   `format_changed: false` si l'export était déjà au format Devkit. Si `format_changed` est `true`,
   noter lequel (guillemets, décimales, fins de ligne) : c'est une normalisation à examiner, pas un
   échec automatique.
2. Pour forcer une **vraie** écriture sans toucher à l'original, écrire vers un autre fichier :
   `path: "roundtrip/<table>.csv"`, `dry_run=false`, `confirm=true`. (Écrire sur le chemin d'origine
   avec un contenu identique ne produit que « AUCUN CHANGEMENT » et ne teste donc pas l'écriture.)
3. Comparer à l'original :
   `fc /b D:\ConanMod\csv\<table>.csv D:\ConanMod\csv\roundtrip\<table>.csv`
   (ou `cmp` sous WSL, ou comparer les empreintes SHA256). Attendu : **« Aucune différence »**.
   Sinon, noter l'offset et les octets qui diffèrent (souvent fin de ligne finale, guillemets ou
   flottants à six décimales) : l'écart devient un nouveau test, avec l'export comme fixture.
4. Facultatif : `update_row` sur une ligne (changer un texte contenant une virgule), puis le
   remettre à sa valeur d'origine : le fichier doit redevenir identique à l'original (empreinte).
   Une sauvegarde apparaît dans `.backup\`.

### 7.4 Réimport dans le Devkit, puis suppression

1. Importer `roundtrip/<table>.csv` (le fichier réécrit, pas l'original) comme **nouvelle DataTable
   dans `Mods/test`**, nom différent (p. ex. `DT_Test_Textes_Import`), **row struct de la table**,
   options d'import décochées.
2. Ouvrir la table : les mêmes lignes, les textes avec virgules, guillemets et accents **identiques**
   à ceux de l'original (comparer visuellement quelques lignes).
3. **Supprimer l'asset importé** (et rien d'autre) et vérifier qu'aucune table de base du jeu n'a été
   réimportée ni modifiée. Ne jamais réimporter une table de base (assets verrouillés).
4. Supprimer aussi `CSV_ROOT\roundtrip\` si on ne veut pas le conserver.

### 7.5 Résultat à rapporter

Pour chaque étape : OK, ou l'écart constaté (octets, ligne, texte). Joindre l'export (ou un extrait
anonymisé) pour qu'il devienne une fixture et que les tests « non validés » de `csv.rs` soient
remplacés par des tests sur ce fichier. Cocher ensuite la case correspondante dans
[CLAUDE.md](../CLAUDE.md) (« Exporter une DataTable contenant du texte… »).
