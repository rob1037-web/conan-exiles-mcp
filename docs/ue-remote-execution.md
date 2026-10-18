# Protocole « Python Remote Execution » de l'éditeur Unreal (Devkit Conan Exiles)

Spécification établie à partir du client de référence fourni avec le Devkit. Toutes les lignes citées
sont celles de ce fichier (649 lignes, `remote_execution.py`, daté du 9 octobre) :

```
D:\Program Files\CEUE5Devkit\Engine\Plugins\Experimental\PythonScriptPlugin\Content\Python\remote_execution.py
```

- Moteur : **5.8.2** (`Engine\Build\Build.version` : MajorVersion 5, MinorVersion 8, PatchVersion 2,
  branche `++exiles+release`).
- Protocole : **version 1**, magic `ue_py` (lignes 12-13). C'est ce que décrit l'ordre de mission : pas
  d'écart de version, on continue.
- Activation dans le projet : `UE4\Config\DefaultEngine.ini`, lignes 1442-1443 :
  `[/Script/PythonScriptPlugin.PythonScriptPluginSettings]` `bRemoteExecution=True`. Le projet est
  `ConanSandbox` (`UE4\ConanSandbox.uproject`).
- Le côté éditeur (C++, `PythonScriptRemoteExecution.cpp`) n'est **pas** livré avec le Devkit (pas de
  dossier `Source`). Le fichier Python n'en montre que ce qu'un client voit ; les points non
  vérifiables ici sont listés en fin de document.

## 1. Constantes

| Constante | Valeur | Ligne |
|---|---|---|
| version du protocole | `1` | 12 |
| magic | `'ue_py'` | 13 |
| types | `ping`, `pong`, `open_connection`, `close_connection` (UDP) ; `command`, `command_result` (TCP) | 14-19 |
| intervalle de ping | 1 s | 21 |
| expiration d'un nœud sans pong | 5 s | 22 |
| TTL multicast | `0` (machine locale) | 24 |
| groupe multicast | `('239.0.0.1', 6766)` | 25 |
| adresse de liaison multicast | `'127.0.0.1'` | 26 |
| endpoint de commande (TCP, côté client) | `('127.0.0.1', 6776)` | 27 |
| taille de lecture | `8192` octets | 28 |

Le groupe, le port et l'adresse de liaison « doivent correspondre » aux réglages *Multicast Group
Endpoint* / *Multicast Bind Address* du plugin (commentaires lignes 25-26). Les valeurs de l'ordre de
mission (239.0.0.1:6766, bind 127.0.0.1, TTL 0) sont celles de ce fichier.

Modes d'exécution (lignes 31-33), valeurs textuelles exactes :

| Mode | Valeur | Usage |
|---|---|---|
| `MODE_EXEC_FILE` | `ExecuteFile` | script littéral de plusieurs instructions (défaut de `run_command`, ligne 111) |
| `MODE_EXEC_STATEMENT` | `ExecuteStatement` | une instruction ; le résultat est imprimé |
| `MODE_EVAL_STATEMENT` | `EvaluateStatement` | une expression ; la valeur est renvoyée |

## 2. Format des messages (UDP et TCP)

Un message est **un objet JSON encodé en UTF-8** (`to_json_bytes`, lignes 554-562 ; `json.dumps(...,
ensure_ascii=False)`, ligne 552). Champs (lignes 542-551) :

| Champ | Type | Présence | Sens |
|---|---|---|---|
| `version` | entier | toujours | `1` ; tout autre valeur fait rejeter le message (lignes 577-578) |
| `magic` | chaîne | toujours | `"ue_py"` ; sinon rejet (lignes 579-580) |
| `type` | chaîne | toujours, non vide | un des six types (lignes 538-539, 582) |
| `source` | chaîne | toujours, non vide | identifiant du nœud émetteur (lignes 540-541, 583) |
| `dest` | chaîne | **omis si vide** | identifiant du nœud destinataire ; absent = tous les nœuds (UDP) (lignes 548-549, 587) |
| `data` | objet | **omis si vide ou falsy** | charge utile propre au type (lignes 550-551, 588) |

Réception : un message est rejeté (et loggé) s'il ne se parse pas ou si `version`/`magic` diffèrent
(lignes 574-592). Filtre de réception (lignes 519-529) : le message est ignoré si `source` est notre propre
identifiant, ou si `dest` est renseigné et différent du nôtre.

Identifiant de nœud : `str(uuid.uuid4())` (ligne 56), soit un GUID v4 avec tirets. Le côté éditeur
est en C++ ; **utiliser un vrai UUID v4** est le choix sûr.

## 3. Charges utiles (`data`)

| Type | Sens | `data` | Réf. |
|---|---|---|---|
| `ping` | client -> tous (UDP) | aucune | 323 |
| `pong` | éditeur -> tous (UDP) | décrit l'instance ; le client le conserve tel quel, avec `node_id = source` | 371-378, 169-175 |
| `open_connection` | client -> nœud (UDP) | `{"command_ip": <str>, "command_port": <int>}` | 332-335 |
| `close_connection` | client -> nœud (UDP) | aucune | 344 |
| `command` | client -> éditeur (TCP) | `{"command": <str>, "unattended": <bool>, "exec_mode": <str>}` | 434-438 |
| `command_result` | éditeur -> client (TCP) | résultat ; le client de référence lit au moins `success` (bool) et `result` (str) | 439-440, 125-127 |

