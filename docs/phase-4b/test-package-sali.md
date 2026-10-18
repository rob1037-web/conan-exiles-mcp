# Test positif de détection des packages sales (début de l'étape 3)

**Ne pas exécuter avant l'étape 3, avec Robin devant le Devkit.** Jusqu'ici la détection
(`EditorLoadingAndSavingUtils.get_dirty_content_packages()` / `get_dirty_map_packages()`) n'a jamais rien
signalé : on ne sait pas qu'elle fonctionne (voir `sonde.md`, section 5). Ce test la prouve dans le sens positif.
Aucun enregistrement, aucune écriture, aucune compilation.

## Préconditions
- Devkit ouvert, projet `ConanSandbox`, aucun asset en attente de sauvegarde (rien de modifié dans le Content Browser).
- `BP_Test` (`/Game/Mods/test/BP_Test`) existe et n'est pas ouvert avec des modifications.
- Le serveur de la branche contient la tâche `read_state` (sonde) ou l'outil de l'étape 3 équivalent.

## Déroulé
1. **Référence** : lancer `read_state` sur `BP_Test`. Attendu : `sales_avant` et `sales_apres` vides.
2. **Robin modifie `BP_Test` dans l'éditeur sans enregistrer** (par exemple un défaut d'une variable, ou déplacer
   un nœud). Ne pas utiliser Ctrl+S, ne pas fermer l'asset ni l'éditeur.
3. **Lancer `read_state`.** Attendu : la liste des packages sales contient `/Game/Mods/test/BP_Test` (et rien d'autre).
   - Si la liste est vide : la détection est **inopérante** ; l'étape 3 ne doit pas utiliser cette garde seule
     (voir plan B ci-dessous). Arrêter, rapporter.
   - Si d'autres packages apparaissent : les nommer dans le rapport ; ils bloquent tout save futur.
4. **Robin annule** la modification (Ctrl+Z jusqu'à l'état d'origine). Attention : l'annulation ne retire pas
   toujours l'indicateur « modifié » du package.
5. **Lancer `read_state`.** Attendu : liste vide.
   - Si `BP_Test` reste listé après annulation : le comportement est « sali puis annulé = encore sale » ; le noter
     et décider avec Robin (la garde « refuser si sales » devient plus stricte que prévu, ce qui est acceptable,
     et la procédure doit prévoir de fermer/rouvrir l'asset sans enregistrer).
6. Rapport : les trois sorties brutes (avant / modifié / annulé), sans interprétation.

## Plan B si la détection est inopérante
Comparer la date de modification et la taille du fichier réel (`Content/Mods/<mod>/Local/<reste>.uasset`) avec celles
relevées avant la commande, et refuser si un asset du mod est ouvert dans l'éditeur. Décision à prendre avec Robin.

## Garde-fous
- Aucune commande d'écriture n'est émise pendant ce test ; le gabarit `read_state` est vérifié « lecture seule »
  par le test statique des gabarits.
- Si l'éditeur ne répond plus plus de 15 s : ne rien relancer, ne pas relancer la commande, prévenir Robin.
