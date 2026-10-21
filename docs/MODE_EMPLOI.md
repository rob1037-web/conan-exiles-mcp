# Mode d'emploi — serveur MCP Conan Exiles (données CSV validées par schéma)

Ce serveur permet à Claude de lire des DataTables CSV, de les valider contre un schéma et de les
écrire dans un dossier séparé, sans jamais toucher au dossier du mod. **Pas de schéma valide =
pas d'écriture.** L'import du CSV dans le Devkit reste manuel.

## 1. Les trois dossiers

| Variable | Rôle | Accès | Défaut |
|---|---|---|---|
| `MOD_ROOT` | Dossier du mod (`...\Content\Mods\<mod>`) | **lecture seule** | aucun (obligatoire) |
| `CSV_ROOT` | Dossier où les CSV sont écrits | lecture + écriture | `D:\ConanMod\csv` |
| `SCHEMA_ROOT` | Dossier des schémas `.toml` | **lecture seule** | aucun (optionnel) |

- Les trois sont des variables d'environnement, posées dans le bloc `env` de
  `claude_desktop_config.json` (voir [claude-desktop.md](claude-desktop.md)).
- `SCHEMA_ROOT` absent, vide ou invalide : le serveur démarre, la lecture fonctionne
  (`list_mod_files`, `read_datatable`), mais **toute écriture est refusée** avec le message
  `ERREUR SchemaRootUnavailable`. Un avertissement est écrit sur stderr au démarrage.
- `CSV_ROOT` ne peut pas être situé sous `MOD_ROOT` ni sous `SCHEMA_ROOT`. Le serveur refuse de
  démarrer dans ce cas.
- Les schémas sont des fichiers que **vous** maintenez : l'outil ne peut ni les créer ni les
  modifier. Un chemin qui sortirait d'un dossier (`..`, chemin absolu, lien symbolique, jonction)
  est refusé.

Exemple de configuration (à adapter) :

```json
"env": {
  "MOD_ROOT": "D:\\Program Files\\CEUE5Devkit\\UE4\\Content\\Mods\\test",
  "CSV_ROOT": "D:\\ConanMod\\csv",
  "SCHEMA_ROOT": "D:\\ConanMod\\schemas"
}
```

Après toute modification de la configuration : terminer **tous** les processus Claude dans le
Gestionnaire des tâches, puis relancer Claude Desktop (fermer la fenêtre ne suffit pas).

## 2. Écrire un schéma

Un fichier `.toml` par DataTable dans `SCHEMA_ROOT`, **nommé d'après la table** :
`DT_Purge_Nordheimer.toml`. Le champ `name` doit être identique au nom de la table.

À partir de l'export CSV réel du Devkit :

1. L'en-tête de la première colonne (la clé de ligne, « Row Name ») est copié dans `key_column`
   (`---` dans les exports observés). **`key_column` est obligatoire.**
2. Chaque autre colonne de l'en-tête devient un bloc `[[fields]]`, **dans le même ordre**.
3. Le type de chaque colonne se déduit des valeurs de l'export (jamais d'autre source).

| Clé du bloc | Sens |
|---|---|
| `name` | Nom exact de la colonne (casse comprise) |
| `type` | `string`, `int`, `float`, `bool`, `enum` (aussi `asset_ref` et `raw`) |
| `required` | `true`: une cellule vide est refusée (défaut `false`) |
| `values` | Pour `enum`: liste exhaustive des valeurs autorisées (casse comprise) |
| `min`, `max` | Bornes pour `int` et `float` |
| `min_len`, `max_len` | Bornes de longueur pour `string` |
| `description` | Commentaire libre |

Toute clé inconnue, tout type inconnu, toute contrainte mal placée est une **erreur bloquante**.

### Exemple : la table de test `DT_Purge_Nordheimer`

Export réel (`fixtures/DT_Purge_Nordheimer.csv`) :

```
---,SourceName,TargetName,Weight,RowType
Trash1,"Nordheimer","PurgeNew_Nordheimer_Grunt_1","10.000000","WeightedSpawnTable"
Trash2,"Nordheimer","PurgeNew_Nordheimer_Grunt_2","3.000000","WeightedSpawnTable"
```

Schéma correspondant (`schemas/DT_Purge_Nordheimer.toml`) :

```toml
name = "DT_Purge_Nordheimer"
description = "Table de purge Nordheimer (struct PurgeEnemyMapRow)"
key_column = "---"

[[fields]]
name = "SourceName"
type = "string"
required = true

[[fields]]
name = "TargetName"
type = "string"
required = true

[[fields]]
name = "Weight"
type = "float"
required = true

[[fields]]
name = "RowType"
type = "enum"
required = true
values = ["WeightedSpawnTable"]
```

