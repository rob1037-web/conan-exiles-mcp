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

## Garde-fou `EditorBusy` (commande expirée)

Quand Rust abandonne (délai de 15 s), **Python peut tourner encore** dans l'éditeur : le délai ne l'interrompt pas, et
un éditeur occupé ne répond sans doute pas aux pings (non mesuré). Sans garde-fou, l'appel suivant échouerait avec un
`DevkitNotRunning` trompeur, ou enverrait une commande de plus à un éditeur déjà bloqué.

- **Le drapeau « suspect » est posé** (dans `ConanServer`, en mémoire) dans deux cas, après l'envoi de la commande :
  1. `Timeout` à l'étape « exécution de la commande dans l'éditeur » ;
  2. toute `ProtocolError` (connexion fermée, JSON illisible ou trop gros, mauvais type de message, `success`
     manquant, marqueurs absents de la sortie ; l'échec d'écriture de la commande compte aussi, par prudence).
  Ne le posent pas : la découverte (`DevkitNotRunning`), l'expiration de la connexion TCP (rien n'est parti), une
  erreur Python (l'éditeur a répondu) et les erreurs de socket locales.
- **Drapeau posé** : tous les outils `ue_*` sauf `ue_status` répondent `ERREUR EditorBusy: …` **sans rien envoyer**
  (ni ping, ni connexion, ni commande). Le message dit de ne pas relancer et de prévenir Robin. Une lecture enchaînée
  (`ue_get_defaults`, `ue_diff_defaults`) s'arrête donc à la commande suivante. Les outils CSV/schémas ne sont pas
  concernés.
- **Levée** : seul un `ue_status` **réussi** lève le drapeau (la réponse contient alors `busy_flag_cleared:true`).
  Un `ue_status` en échec le garde et l'indique : « éditeur peut-être occupé (commande précédente expirée) » (champ
  `note` / `busy_suspected:true` si l'éditeur ne répond pas du tout, ou suffixe du message d'erreur).
- Pas d'expiration automatique : le drapeau survit tant que `ue_status` n'a pas réussi, y compris si l'éditeur a
  vraiment été fermé (relancer le Devkit, puis `ue_status`).
- Redémarrer le serveur MCP (Claude Desktop) remet aussi le drapeau à zéro ; ce n'est pas la procédure normale.
- Tests : `tests/ue_tools.rs` (faux éditeur muet : l'outil suivant n'envoie rien, `ue_status` en échec garde le
  drapeau, `ue_status` réussi le lève ; ruptures de protocole ; erreurs qui ne posent pas le drapeau ; lecture
  enchaînée interrompue).

## Procédure de validation (exécutée le 18 octobre, résultats plus bas)

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

## Résultats de la validation (18 octobre, Devkit ouvert, exe déployé = exe du commit 4)

Pilotage : `validation/mcp_seq.py` (session MCP réelle sur `mcp-server.exe`, appels séquentiels, arrêt au premier KO)
et `validation/chest_batches.py` (coffre lot par lot via `ue_spike`). Sorties complètes conservées hors dépôt (dossier
`CONAN_VAL_OUT`). Aucun enregistrement, aucune compilation. Éditeur sans Timeout ni plantage, garde-fou `EditorBusy`
jamais déclenché.

