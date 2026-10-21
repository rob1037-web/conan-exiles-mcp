# Fixtures d'encodage

Fichiers **synthétiques** (générés à la main, pas des exports réels du Devkit) qui reproduisent le format
observé sur `LoreObjectsTable.csv` : colonnes `---,Name,Text,Category`, valeurs entre guillemets, FText
`NSLOCTEXT("espace", "clé", "texte")`, virgules dans les textes. **Échappement des textes** (observé sur l'export réel,
clés 14, 34 et 1011) : `\"` pour un guillemet, `\'` pour une apostrophe, `\r\n` (échappements, pas un saut de ligne
brut) pour un retour à la ligne ; le doublement des guillemets (`""`) est celui du CSV, défait au parsing.

| Fichier | Encodage | Fins de ligne |
|---|---|---|
| `lore_utf16le_bom_crlf.csv` | UTF-16 LE avec BOM (`FF FE`), accents | CRLF |
| `lore_utf8_bom_crlf.csv` | UTF-8 avec BOM (`EF BB BF`), accents | CRLF |
| `lore_ascii_lf.csv` | ASCII (UTF-8 sans BOM) | LF |

Les tests (`crates/core/tests/encodings.rs`) exigent qu'une lecture suivie d'une réécriture sans
modification redonne ces fichiers à l'octet près. À remplacer ou compléter par un export réel.