Attention : l'export ne montre que la valeur d'enum `WeightedSpawnTable`. Si l'enum
`EPurgeMapRowType` du Devkit en accepte d'autres, ajoutez-les à `values` ; sinon leur écriture
sera refusée.

Copiez ensuite le fichier dans `SCHEMA_ROOT` (par exemple
`cp schemas/DT_Purge_Nordheimer.toml /mnt/d/ConanMod/schemas/` depuis WSL).

## 3. Déroulé type

Demandez à Claude, par exemple : « Ajoute une ligne Trash5 à DT_Purge_Nordheimer ». Il suit :

1. **`list_schemas`** : quelles tables ont un schéma valide ?
2. **`describe_schema`** : l'en-tête attendu, les types, les valeurs d'enum.
3. **`read_datatable`** : lit le CSV existant dans `CSV_ROOT` (évite les doublons de clé).
4. **`validate_rows`** (optionnel) : valide des lignes proposées, sans rien écrire. Sans `rows`,
   valide le CSV existant : c'est un diagnostic de corruption, sans réparation.
5. **`write_datatable`** en **dry-run** (comportement par défaut) : renvoie le diff par clé
   (lignes ajoutées, modifiées, supprimées) et le rapport de validation. Rien n'est écrit.
6. **`write_datatable`** avec `dry_run=false` **et** `confirm=true` : écrit. Si le fichier
   existait, il est d'abord sauvegardé dans `CSV_ROOT\.backup\` (horodaté), puis remplacé de façon
   atomique. Le chemin de la sauvegarde est renvoyé.
7. **Import manuel** dans le Devkit, comme d'habitude (voir la section suivante pour une
   vérification sans risque).

### Vérifier un CSV en l'important dans le Devkit

Pour contrôler qu'un CSV écrit est bien lu par le Devkit **sans toucher à un asset existant** :

1. Dans le Devkit, importez le CSV **comme nouvelle DataTable dans le dossier du mod** (le dossier
   de `MOD_ROOT`), avec le row struct de la table (ici `PurgeEnemyMapRow`) et **toutes les options
   d'import décochées**.
2. Vérifiez les lignes, les valeurs et la reconnaissance des enums.
3. Supprimez ensuite l'asset de test.

**On ne réimporte jamais une table de base du jeu** : ses assets sont verrouillés et ne doivent
pas être remplacés. L'import de vérification se fait toujours dans le dossier du mod, vers un
nouvel asset.

**Format du fichier écrit.** Le contenu reçu est toujours remis au format des exports du Devkit
avant comparaison et écriture : en-tête et clés de ligne sans guillemets, valeurs entre
guillemets, flottants à six décimales (`20` devient `"20.000000"`, sans jamais arrondir une valeur
qui aurait plus de six décimales), avec retour final. Claude n'a donc
pas à reproduire le format exact : le diff compare les **valeurs**, jamais les guillemets, et ne
montre que les vrais changements. Le rapport indique « Format : le contenu reçu a été remis au
format du Devkit » quand c'est arrivé.

**Encodage.** Le Devkit exporte en ASCII quand tout le texte est ASCII, sinon en **UTF-16 LE avec
BOM** (`FF FE`). Le serveur lit l'encodage d'après le BOM (`FF FE` UTF-16 LE, `EF BB BF` UTF-8 avec
BOM, sinon UTF-8 strict) et `read_datatable` le renvoie dans `format.encoding` (`utf-16le-bom`,
`utf-8-bom`, `utf-8`). À l'écriture, un fichier **existant** garde exactement son encodage, son BOM et
ses fins de ligne (CRLF compris) : Claude envoie du texte UTF-8, le fichier reste en UTF-16 si c'était
le cas. Un **nouveau** fichier est écrit en UTF-8 sans BOM, fins de ligne LF. Le rapport des dry-runs
indique « Encodage : … ». Un fichier tronqué ou invalide (p. ex. UTF-16 de longueur impaire) est refusé
avec une erreur qui nomme l'encodage détecté ; UTF-16 BE n'est pas pris en charge.

Garde-fous : l'écriture réelle est refusée si le schéma est absent, malformé, si l'en-tête ne
correspond pas, ou si une seule valeur est invalide. **Il n'existe aucun mode « forcer ».** Un
contenu identique au fichier existant ne provoque ni écriture ni sauvegarde.

## 4. Agir sur une seule ligne : `add_row`, `update_row`, `delete_row`

Pour ajouter, modifier ou supprimer **une ligne** d'une table qui existe déjà dans `CSV_ROOT`,
trois outils séparés (Claude Desktop demande donc une approbation distincte pour chacun, en
particulier pour la suppression). Ils prennent `table` et `key` (obligatoires), `dry_run`
(**vrai par défaut**), `confirm` et, si besoin, `path` (défaut `<table>.csv`).

