//! Syntaxe des textes localisés `NSLOCTEXT("espace", "clé", "texte")` des DataTables du Devkit.
//!
//! Dans un export, la valeur d'une cellule de texte est du code Unreal: trois littéraux de chaîne C++
//! entre guillemets. Un guillemet à l'intérieur d'un littéral s'écrit `\"`, une apostrophe `\'`, un
//! retour à la ligne `\r\n` (deux séquences d'échappement, jamais un saut de ligne brut). Le doublement
//! des guillemets (`""`) est celui du **CSV**, qui se défait au parsing: il ne fait pas partie de la
//! valeur. Une valeur qui ne respecte pas cette syntaxe s'importe sans erreur, mais Unreal l'affiche
//! brute: on la refuse avant l'écriture.
//!
//! Vérifié sur l'export réel `LoreObjectsTable.csv` (579 valeurs `NSLOCTEXT`, aucune refusée; seuls les
//! échappements `\"`, `\'`, `\r`, `\n` y apparaissent). Seule la **syntaxe** est vérifiée: ni le format
//! de l'espace de noms ni celui de la clé, que le Devkit ne contraint pas.

/// Début d'une valeur `NSLOCTEXT`. Les autres formes (`LOCTABLE(...)`, texte brut) ne sont pas vérifiées.
pub const PREFIX: &str = "NSLOCTEXT(";

/// Échappements admis après un antislash dans un littéral (échappements simples du C++).
const ESCAPES: [char; 11] = ['\\', '"', '\'', '?', 'a', 'b', 'f', 'n', 'r', 't', 'v'];

/// La valeur commence-t-elle par `NSLOCTEXT(` (donc doit-elle être bien formée) ?
pub fn is_nsloctext(value: &str) -> bool {
    value.starts_with(PREFIX)
}

/// Vérifie la syntaxe d'une valeur `NSLOCTEXT(...)`: trois littéraux, guillemets internes échappés,
/// parenthèse fermante, rien après. `Err` décrit le premier problème, avec sa position (en
/// caractères, à partir de 1).
pub fn check_nsloctext(value: &str) -> Result<(), String> {
    let chars: Vec<char> = value.chars().collect();
    let mut i = PREFIX.chars().count();
    for n in 1..=3 {
        skip_spaces(&chars, &mut i);
        literal(&chars, &mut i, n)?;
        skip_spaces(&chars, &mut i);
        if n < 3 {
            match chars.get(i) {
                Some(',') => i += 1,
                other => {
                    return Err(format!(
                        "attendu `,` après le littéral n°{n} à la position {}, trouvé {}",
                        i + 1,
                        describe(other)
                    ))
                }
            }
        }
    }
    match chars.get(i) {
        Some(')') if i + 1 == chars.len() => Ok(()),
        Some(')') => Err(format!(
            "texte inattendu après la parenthèse fermante, à la position {}",
            i + 2
        )),
        other => Err(format!(
            "attendu `)` après le troisième littéral à la position {}, trouvé {}; un guillemet à \
             l'intérieur du texte doit s'écrire \\\"",
            i + 1,
            describe(other)
        )),
    }
}

fn describe(c: Option<&char>) -> String {
    match c {
        Some(c) => format!("{c:?}"),
        None => "la fin de la valeur".into(),
    }
}

fn skip_spaces(chars: &[char], i: &mut usize) {
    while chars.get(*i) == Some(&' ') {
        *i += 1;
    }
}

