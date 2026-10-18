# Phase 4a — procédure d'intégration (pont MCP ↔ Devkit, lecture seule)

À exécuter par Robin. Les résultats « attendus » ci-dessous sont ceux observés pendant le
développement, le 18 octobre, sur ce Devkit (UE 5.8.2, projet `ConanSandbox`) ouvert, avec le
binaire de cette branche lancé en stdio.

## 0. Principe et garanties

- Le serveur discute avec l'éditeur par **Python Remote Execution** (protocole décrit dans
  [ue-remote-execution.md](ue-remote-execution.md)) : découverte UDP multicast `239.0.0.1:6766`
  (interface `127.0.0.1`, TTL 0), puis un serveur TCP éphémère sur `127.0.0.1`, une commande,
  `close_connection`. **Une session par appel d'outil**, rien de persistant.
- Le code Python envoyé est constitué de **4 gabarits figés** (constantes Rust,
  `crates/mcp-server/src/ue_bridge/templates.rs`). Les paramètres y entrent par `json.loads` d'un
  littéral, jamais par interpolation. Aucun gabarit n'écrit, ne sauvegarde, ne supprime, ne renomme,
  ne duplique, n'importe d'asset ni ne modifie de propriété (test automatique de cette liste).
- Seul `ue_blueprint_variables` charge un asset (`load_asset`, une fois, l'asset demandé) ; il
  indique ensuite si le package est marqué modifié dans l'éditeur.
- Le Devkit doit être **ouvert** (démarrage ~2 minutes). Sinon : `ue_status` répond « INJOIGNABLE »
  et les autres outils `ERREUR DevkitNotRunning`.

## 1. Déploiement

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mcp-server
cp target/x86_64-pc-windows-gnu/release/mcp-server.exe /mnt/d/ConanMod/bin/
```

La configuration de Claude Desktop est celle de [claude-desktop.md](claude-desktop.md) (rien à
ajouter). Variable facultative : `UE_PROJECT_NAME` (défaut `ConanSandbox` ; vide = n'importe quel
éditeur). Puis terminer **tous** les processus Claude (Gestionnaire des tâches) et relancer.
Ouvrir le Devkit et attendre la fin du chargement.

## 2. Déroulé dans Claude Desktop

> Depuis la Phase 4b (étape 0), les quatre outils `ue_*` renvoient un **objet JSON compact** (le même
> texte que le contenu structuré). Les blocs « Attendu » ci-dessous décrivent les valeurs de ces
> champs (`engine_version`, `project_name`, `asset_count`, `assets[].object_path`, `tags`...).
> `ue_status` avec le Devkit fermé répond `{"reachable":false,"error_code":"DevkitNotRunning","error":...}`.
> `ue_search_assets` trie ses résultats par `object_path`.

Les autorisations : « Autoriser une fois » suffit (outils en lecture seule).

### 2.1 `ue_status`

Demande : « Le Devkit est-il joignable ? »

Attendu :
```
Devkit joignable.
Moteur: 5.8.2-378787+++exiles+release
Projet: ConanSandbox (D:/Program Files/CEUE5Devkit/UE4/)
Assets dans le registre: 85240        (nombre approximatif, varie avec le contenu)
Registre en cours de chargement: false
```
`structuredContent.node` contient le `pong` brut de l'éditeur (`project_name`, `engine_root`,
`project_root`, `machine`, `user`...). Durée ~1,5 s.

### 2.2 `ue_search_assets` (path_prefix `Mods/test`)

Attendu : **3 assets**
```
Blueprint   /Game/Mods/test/BP_Test.BP_Test
Blueprint   /Game/Mods/test/test/Mailbox/BP_PL_Mailbox.BP_PL_Mailbox
StaticMesh  /Game/Mods/test/test/Mailbox/SM_BoiteAuxLettres.SM_BoiteAuxLettres
```
Les chemins de package sont sous `/Game/Mods/test/...` ; le préfixe est accepté avec ou sans `/Game`.

### 2.3 `ue_describe_asset` sur `SM_BoiteAuxLettres`

Entrée : `/Game/Mods/test/test/Mailbox/SM_BoiteAuxLettres` (le suffixe `.SM_BoiteAuxLettres` est
ajouté automatiquement). Attendu :
- Classe `StaticMesh` (`/Script/Engine`).
- Tags : `ApproxSize = 45x35x153`, `Triangles = 224`, `Vertices = 492`, `LODs = 1`,
  `Materials = 2`, `NaniteEnabled = True`, `UVChannels = 1`, `AssetImportData` (chemin du FBX source).
- 3 dépendances directes : `/Script/NavigationSystem`, `/Script/InterchangeEngine`,
  `/Game/Items/Placeables/Materials/FLX/MI_A2C2_rustic_wooden_frame`.
- Aucun avertissement.

### 2.4 `ue_blueprint_variables` sur `BP_PL_Mailbox`

Entrée : `/Game/Mods/test/test/Mailbox/BP_PL_Mailbox`. Attendu :
```
0 variable(s) propre(s) au Blueprint, 244 héritée(s) (non listées).
Package marqué modifié dans l'éditeur après lecture: non
```
**Ce 0 est exact** : `BP_PL_Mailbox` ne déclare aucune variable, il hérite de
`BP_PlaceableItemContainer` puis de `BP_Master_Placeables`. Avec `include_inherited=true`, les 244
héritées sont listées (nom qualifié par la classe d'origine, sans valeur par défaut).

Pour voir de vraies variables, essayer un Blueprint du jeu de base qui en déclare :
`/Game/Systems/BP_interactable_relic_datatable` → **11 variables propres**, par exemple
`MaxAmount : int = 1 [catégorie: Items]`, `ChanceToSpawnShown : real(double) = 0.3…`,
`PossibleItemIDs : Array of int = [11103]`, `Item Data Table : object<DataTable> = /Game/Items/ItemTable.ItemTable`.

### 2.5 Recherche sur le contenu de base

`ue_search_assets` avec `class_name = DataTable`, `limit = 20`. Attendu : **899 DataTables au total,
20 affichées**, marquées « Résultats tronqués » ; la première est
`/FLXGameUserSettings/StringTables/DT_LocalizedEnums.DT_LocalizedEnums`. Durée ~1,5 s.

## 3. Vérification : aucun asset n'est modifié

1. Dans l'éditeur, la barre de titre et le Content Browser ne montrent aucun astérisque / asset
   « non sauvegardé » pour `BP_PL_Mailbox` et `SM_BoiteAuxLettres` (menu *File > Save All* : la liste
   des assets modifiés ne doit pas les contenir).
2. Le résultat de `ue_blueprint_variables` doit afficher `Package marqué modifié ... non`
   (`package_marked_modified: false`).
3. Sur disque, ni `modinfo.json` ni les `.uasset` du mod ne changent de date.

## 4. Cas d'erreur à essayer

| Action | Résultat attendu |
|---|---|
| Fermer le Devkit puis `ue_status` | « Devkit INJOIGNABLE », `reachable: false`, pas d'erreur d'outil |
| Fermer le Devkit puis `ue_search_assets` | `ERREUR DevkitNotRunning` |
| `ue_describe_asset` sur `/Game/Nope/Nope` | `ERREUR PythonError` : « Asset introuvable dans l'Asset Registry » |
| `ue_search_assets` avec `limit=0` | `ERREUR InvalidParameter` (rien n'est envoyé à l'éditeur) |
| `ue_search_assets` avec `limit=9999` | 500 résultats au plus, `limit_clamped: true` |
| `ue_describe_asset` avec `Game/X` (sans `/`) | `ERREUR InvalidParameter` |

Codes d'erreur du pont : `DevkitNotRunning`, `ProtocolError`, `Timeout` (connexion 5 s, commande
15 s depuis la phase 4b), `PythonError` (traceback renvoyé, 4000 caractères au plus), `SocketError`.

## 5. Dépannage

- « aucun éditeur n'a répondu » : Devkit fermé, ou encore en démarrage, ou *Remote Execution*
  désactivé (`DefaultEngine.ini` : `bRemoteExecution=True`).
- « N éditeur(s) ont répondu, aucun pour le projet ... » : régler `UE_PROJECT_NAME` (le message
  liste les projets vus).
- `Timeout` à la connexion : pare-feu local ou éditeur occupé (modale, import en cours).
- Les autres outils (CSV, schémas) ne dépendent pas du Devkit et fonctionnent Devkit fermé.

## 6. Limites constatées (notamment `ue_blueprint_variables`)

Spike réalisé sur le Devkit ouvert (BP_PL_Mailbox, BP_Test, BP_interactable_relic_datatable).

**Obtenu proprement** : noms des variables déclarées (noms réels du Blueprint, y compris avec
espaces), type (via `get_member_variable_type().export_text()` : catégorie, sous-catégorie, classe
d'objet, conteneur), catégorie, réplication, valeur par défaut lue sur le CDO
(`get_editor_property(nom)`).

**Non obtenu ou approximatif** :
- `Blueprint.new_variables` et `parent_class` ne sont pas accessibles en Python (propriétés non
  exposées) ; `dir()` du CDO n'expose pas les variables du Blueprint. Les méthodes
  `list_member_variable_names` / `get_member_variable_type` / `get_blueprint_variable_category`
  de l'objet `Blueprint` sont la seule voie fiable trouvée.
- `list_member_variable_names` mélange variables propres et héritées ; les héritées ont un nom
  qualifié (`/Game/X.X_C.Nom`, `/Script/Engine.Actor.Nom`) : c'est ainsi qu'on les distingue.
- Le type est un libellé dérivé (`int`, `real(double)`, `object<DataTable>`, `Array of int`) ; les
  enums, structs et classes sont repérés par `sub_category_object`, pas par un type Python.
- Les valeurs par défaut : un `real` simple précision apparaît avec ses chiffres de double
  (`0.30000001192092896`) ; les objets sont rendus par leur chemin ; `Name` vide s'affiche `None`.
  Défaut non lisible : `default_available: false`.
- Les drapeaux « instance editable », « expose on spawn », « private », les tooltips ne sont pas lus.
- Le Blueprint est **chargé** en mémoire (sans être modifié) ; ses dépendances peuvent l'être aussi.

**Autres limites**
- `ue_describe_asset` : l'API Python n'énumère pas les tags du registre ; une liste fixe de tags
  courants est interrogée (`RowStructure`, `ParentClass`, `Triangles`...). Un tag exotique peut manquer.
- `ue_status` compte les assets avec `get_all_assets()` (~1 s pour 85 000) ; chaque outil ouvre sa
  propre session (~1,5 s).
- Un seul appel `ue_*` à la fois (verrou), pas de connexion persistante.
- Les tests des gabarits qui exécutent Python (faux module `unreal`) sont ignorés sous Windows
  (pas de `python3` dans le PATH) et tournent sous WSL.

## 7. Points à trancher pour 4b

1. Écriture dans l'éditeur (création/modification de DataTable) : par import du CSV via Python,
   ou toujours manuel ? Si automatique : droits, sauvegarde préalable de l'asset, confirmation.
2. Session persistante (pour enchaîner plusieurs commandes) ou rester à une session par appel.
3. Lecture du contenu d'une DataTable de base directement depuis l'éditeur (`DataTable` Python :
   noms de lignes et valeurs) pour alimenter `read_datatable` sans export CSV manuel.
4. Génération de schémas TOML à partir de la struct de la DataTable (row struct) lue dans l'éditeur.
5. `UE_PROJECT_NAME` et délais : à exposer ou laisser aux valeurs par défaut.
6. Tags du registre : élargir la liste fixe, ou lire d'autres sources.