| Outil | Ce qu'il fait | `values` |
|---|---|---|
| `add_row` | Ajoute la ligne **en fin de table**. La clé ne doit pas exister | **Toutes** les colonnes du schéma (nom -> valeur). Colonne inconnue ou manquante : refus |
| `update_row` | Modifie une ligne existante | **Seulement** les colonnes à changer, fusionnées avec la ligne existante. La colonne clé est refusée (`KeyNotEditable`) : renommer = `delete_row` puis `add_row` |
| `delete_row` | Supprime une ligne existante | (aucune). Le dry-run affiche la ligne qui serait supprimée |

Les valeurs peuvent être du texte, un nombre ou un booléen JSON.

Règles communes :

- **Clés insensibles à la casse** (les noms de ligne Unreal sont des `FName`) : `trash1` désigne
  `Trash1`. À l'ajout, la casse fournie est conservée ; à la modification, la casse déjà stockée.
- **Le CSV existant doit être valide.** Sinon refus de toute opération (`SourceInvalid`), sans aucune
  réparation : lancez `validate_rows` pour le diagnostic.
- **La table entière résultante est revalidée**, pas seulement la ligne touchée.
- L'ordre des lignes existantes est conservé. Le fichier est réécrit au format du Devkit, avec
  sauvegarde `.backup` horodatée avant toute écriture réelle, comme `write_datatable`.
- Le dry-run et l'écriture réelle renvoient **le même diff**.
- `update_row` qui ne change aucune valeur (par exemple `10` pour `10.000000`) répond
  « AUCUN CHANGEMENT » : ni écriture ni sauvegarde.
- Écriture réelle : `dry_run=false` **et** `confirm=true`, explicites.

Exemples : « Ajoute la ligne Trash5 (SourceName Nordheimer, TargetName PurgeNew_X, Weight 2,
RowType WeightedSpawnTable) à DT_Purge_Nordheimer » ; « Passe le poids de Trash1 à 25 » ;
« Supprime Trash3 ».

## 5. Interpréter les erreurs

Les erreurs d'outil commencent par `ERREUR <code>:`. Les rapports de validation listent **toutes**
les erreurs, chacune avec la ligne, la clé de ligne, la colonne, la valeur reçue, la règle violée
et la valeur attendue :

```
- ligne 3 (clé Trash2), colonne 'Weight' [WrongType]: reçu "trois", attendu un nombre
```

`ligne` est le numéro de ligne physique dans le CSV (1 = en-tête, les données commencent à 2).

### Règles de validation (`[Règle]`)

| Règle | Signification |
|---|---|
| `KeyHeaderMismatch` | L'en-tête de la colonne clé n'est pas celui de `key_column` |
| `UnknownColumn`, `MissingColumn`, `DuplicateColumn`, `ColumnOrder` | **Dérive de schéma** : l'en-tête ne correspond pas exactement au schéma (noms et ordre). Bloque l'écriture ; les lignes ne sont alors pas validées |
| `ColumnCount` | Une ligne n'a pas le bon nombre de valeurs |
| `EmptyKey`, `DuplicateKey` | Clé de ligne vide, ou déjà utilisée (casse ASCII ignorée, comme les noms Unreal) |
| `RequiredEmpty` | Cellule vide dans un champ obligatoire |
| `WrongType` | Valeur qui n'est pas du type attendu (entier, nombre fini, true/false) |
| `OutOfRange` | Hors des bornes (`min`/`max`, `min_len`/`max_len`) |
| `NotInEnum` | Valeur absente de la liste `values` (casse comprise) |
| `MalformedLocText` | Valeur `NSLOCTEXT(...)` mal formée : Unreal l'afficherait brute. Voir « Textes localisés » ci-dessous |

### Textes localisés (`NSLOCTEXT`)

Dans un export du Devkit, une cellule de texte est du code Unreal :
`NSLOCTEXT("espace", "clé", "texte")`, trois littéraux entre guillemets. À l'intérieur d'un littéral, le texte
est échappé comme en C++ : **`\"`** pour un guillemet, **`\'`** pour une apostrophe, **`\r\n`** (ces quatre
caractères, pas un saut de ligne) pour un retour à la ligne. Le doublement des guillemets (`""`) est celui du
**CSV** et disparaît au parsing : il ne fait pas partie de la valeur envoyée aux outils.

Exemple de valeur envoyée à `add_row` (un seul niveau d'échappement, celui d'Unreal) :
`NSLOCTEXT("", "A156DFE041A3D799208675802D640A17", "Il dit \"non\", c\'est fini.")`