| Étape | Résultat | Durée | Détail |
|---|---|---|---|
| 0. `tools/list` | OK | – | 15 outils |
| 1. `ue_status` | OK | 1,45 s | joignable, `ConanSandbox`, moteur 5.8.2, Python 3.11.8, 85 240 assets |
| 2a. `ue_get_defaults` membres, `BP_Test` | OK | 2,92 s | 121 membres, 3 commandes, `complete:true`, `dirty_packages:[]` ; 61 lisibles, 60 illisibles |
| 2b. `ue_get_defaults` composants, `BP_Test` | OK | 1,09 s | 2 éléments (`Default__BP_Test_C`, `DefaultSceneRoot`) |
| 3. `ue_diff_defaults` `BP_Test` / `BP_Test` | OK | 6,52 s | 6 commandes, `different:0`, `only_in_*`:0, `identical` 50 + `not_compared` 71 = **121** (71 = 60 illisibles + 11 conteneurs ou structs non dépliés) |
| 4. `ue_blueprint_variables` `BP_Test` | OK | 1,09 s | `warnings:[]`, `package_marked_modified:false`, aucune variable propre |
| 4b. même outil, `include_inherited:true` | OK | 0,89 / 1,09 / 1,10 s | fenêtres `start` 0, 60, 120 : 121 variables ; `next_index` 60, 120, absent |
| 5. packages sales | OK | 0,80 s (modifié), puis lecture après annulation | voir ci-dessous |
| 6. coffre, lot par lot | OK | 1,10 à 1,22 s par lot | 5 lots (60, 60, 60, 60, 5) = 245 membres : 171 lisibles, 74 illisibles ; `dirty_packages:[]` |
| 7. coffre, `ue_get_defaults` enchaîné (MCP) | OK | 5,18 s | 5 commandes, `complete:true`, 245 membres : 171 lisibles, 74 illisibles ; `MaxHealth` 5000, `SmartObjectInteractRange` 500.0, `InventorySharingAccess` `InventoryShareAccess.ON`, `bLastOpenCloseFinished` true |
| `ue_describe_asset` `BP_Test` | OK | 0,90 s | classe `Blueprint`, `ParentClass` `Actor`, `warnings:[]` : `k2_get_asset_by_object_path(SoftObjectPath)` fonctionne sans repli |
| Fin | OK | – | `ue_status` joignable ; `dirty_packages:[]` ; `Local/BP_Test.uasset` toujours 32 476 octets (`list_mod_files`, source mod) |

### Étape 5 : modification de `BP_Test` par Robin
Dans les Class Defaults de `BP_Test`, **Net Load on Client décoché** (propriété probablement `bNetLoadOnClient`),
**sans enregistrer**. Résultats, relevés tels quels avec `ue_get_defaults` (`names:["bHidden"]`, un seul lot) :
1. Avant : `dirty_packages:[]` (relevé à l'étape 4).
2. « modifié » : `dirty_packages:["/Game/Mods/test/BP_Test"]` — la détection fonctionne dans le sens positif, rien d'autre.
3. Annulation par **Ctrl+Z** : `dirty_packages:[]`. Aucun Reload nécessaire. (L'annulation retire donc bien
   l'indicateur « modifié » du package ; ce n'est pas garanti pour toute modification.)

### Notes de séance, telles quelles
- **Relecture du lot 0 de l'étape 6.** Le premier passage du script de lots a envoyé le lot 0 (`start=0`), l'éditeur a
  répondu, puis le script a échoué à lire la réponse (il a pris l'accolade de la ligne « noeud: … » de `ue_spike` pour
  le début du JSON). La réponse étant perdue, le lot 0 a été **relu** avant de continuer. Ce n'est pas un nouvel essai
  après un échec de l'éditeur : un défaut du script, corrigé (le JSON commence après la première ligne). Le lot 0 est
  donc parti deux fois ; les deux lectures sont en lecture seule.
- **Durée négative.** La mesure de l'étape 5 « annulé » a affiché −2,01 s : le pilote utilisait l'horloge murale
  (`time.time()`), qui a sauté sous WSL (resynchronisation). La durée de cet appel n'est pas fiable (non reportée
  dans le tableau). Correctif : `time.monotonic()` partout dans le pilote (`validation/mcp_seq.py`). Toutes les autres
  durées ont été mesurées après le correctif, sauf les étapes 1 à 4 et l'étape 5 « modifié », mesurées avant : elles
  sont positives et cohérentes (≈ 1 s par commande), sans saut constaté.