Contenu exact de `pong` et de `command_result` : le client Python ne les interprète pas (il renvoie
`data` tel quel, lignes 172-174 et 440). Voir « Points non vérifiables ».

## 4. Séquence

1. **Découverte (UDP multicast).** Le client ouvre un socket UDP lié à `(bind_address, 6766)`, avec
   `SO_REUSEPORT` ou, à défaut, `SO_REUSEADDR` (lignes 259-263 : indispensable car l'éditeur écoute
   sur le même port), active `IP_MULTICAST_LOOP` (264), règle `IP_MULTICAST_TTL` (265) et
   `IP_MULTICAST_IF` (266), puis rejoint le groupe `239.0.0.1` sur l'interface `bind_address`
   (`IP_ADD_MEMBERSHIP`, 267). Lecture avec timeout de 0,1 s (268).
2. **`ping`.** Envoyé au groupe `239.0.0.1:6766` à l'ouverture puis toutes les secondes
   (lignes 313-323, `sendto` ligne 311).
3. **`pong`.** Chaque éditeur répond (UDP) ; le client retient `source` -> `data`, rafraîchit
   l'horodatage, et oublie un nœud sans pong depuis plus de 5 s (lignes 371-378, 192-204, 141-151).
4. **`open_connection`.** Le client **ouvre d'abord un serveur TCP** (`bind` sur `command_endpoint`,
   `listen(1)`, timeout d'accept 5 s, lignes 473-484), puis diffuse en UDP
   `open_connection` avec `dest = <node_id de l'éditeur>` et `data = {command_ip, command_port}`
   (lignes 325-335). Il retente jusqu'à **6 fois**, en attendant 5 s l'`accept` à chaque fois, soit
   30 s au plus (lignes 486-501, `accept` ligne 496). Échec : `RuntimeError('Remote party failed to
   attempt the command socket connection!')` (ligne 501).
5. **Connexion TCP entrante.** C'est **l'éditeur qui se connecte** à `command_ip:command_port`
   (le client est le serveur TCP). Après `accept`, le socket passe en mode bloquant (ligne 497).
   Comme l'adresse et le port sont transmis dans `open_connection`, un **port éphémère** (port 0
   puis port réellement alloué) convient.
6. **`command` -> `command_result`.** Le client envoie un message `command` (TCP, ligne 449
   `sendall`) et attend un message `command_result` (ligne 439, `_receive_message`, 451-471).
   Un autre type, un message invalide ou une réponse vide : `RuntimeError('Remote party failed to
   send a valid response!')` (ligne 471).
7. **Fermeture.** Le client diffuse `close_connection` (UDP, `dest` = éditeur), ferme le socket de
   canal puis le socket d'écoute (lignes 407-420 ; `stop()` appelle ceci, ligne 75).

## 5. Découpage TCP (framing)

**Il n'y a ni préfixe de longueur ni délimiteur.** Un message est un JSON UTF-8 brut :

- émission : `sendall(json_bytes)` (ligne 449) ;
- réception : boucle `recv(8192)` qui concatène les morceaux et **s'arrête dès qu'un morceau fait
  moins de 8192 octets** (lignes 461-466 pour TCP ; 284-297 pour l'UDP).

Cette heuristique suppose qu'un message arrive en un bloc. Elle échoue si un message est coupé en
deux segments TCP dont le premier est plus court que 8192 octets, ou si sa taille est un multiple
exact de 8192. **Le client Rust ne doit pas la reproduire** : il accumule les octets jusqu'à obtenir
un objet JSON complet (parse réussi) avec un délai global.

## 6. Valeurs par défaut

- Multicast : `239.0.0.1:6766`, interface `127.0.0.1`, TTL `0`, boucle locale activée.
- Commande TCP : `127.0.0.1:6776` (fixe dans le client de référence ; l'éditeur utilise ce que dit
  `open_connection`).
- Délais : ping 1 s, expiration de nœud 5 s, accept 5 s x 6 tentatives, lecture UDP 0,1 s.
- Le paramètre `unattended` est `True` par défaut (ligne 111).

## 7. Points non vérifiables depuis ce fichier

1. **Contenu du `pong`.** Défini par le C++ (non livré). Le client Python n'en lit que `source`.
   Les champs d'identification utilisés pour choisir le nœud (`project_name`, `project_root`,
   `engine_version`, `machine`, `user`...) sont ceux de l'implémentation d'Epic mais **ne sont
   pas confirmés ici** : le module `ue_bridge` doit les lire sans présumer, et `ue_status` doit pouvoir
   renvoyer le `pong` brut. Le premier test réel (docs/phase-4a-test.md) les établit.
2. **Contenu de `command_result`.** Au minimum `success` et `result` (lignes 125-127). Un tableau de
   sortie (`output`) existe dans l'implémentation d'Epic mais n'apparaît pas dans ce fichier ; le
   module ne dépend pas de lui : les scripts impriment leur résultat entre marqueurs, récupérés
   dans `result` **ou** dans la sortie si elle est fournie.
3. **Comportement exact de `ExecuteFile` sur un script littéral** (c'est le mode du client de
   référence par défaut, ligne 111) : le résultat d'un `print` est rapporté par l'éditeur dans la
   sortie du `command_result`, pas dans `result`. À vérifier au premier appel réel.