/// Lit un littéral de chaîne qui commence à `chars[*i]`; `*i` passe après le guillemet fermant.
fn literal(chars: &[char], i: &mut usize, n: usize) -> Result<(), String> {
    if chars.get(*i) != Some(&'"') {
        return Err(format!(
            "littéral n°{n}: guillemet ouvrant attendu à la position {}, trouvé {}",
            *i + 1,
            describe(chars.get(*i))
        ));
    }
    let start = *i + 1;
    *i += 1;
    loop {
        match chars.get(*i) {
            None => {
                return Err(format!(
                    "littéral n°{n} commencé à la position {start} jamais fermé (guillemet fermant manquant)"
                ))
            }
            Some('"') => {
                *i += 1;
                return Ok(());
            }
            Some('\\') => match chars.get(*i + 1) {
                Some(c) if ESCAPES.contains(c) => *i += 2,
                Some(c) => {
                    return Err(format!(
                        "séquence d'échappement `\\{c}` inconnue dans le littéral n°{n}, position {} \
                         (admis: \\\\ \\\" \\' \\? \\a \\b \\f \\n \\r \\t \\v)",
                        *i + 1
                    ))
                }
                None => {
                    return Err(format!(
                        "antislash isolé en fin de valeur (littéral n°{n}, position {})",
                        *i + 1
                    ))
                }
            },
            Some(c @ ('\n' | '\r')) => {
                return Err(format!(
                    "retour à la ligne brut ({c:?}) dans le littéral n°{n}, position {}: l'écrire \
                     `\\r\\n` (échappé)",
                    *i + 1
                ))
            }
            Some(_) => *i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(v: &str) {
        assert_eq!(check_nsloctext(v), Ok(()), "{v}");
    }

    fn err(v: &str) -> String {
        check_nsloctext(v).expect_err(v)
    }

    #[test]
    fn forms_found_in_the_real_export_are_accepted() {
        ok(r#"NSLOCTEXT("", "C975A22C44D0E510FAE1558B3736E1CB", "Sealing the Borders")"#);
        ok(
            r#"NSLOCTEXT("[01D4587E3401BCF17310899AB495BFD5]", "A9707FDD4FF09FB70ED7B6907BBAB162", "Slavé, War-maker Klæl")"#,
        );
        // \" et \' échappés, \r\n sous forme d'échappements (clés 14, 34 et 1011 de LoreObjectsTable)
        ok(r#"NSLOCTEXT("", "A156", "Il dit \"non\", c\'ets fini")"#);
        ok(r#"NSLOCTEXT("", "FA79", "Harlot\'s Journal #6")"#);
        ok(r#"NSLOCTEXT("", "A48A", "base.\r\n\r\nThe mere touch \r\nof a hand")"#);
        // littéraux vides, espaces variables, antislash échappé
        ok(r#"NSLOCTEXT("","","")"#);
        ok(r#"NSLOCTEXT(  "a" ,  "b"  ,  "c\\d"  )"#);
    }

    #[test]
    fn raw_double_quotes_inside_the_text_are_refused_e3() {
        // le cas E3: guillemets bruts dans le texte (ce que donne un CSV doublé mal compris)
        let e = err(r#"NSLOCTEXT("", "k", "Il dit "non" puis part")"#);
        assert!(e.contains("attendu `)`") && e.contains("\\\""), "{e}");
        assert!(e.contains("'n'"), "{e}");
        // guillemet brut en toute fin de texte
        err(r#"NSLOCTEXT("", "k", "Il dit "non"")"#);
    }

    #[test]
    fn csv_level_doubled_quotes_that_were_not_undone_are_refused() {
        let e = err(r#"NSLOCTEXT("""", ""k"", ""texte"")"#);
        assert!(e.contains("attendu `,` après le littéral n°1"), "{e}");
    }

    #[test]
    fn structural_errors_name_the_literal_and_the_position() {
        // deux littéraux seulement: la virgule manque après le deuxième
        assert!(err(r#"NSLOCTEXT("", "k")"#).contains("attendu `,` après le littéral n°2"));
        assert!(err(r#"NSLOCTEXT("", "k", "t", "u")"#).contains("attendu `)`"));
        assert!(err(r#"NSLOCTEXT("", "k", "t""#).contains("attendu `)`"));
        assert!(err(r#"NSLOCTEXT("", "k", "t) "#).contains("jamais fermé"));
        assert!(err(r#"NSLOCTEXT("", "k", "t") x"#).contains("après la parenthèse"));
        assert!(err(r#"NSLOCTEXT(, "k", "t")"#).contains("guillemet ouvrant attendu"));
        assert!(err(r#"NSLOCTEXT("", k, "t")"#).contains("littéral n°2"));
        assert!(err("NSLOCTEXT(").contains("littéral n°1"));
    }

    #[test]
    fn escapes_and_raw_line_breaks_are_checked() {
        assert!(err(r#"NSLOCTEXT("", "k", "a\xb")"#).contains("\\x"));
        assert!(err(r#"NSLOCTEXT("", "k", "a\"#).contains("antislash isolé"));
        let e = err("NSLOCTEXT(\"\", \"k\", \"ligne1\nligne2\")");
        assert!(
            e.contains("retour à la ligne brut") && e.contains("\\r\\n"),
            "{e}"
        );
        err("NSLOCTEXT(\"\", \"k\", \"a\r\nb\")");
    }

    #[test]
    fn only_values_starting_with_nsloctext_are_in_scope() {
        assert!(is_nsloctext(r#"NSLOCTEXT("", "k", "t")"#));
        assert!(!is_nsloctext(r#"LOCTABLE("/Game/X/ST_X", "Key")"#));
        assert!(!is_nsloctext("Texte brut"));
        assert!(!is_nsloctext(""));
        // la casse compte: ce n'est pas la forme exportée
        assert!(!is_nsloctext(r#"nsloctext("", "k", "t")"#));
    }
}
