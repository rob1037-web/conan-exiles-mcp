# Phase 4b — étape 1 : résultats de la sonde (lecture seule)

Sonde réalisée le 18 octobre sur le Devkit (UE 5.8.2, projet `ConanSandbox`, Python 3.11.8), avec
`ue_spike` et les gabarits de `crates/mcp-server/src/ue_bridge/probes.rs` (feature `test-support`,
absents du binaire livré). **Aucun enregistrement n'a été lancé.** Une seule compilation
(`compile_blueprint` sur `BP_Test`), sans save.

Artefacts conservés dans ce dossier (ne pas supprimer) :
`members_BP_PL_Mailbox.json`, `members_BP_PL_Chest_Medium.json` (énumération seule),
`inventaire_coffre_trace.json` (résultat de l'inventaire complet du coffre).
La trace `D:\ConanMod\probe_trace.log` (1770 lignes) a été supprimée avant que cette consigne n'existe :
elle n'est pas conservée. Ses chiffres sont repris ci-dessous.

## 1. Inventaire complet du coffre, avec trace durable

Cible `/Game/Systems/Building/Placeables/BP_PL_Chest_Medium`, tâche `inventory` d'origine (même conversion
récursive `_plain`), trace JSON avec `flush` + `fsync` avant chaque opération :

```json
{"cible":"BP_PL_Chest_Medium","membres":245,"lisibles":171,"illisibles":74,"conversions_enfant":171,"conversions_parent":170,"lignes_de_trace":1770,"fsync":1770,"profondeur_max":1,"listes":26,"maps":2,"cles_de_map":14,"liste_de_20_elements_ou_plus":0,"duree_totale_appel_s":0.164,"crash":false,"derniere_ligne":{"step":"done","members":245,"readable":171}}
```

**Prouvé**
- Pas de crash, appel complet en 0,164 s (trace comprise), alors que les deux crashs précédents avaient bloqué le
  thread jeu plus de 12 s.
- Des lectures par fenêtres de membres (tâches `inventory` avec `offset`/`limit`) ont déjà donné 245 membres et
  171 lisibles sur ce même asset avant l'appel complet tracé.
- Les deux crashs ont la même signature : plus aucune sortie Python pendant 12,5 s, puis
  `EXCEPTION_ACCESS_VIOLATION reading address 0xffffffffffffffff` dans `python311.dll` (7 premières frames), appelé par
  `PythonScriptPlugin`, avec au même instant un avertissement `AudioMixer … Timeout [10 Seconds]`.

**Supposé (non démontré)**
- Cause : état accumulé de la session (les deux crashs sont survenus après de nombreux appels de sonde ; le sondage
  réussi était le premier de sa session) et/ou un effet de la trace (une trace avec `fsync` change le déroulement).
  Le test qui trancherait (rejouer sans trace dans une session neuve) a été exclu : un seul essai était autorisé.