Toute valeur qui commence par `NSLOCTEXT(` dans une colonne `string` est vérifiée (trois littéraux, échappements
admis `\\ \" \' \? \a \b \f \n \r \t \v`, parenthèse fermante, rien après, aucun saut de ligne brut). Une valeur mal
formée s'importe sans erreur mais s'affiche brute dans Unreal : elle est donc signalée par `validate_rows` et les
dry-runs (`MalformedLocText`, avec la position du problème) et **l'écriture réelle est refusée**. Les autres formes
(`LOCTABLE(...)`, texte brut, cellule vide) et les colonnes `raw` ne sont pas vérifiées. Seule la syntaxe est
contrôlée, pas le format de l'espace de noms ni de la clé.

### Codes d'erreur d'outil (`ERREUR <code>`)

| Code | Cause et remède |
|---|---|
| `SchemaRootUnavailable` | `SCHEMA_ROOT` absent ou invalide : le configurer, redémarrer Claude Desktop |
| `SchemaMissing` | Pas de `<table>.toml` dans `SCHEMA_ROOT` : écrire le schéma |
| `SchemaMalformed` | Schéma illisible, `name` différent du nom du fichier, ou `key_column` absent. Le message indique le fichier et le champ fautif |
| `SchemaDrift` | Écriture refusée : l'en-tête ne correspond pas au schéma |
| `ValidationFailed` | Écriture refusée : au moins une valeur invalide |
| `ConfirmRequired` | Écriture réelle sans `dry_run=false` **et** `confirm=true` |
| `SourceInvalid` | Opération par ligne refusée : le CSV existant est invalide. `validate_rows` pour le diagnostic, aucune réparation |
| `KeyExists`, `KeyNotFound` | `add_row` sur une clé déjà présente ; `update_row` / `delete_row` sur une clé absente (casse ignorée) |
| `BadColumns` | Colonne inconnue, en double ou (pour `add_row`) manquante |
| `KeyNotEditable` | `update_row` a reçu la colonne clé |
| `InvalidParameter` | Valeur JSON non admise (ni texte, ni nombre, ni booléen) |
| `ModReadOnly`, `SchemaReadOnly` | Tentative d'écriture sous `MOD_ROOT` ou `SCHEMA_ROOT` |
| `PathRefused`, `PathEscapes`, `LinkRefused`, `ReservedPath`, `InvalidTableName` | Chemin ou nom de table refusé (hors dossier, `..`, lien, `.backup`…) |
| `BackupFailed` | La sauvegarde a échoué : le fichier n'a pas été modifié |
| `InvalidCsv`, `ExistingUnreadable` | Contenu ou fichier existant non lisible comme CSV |
| `NotFound`, `TooLarge`, `IoError` | Fichier introuvable, trop gros (> 16 Mo), erreur disque |

## 6. Approbation dans Claude Desktop

À la première utilisation de chaque outil, Claude Desktop demande une autorisation :

- **Autoriser une fois** : l'outil s'exécute cette fois seulement. **Recommandé pour
  `write_datatable`** : chaque écriture réelle est ainsi validée par vous, en plus du
  `dry_run`/`confirm`.
- Les outils `add_row`, `update_row`, `delete_row` et `write_datatable` modifient des fichiers :
  gardez **Autoriser une fois**, en particulier pour `delete_row`.
- **Toujours autoriser** : acceptable pour les outils de lecture (`list_mod_files`,
  `read_datatable`, `list_schemas`, `describe_schema`, `validate_rows`), qui ne modifient rien.
- Un refus de votre part est respecté : Claude ne peut pas contourner l'autorisation.

## 7. Recette sur la table de test

Avec `DT_Purge_Nordheimer.csv` présent dans `CSV_ROOT` et le schéma dans `SCHEMA_ROOT` :

1. **Dry-run valide** : « Montre-moi (sans écrire) le diff si on ajoute `Trash5`,
   `Nordheimer`, `PurgeNew_Nordheimer_Grunt_3`, poids `2.000000`, `WeightedSpawnTable` ». Résultat
   attendu : `DRY-RUN`, 1 ligne ajoutée, `Validation: OK`.
2. **Dry-run invalide** : « Même chose avec un poids `deux` et un RowType `Inconnu` ». Attendu :
   `DRY-RUN — VALIDATION ÉCHOUÉE` avec les erreurs `[WrongType]` et `[NotInEnum]`.
3. **Écriture réelle** : « Écris la version valide ». Autoriser une fois. Attendu : `ÉCRIT`, une
   sauvegarde dans `.backup\`, et le CSV mis à jour dans `CSV_ROOT`.
