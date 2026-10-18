# Phase 4b — étape 2 : lecture des défauts (`ue_get_defaults`, `ue_diff_defaults`)

Tout est en **lecture seule**. Aucun enregistrement, aucune compilation. Les gabarits d'écriture n'existent pas
(le test statique `forbidden_calls_never_appear_in_templates` l'impose ; l'exécutable Windows ne contient ni
`compile_blueprint` ni aucune sonde).

## Ce qui a changé

| Sujet | Avant | Maintenant |
|---|---|---|
| Délai d'une commande côté Rust | 30 s | **15 s** (`BridgeConfig::default_multicast`) |
| Conversion des valeurs | `_plain` récursive, `list(valeur)`, `valeur[clé]` | `_safe` : aucune copie de conteneur, aucune lecture par clé |
| Tableau / set / map / struct | déplié (50 éléments, profondeur 2) | **`not_expanded`** (type + longueur) ; déplié sur demande (`expand`), **20 éléments au plus**, `itertools.islice` |
| Taille d'une commande | tout le Blueprint | **60 éléments au plus**, budget de **2 s** vérifié entre deux éléments ; au moins un élément par commande |
| Reprise | aucune | `next_index` (résultat partiel) ; l'outil enchaîne jusqu'à 12 commandes puis rend `complete:false` |
| Chargement | `load_asset` une fois | idem, **une fois par commande** (« un chargement par commande », jamais dans une boucle) |
| Asset Registry | `get_asset_by_object_path` (obsolète) | `k2_get_asset_by_object_path(SoftObjectPath)` ; l'ancienne API n'est qu'un repli, signalé dans `warnings` |
| Composants | `get_object` (obsolète) | `get_object_for_blueprint` seul |

`ue_blueprint_variables` suit les mêmes règles (paramètres `start` et `expand`, champs `next_index`,
`budget_exceeded`, `dirty_packages`). **Elle reste interdite sur le coffre (`BP_PL_Chest_Medium`) tant qu'elle n'a pas
été validée sur `BP_Test`.**

## Les outils

- `ue_get_defaults(object_path, section?, component?, names?, expand?, start?)`
  - `section` : `members` (défaut), `components`, `component_values` (avec `component` = nom de variable du composant).
  - `members` : un élément par membre `{name, owner, type, python_type, value}` ou `{name, owner, type, unreadable}`.
    `name` est le nom UPROPERTY **exact** ; `owner` la classe qui le déclare (vide pour une variable propre).
    Un membre illisible est toujours listé, avec la raison.
  - Clés JSON ASCII uniquement. Aucun nom affiché (abandonnés).
- `ue_diff_defaults(object_path_a, object_path_b, section?, component?, names?, expand?)`
  - Clé de comparaison : propriétaire + nom. Flottants égaux à 1e-6 près, entiers exacts.
  - Rend `differences` (A, B, types), `only_in_a_names`, `only_in_b_names`, et `not_compared_items`
    (illisible ou non déplié, avec la raison) : un conteneur non déplié n'est **jamais** déclaré égal.
  - Au-delà de 12 commandes pour un Blueprint : `ERREUR TooLarge`, restreindre avec `names`.

## Tests automatiques (sans Devkit)

- Balayage statique de tous les gabarits : pas de `list(`, `tuple(`, `dict(`, `value[`, `[key]`, `get_object(` ;
  une seule occurrence de l'ancienne API d'Asset Registry (le repli) ; `load_asset` une fois, au niveau de la fonction.
- Faux conteneur de **100 000 éléments** (tableau et map) : par défaut zéro itération (le faux conteneur lève s'il est
  itéré), `__getitem__` lève ; déplié : 20 éléments exactement, `more:true`, résultat < 4 ko.
- Lots de 60 sur 130 membres ; budget 0,05 s sur des lectures de 0,04 s : résultat partiel, reprise, aucun oubli ;
  un budget demandé à 999 s est ramené à 2 s.
- Outils : pagination, 12 commandes puis reprise par `start`, validation des paramètres avant tout envoi, curseur
  bloqué refusé, différences avec tolérance flottante.

## Validation (à faire sur signal de Robin, après les commits et le déploiement de l'exe — NON exécutée)

Préalable : l'exe déployé expose **15 outils** (`tools/list`). Ordre à respecter ; s'arrêter au premier écart,
sans réessayer.

1. `ue_status`. Attendu : projet `ConanSandbox`.
2. `ue_get_defaults` sur `/Game/Mods/test/BP_Test`, section `members` : `total` 121, `complete:true`, 3 commandes,
   `dirty_packages` vide. Puis section `components`.
3. `ue_diff_defaults` de `BP_Test` contre lui-même : `different` = 0, `only_in_a` = `only_in_b` = 0. Un `not_compared`
   non nul est normal (conteneurs non dépliés, membres illisibles) ; `identical` + `not_compared` = 121.
   Le test sur les valeurs illisibles/non dépliées ne doit rien déclarer égal.
4. `ue_blueprint_variables` sur `BP_Test` (validation du correctif 4a), puis `include_inherited:true`.
5. Test positif des packages sales, avec Robin : voir `test-package-sali.md`.
6. En dernier, le coffre : `ue_get_defaults` section `members`, **lot par lot**, un seul `start` par commande
   (`ue_spike defaults '{"object_path":"…/BP_PL_Chest_Medium.BP_PL_Chest_Medium","start":0}'`, puis 60, 120, 180, 240
   selon `next_index`) pour savoir exactement quel lot est parti en dernier. Si le Devkit tombe : s'arrêter, rapporter
   le dernier lot envoyé et le dernier journal, ne rien relancer.

`ue_blueprint_variables` reste interdit sur le coffre dans ce passage.

Temps attendu par commande : bien moins de 2 s. Au-delà de 10 s, l'éditeur plante (watchdog audio, voir `sonde.md`).