- Une explosion mémoire pendant le blocage est plausible (violation d'accès en fin de blocage), non vérifiée.

**Conséquence** : l'étape 2 ne s'appuie pas sur une propriété coupable (aucune n'est identifiée) mais sur des règles
structurelles (lots de 60, budget de 2 s, aucune itération complète de conteneur, un seul chargement par commande).

## 2. Écart de comptage : 244 / 170 puis 245 / 171

Ce n'est **pas le même asset**.

| Asset | Membres | Lisibles | Illisibles |
|---|---|---|---|
| `BP_PL_Mailbox` (la boîte) | 244 | 170 | 74 |
| `BP_PL_Chest_Medium` (le coffre) | 245 | 171 | 74 |

Les lectures par fenêtres faites plus tôt sur le coffre donnaient déjà 245 membres et 171 lisibles : le « 244 / 170 »
du rapport précédent venait de la boîte.

Le membre en plus est **`bLastOpenCloseFinished`**, présent seulement dans le coffre. L'énumération seule
(`list_member_variable_names`, sans `get_editor_property` ni conversion) le montre : c'est le seul nom qui n'est pas dans
la boîte, et le seul sans propriétaire qualifié, donc la seule **variable propre** du coffre (les 244 autres sont
héritées, avec un nom qualifié). Propriétés relevées : type `bool`, catégorie `Default`, valeur par défaut `true`, lisible.
D'où 245 membres, 171 lisibles, les 74 illisibles étant identiques. La boîte, elle, ne déclare aucune variable propre
(`own_variable_count: 0`). Les noms de membres sont uniques dans les deux listes (aucun doublon de nom simple).

Répartition des 245 membres du coffre par propriétaire : `/Script/Engine.Actor` 121, `BP_Master_Placeables_C` 45,
`/Script/ConanSandbox.BuildableBase` 44, `/Script/ConanSandbox.PlaceableBase` 30,
`BP_PlaceableItemContainer_C` 4, variable propre 1.

## 3. Couverture des champs demandés

Noms UPROPERTY exacts acceptés par `get_editor_property` ; les noms affichés avec espaces et le snake_case sont refusés
(sauf les 84 propriétés visibles dans `dir()` du CDO). La liste complète vient de
`blueprint.list_member_variable_names()` (nom qualifié par la classe propriétaire).

| Champ | Boîte | Coffre | Type | Propriétaire |
|---|---|---|---|---|
| `MaxHealth` | 1000 | 5000 | `int` | `/Script/ConanSandbox.BuildableBase` |
| `DestroyActorOnZeroHealth` | true | true | `bool` | `/Script/ConanSandbox.PlaceableBase` |
| `DoNotSaveToDB` | false | false | `bool` | `BP_Master_Placeables_C` |
| `CanBeDismantled` | true | true | `bool` | `BP_Master_Placeables_C` |
| `InventorySharingAccess` | `InventoryShareAccess.ON` | idem | `byte` (enum) | `BP_PlaceableItemContainer_C` |
| `SmartObjectInteractRange` | 500.0 | 500.0 | `real(double)` | `BP_Master_Placeables_C` |
| composant `StaticMesh`, `static_mesh` | `…/Mailbox/SM_BoiteAuxLettres` | `…/FLX/SM_container_basic_wood1_Base` | objet | hérité dans les deux |

- float contre double : `get_member_variable_type().export_text()` donne `PinCategory="real"` avec
  `PinSubCategory="float"` ou `"double"`. Python ne voit qu'un `float`.
- Types vus dans l'inventaire : voir le champ `types_seen` de `inventaire_coffre_trace.json` (liste exacte).
- Composants propres du coffre relevés : `Lid_GEN_VARIABLE` et `BP_BAC_Storage_GEN_VARIABLE` (non comparés
  systématiquement à ceux de la boîte).
- Composants hérités : `SubobjectDataSubsystem.k2_gather_subobject_data_for_blueprint` + 
  `SubobjectDataBlueprintFunctionLibrary` (`get_variable_name`, `is_inherited_component`, `is_native_component`,
  `get_object_for_blueprint`). Le modèle de composant est `…/BP_PL_Mailbox_C:StaticMesh_GEN_VARIABLE`.
- `is_editor_property_overridden` renvoie toujours `true` : inutilisable. Comparer au CDO du parent exige de
  décomposer les structs (la chaîne d'un struct contient son adresse mémoire).
- Aucune API Python de métadonnées pour lire un `DisplayName` : les noms affichés sont abandonnés.

## 4. Fichiers sur disque : règle `Local/`

`SystemLibrary.get_system_path(asset)` renvoie un chemin qui **n'existe pas** :
`…/Content/Mods/test/BP_Test.uasset`. Le vrai fichier est dans `Local/`.

| Package | Fichier réel (sous `D:\Program Files\CEUE5Devkit\UE4\Content\Mods\test\`) | Taille | Modifié |
|---|---|---|---|
| `/Game/Mods/test/BP_Test` | `Local/BP_Test.uasset` | 32 476 | 2026-10-09 16:20 |
| `/Game/Mods/test/test/Mailbox/BP_PL_Mailbox` | `Local/test/Mailbox/BP_PL_Mailbox.uasset` | 128 815 | 2026-10-15 22:41 |
| `/Game/Mods/test/test/Mailbox/SM_BoiteAuxLettres` | `Local/test/Mailbox/SM_BoiteAuxLettres.uasset` | 84 169 | 2026-10-14 17:28 |

Règle vérifiée sur ces **3 assets** : `/Game/Mods/<mod>/<reste>` correspond à `Content/Mods/<mod>/Local/<reste>.uasset`.

- **Fichiers voisins : aucun** (`.uexp`, `.ubulk`, `.uptnl` absents) pour ces trois assets.
- Contenu de `Content/Mods/test/` : `Local/`, `Shared/`, `Content/` (dont `Dev/AlmostEmpty.umap`, une carte), 
  `modinfo.json`, `active.txt`.
- **Autosaves de l'éditeur** (`UE4/Saved/Autosaves/`, relevé du 18 octobre) : `Game/Mods/test/BP_Test_Auto1/2.uasset`
  (22 766 / 55 152), `DT_Purge_Nordheimer_Auto1/2.uasset` (3 637 chacun), `test/Mailbox/BP_PL_Mailbox_Auto1…7.uasset`
  (74 961 à 116 953), `test/SM_BoiteAuxLettres_Auto1/2.uasset` (83 748 / 83 652), `Game/Dev/AlmostEmpty_Auto1…4.umap`
  (≈ 1,35 Mo) et `PackageRestoreData.json` (96 octets). Ils suivent le chemin du package (pas `Local/`).
  Ce ne sont pas les fichiers à sauvegarder.
- Conséquence pour `UASSET_BACKUP_ROOT` : résoudre `Local/<reste>` puis `<reste>` direct, refuser si aucun ou si les
  deux existent, copier tous les fichiers de même nom de base. L'endroit où l'éditeur **écrit** réellement
  (`Local/` ou autre) reste à confirmer par le vrai save supervisé de l'étape 3.

## 5. Packages sales et limites

```json
{"lecture_complete_BP_Test":{"membres":121,"convertis":61,"illisibles":60,"sales_avant":[],"sales_apres":[]},"compile_BP_Test_sans_save":{"sales_avant":[],"sales_apres":[],"statut_avant":"BS_UP_TO_DATE","statut_apres":"BS_UP_TO_DATE","fichier_modifie":false}}
```

- Aucun package sali avant ou après la lecture complète des défauts, ni avant ou après la compilation. Aucun package
  supplémentaire à nommer. Le fichier `Local/BP_Test.uasset` n'a pas bougé (date du 9 octobre).
- L'API : `EditorLoadingAndSavingUtils.get_dirty_content_packages()` et `get_dirty_map_packages()`.
- **Limite** : `BP_Test` était déjà à jour ; la compilation n'a pas forcément réécrit quelque chose. La détection n'a jamais
  signalé un package sali : on ne sait donc pas qu'elle fonctionne. Un test positif (modification sans enregistrement)
  est prévu au début de l'étape 3 : voir `test-package-sali.md`.
- **Limite** : après la sonde, `BP_Test` n'est pas marqué modifié dans l'éditeur.
- **Limite** : un appel Python qui bloque le thread jeu plus de 10 s fait planter l'éditeur (watchdog audio). Aucun appel
  ne peut être interrompu une fois lancé ; seule la prévention marche.

## 6. APIs obsolètes rencontrées (journal de l'éditeur)

- `AssetRegistry.get_asset_by_object_path` : « Use other version that takes a SoftObjectPath ».
- `SubobjectDataBlueprintFunctionLibrary.get_object` : « Use GetAssociatedObject - it will not return template obj » ;
  on garde `get_object_for_blueprint`, qui renvoie le modèle.
- Plusieurs propriétés en snake_case (`life_span`, `data_layers`, `on_actor_touch`…) émettent un avertissement
  d'obsolescence quand `dir()` les évalue ; sans effet.

## 7. Règles retenues pour l'étape 2

Lecture par liste de noms exacts, lots de 60, budget de 2 s vérifié entre deux membres (résultat partiel +
`next_index`), maps / structs / tableaux en `not_expanded` par défaut (dépliage : 20 éléments au plus avec `islice`,
jamais `list(valeur)` ni `valeur[clé]` sur une map), un seul chargement d'asset par commande, délai des commandes Rust
ramené de 30 s à 15 s, même correctif pour `ue_blueprint_variables` (sans l'appeler sur le coffre avant validation sur
`BP_Test`).
