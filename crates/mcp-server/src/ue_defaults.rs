//! Comparaison de valeurs par défaut lues dans le Devkit (Phase 4b, étape 2). Logique pure, sans E/S:
//! les deux listes d'éléments viennent du gabarit `defaults` (voir `ue_bridge::templates`).
//!
//! Règles: une variable dont la précision est connue (`precision`: `float` ou `double`, lue sur la broche du
//! Blueprint) se compare exactement (float 32 bits arrondi pour `float`); sans précision connue
//! (composants, type inconnu), les flottants se comparent avec une tolérance absolue de 1e-6. Les entiers
//! se comparent toujours exactement; un
//! élément illisible ou non déplié (conteneur, struct) n'est jamais déclaré égal ni différent: il est
//! listé à part (`not_compared`) avec la raison.

use std::collections::BTreeMap;

use serde_json::{json, Value};

pub const FLOAT_TOLERANCE: f64 = 1e-6;

/// Égalité de deux valeurs JSON, flottants à `FLOAT_TOLERANCE` près.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if let (Some(i), Some(j)) = (x.as_i64(), y.as_i64()) {
                return i == j;
            }
            if let (Some(i), Some(j)) = (x.as_u64(), y.as_u64()) {
                return i == j;
            }
            match (x.as_f64(), y.as_f64()) {
                (Some(i), Some(j)) => (i - j).abs() <= FLOAT_TOLERANCE,
                _ => false,
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(i, j)| values_equal(i, j))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| values_equal(v, w)))
        }
        _ => a == b,
    }
}

/// Égalité exacte de deux valeurs JSON scalaires; `500` et `500.0` sont la même valeur numérique.
fn exact_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(i), Some(j)) => i == j,
            _ => x.as_f64() == y.as_f64(),
        },
        _ => a == b,
    }
}

fn has_precision(item: &Value) -> bool {
    matches!(
        item.get("precision").and_then(Value::as_str),
        Some("float" | "double")
    )
}

/// Un « non déplié » (conteneur ou struct) se trouve-t-il quelque part dans la valeur ?
fn contains_not_expanded(value: &Value) -> bool {
    match value {
        Value::Object(o) => {
            o.get("not_expanded") == Some(&Value::Bool(true))
                || o.values().any(contains_not_expanded)
        }
        Value::Array(a) => a.iter().any(contains_not_expanded),
        _ => false,
    }
}

enum Side {
    Value(Value),
    Unreadable(String),
    NotExpanded,
}

fn side_of(item: &Value) -> Side {
    if let Some(reason) = item.get("unreadable").and_then(Value::as_str) {
        return Side::Unreadable(reason.to_owned());
    }
    let value = match item.get("value") {
        Some(v) => v.clone(),
        // composants: l'élément entier (hors clé) est la valeur comparée
        None => {
            let mut rest = item.clone();
            if let Some(o) = rest.as_object_mut() {
                o.remove("name");
                o.remove("owner");
            }
            rest
        }
    };
    if contains_not_expanded(&value) {
        Side::NotExpanded
    } else {
        Side::Value(value)
    }
}

fn key_of(item: &Value) -> (String, String) {
    let text = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    (text("owner"), text("name"))
}

fn label((owner, name): &(String, String)) -> Value {
    json!({"name": name, "owner": owner})
}

fn type_of(item: Option<&Value>) -> Value {
    item.and_then(|i| i.get("type").cloned())
        .unwrap_or(Value::Null)
}

/// Compare deux listes d'éléments (clé = propriétaire + nom). Résultat compact, ordre stable.
pub fn diff_items(a: &[Value], b: &[Value]) -> Value {
    let index = |items: &[Value]| -> BTreeMap<(String, String), Value> {
        items.iter().map(|i| (key_of(i), i.clone())).collect()
    };
    let (ia, ib) = (index(a), index(b));
    let mut differences = Vec::new();
    let mut not_compared = Vec::new();
    let mut only_a = Vec::new();
    let mut only_b = Vec::new();
    let mut identical = 0usize;
    for (key, item_a) in &ia {
        let Some(item_b) = ib.get(key) else {
            only_a.push(label(key));
            continue;
        };
        match (side_of(item_a), side_of(item_b)) {
            (Side::Value(x), Side::Value(y)) => {
                let same = if has_precision(item_a) || has_precision(item_b) {
                    exact_equal(&x, &y)
                } else {
                    values_equal(&x, &y)
                };
                if same {
                    identical += 1;
                } else {
                    let mut d = label(key);
                    d["a"] = x;
                    d["b"] = y;
                    d["type_a"] = type_of(Some(item_a));
                    d["type_b"] = type_of(Some(item_b));
                    differences.push(d);
                }
            }
            (sa, sb) => {
                let why = |s: &Side, who: &str| match s {
                    Side::Unreadable(r) => Some(format!("{who} illisible ({r})")),
                    Side::NotExpanded => Some(format!("{who} non déplié (conteneur ou struct)")),
                    Side::Value(_) => None,
                };
                let reasons: Vec<String> = [why(&sa, "A"), why(&sb, "B")]
                    .into_iter()
                    .flatten()
                    .collect();
                let mut n = label(key);
                n["reason"] = json!(reasons.join("; "));
                not_compared.push(n);
            }
        }
    }
    for key in ib.keys().filter(|k| !ia.contains_key(*k)) {
        only_b.push(label(key));
    }
    json!({
        "identical": identical,
        "different": differences.len(),
        "only_in_a": only_a.len(),
        "only_in_b": only_b.len(),
        "not_compared": not_compared.len(),
        "differences": differences,
        "only_in_a_names": only_a,
        "only_in_b_names": only_b,
        "not_compared_items": not_compared,
        "float_tolerance": FLOAT_TOLERANCE,
    })
}

/// Vérifie qu'une valeur proposée a la forme attendue par le type de broche. `Err` = raison lisible.
fn check_proposed_type(category: &str, label: &str, proposed: &Value) -> Result<(), String> {
    let expect = |ok: bool, what: &str| {
        if ok {
            Ok(())
        } else {
            Err(format!(
                "type {label} attendu: {what}; reçu {}",
                json_kind(proposed)
            ))
        }
    };
    match category {
        "real" | "float" | "double" => expect(proposed.is_number(), "un nombre"),
        "int" | "int64" => expect(
            proposed.as_i64().is_some() || proposed.as_u64().is_some(),
            "un entier",
        ),
        // un octet sans énumération est un entier; avec énumération (`byte<EEnum>`), un nom
        "byte" if label.contains('<') => expect(proposed.is_string(), "un nom d'énumération"),
        "byte" => expect(
            proposed.as_u64().is_some_and(|n| n <= 255),
            "un entier de 0 à 255",
        ),
        "bool" | "boolean" => expect(proposed.is_boolean(), "un booléen"),
        "string" | "name" | "text" => expect(proposed.is_string(), "une chaîne"),
        "object" | "class" | "softobject" | "softclass" | "interface" => expect(
            proposed.is_string() || proposed.is_null(),
            "un chemin d'asset (chaîne) ou null",
        ),
        _ => Err(format!(
            "type {label}: aucune règle de compatibilité pour cette catégorie (struct, delegate...)"
        )),
    }
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "un booléen",
        Value::Number(_) => "un nombre",
        Value::String(_) => "une chaîne",
        Value::Array(_) => "un tableau",
        Value::Object(_) => "un objet",
    }
}

/// Valeur actuelle et valeur proposée désignent-elles la même chose ? Un nom d'énumération proposé sans
/// son préfixe (`Static` pour `EMobility.Static`) est accepté.
fn proposed_equals_current(current: &Value, proposed: &Value) -> bool {
    if values_equal(current, proposed) {
        return true;
    }
    match (current.as_str(), proposed.as_str()) {
        (Some(c), Some(p)) => c.rsplit_once('.').is_some_and(|(_, tail)| tail == p),
        _ => false,
    }
}

/// `Some(plus courte représentation décimale)` si arrondir `x` en f32 change les chiffres saisis: `x` n'est
/// pas exactement le float 32 bits et sa plus courte écriture (celle qui se relit en `stored`) est un autre
/// nombre. `0.3` → `None` (se relit `0.3`), `16777217` → `Some("16777216")`. Une valeur saisie qui est
/// exactement le float 32 bits (`0.30000001192092896`) n'est pas une perte.
fn float32_loss(x: f64, stored: f32) -> Option<String> {
    if x == f64::from(stored) {
        return None;
    }
    let shortest = stored.to_string();
    (shortest.parse::<f64>().ok() != Some(x)).then_some(shortest)
}

/// Compare des valeurs PROPOSÉES (`nom exact -> valeur`) aux valeurs actuelles lues dans le Devkit.
/// `items`: éléments de la section « members » lus pour ces noms; `missing`: noms introuvables dans le
/// Blueprint. Logique pure: ne décide rien, ne lit rien. Statuts par variable: `unchanged`, `modified`,
/// `type_mismatch`, `unknown_variable`, `not_comparable` (illisible, conteneur, struct non déplié...).
/// Pour un `real(float)` (champ `precision`), la valeur proposée est arrondie en f32 (`proposed_stored`) et
/// comparée exactement, avec `not_representable` + `warning` si l'arrondi change les chiffres saisis
/// (`valid` n'en tient pas compte: c'est une information). Un `real(double)` se compare exactement; sans
/// précision connue, `FLOAT_TOLERANCE` s'applique.
/// `valid` est vrai quand aucune variable n'est inconnue, de mauvais type ou non comparable: c'est le
/// dry-run d'une future écriture.
pub fn diff_proposed(
    items: &[Value],
    missing: &[String],
    proposed: &serde_json::Map<String, Value>,
) -> Value {
    let mut changes: Vec<Value> = Vec::new();
    let mut not_representable = 0usize;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut push = |status: &'static str, mut entry: Value| {
        entry["status"] = json!(status);
        *counts.entry(status).or_default() += 1;
        changes.push(entry);
    };
    for (name, wanted) in proposed {
        let matching: Vec<&Value> = items
            .iter()
            .filter(|i| i.get("name").and_then(Value::as_str) == Some(name.as_str()))
            .collect();
        if matching.is_empty() || missing.contains(name) {
            push(
                "unknown_variable",
                json!({"name": name, "proposed": wanted,
                       "reason": "aucune variable de ce nom exact dans le Blueprint (casse et espaces comptent)"}),
            );
            continue;
        }
        if matching.len() > 1 {
            push(
                "not_comparable",
                json!({"name": name, "proposed": wanted,
                       "reason": format!("{} variables portent ce nom (propriétaires différents)", matching.len())}),
            );
            continue;
        }
        let item = matching[0];
        let label = item.get("type").and_then(Value::as_str).unwrap_or("");
        let mut entry = json!({"name": name, "type": label, "proposed": wanted});
        if let Some(p) = item.get("precision") {
            entry["precision"] = p.clone();
        }
        let current = match side_of(item) {
            Side::Value(v) => v,
            Side::Unreadable(why) => {
                entry["reason"] = json!(format!("valeur actuelle illisible ({why})"));
                push("not_comparable", entry);
                continue;
            }
            Side::NotExpanded => {
                entry["reason"] =
                    json!("valeur actuelle de type conteneur ou struct: non comparée");
                push("not_comparable", entry);
                continue;
            }
        };
        entry["current"] = current.clone();
        let container = ["Array of ", "Set of ", "Map of "]
            .iter()
            .any(|p| label.starts_with(p));
        if container || label.is_empty() {
            entry["reason"] = json!("conteneur ou type inconnu: non comparé");
            push("not_comparable", entry);
            continue;
        }
        let category = item
            .get("pin_category")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| label.split(['(', '<']).next().unwrap_or("").to_owned());
        match check_proposed_type(&category, label, wanted) {
            Err(reason) if reason.contains("aucune règle") => {
                entry["reason"] = json!(reason);
                push("not_comparable", entry);
            }
            Err(reason) => {
                entry["reason"] = json!(reason);
                push("type_mismatch", entry);
            }
            Ok(()) => {
                // La précision (lue sur la broche) décide: real(float) = valeur proposée arrondie en f32 puis
                // comparée exactement (avec avertissement si l'arrondi change les chiffres saisis);
                // real(double) = comparaison exacte; précision inconnue = tolérance de FLOAT_TOLERANCE.
                let equal = match (
                    item.get("precision").and_then(Value::as_str),
                    wanted.as_f64(),
                ) {
                    (Some("float"), Some(x)) => {
                        let stored = x as f32;
                        if !stored.is_finite() {
                            entry["reason"] =
                                json!("hors de la plage d'un float 32 bits (real(float))");
                            push("type_mismatch", entry);
                            continue;
                        }
                        entry["proposed_stored"] = json!(f64::from(stored));
                        if let Some(shortest) = float32_loss(x, stored) {
                            entry["not_representable"] = json!(true);
                            entry["shortest_stored"] = json!(shortest);
                            entry["warning"] = json!(format!(
                                "{wanted} n'est pas représentable en float 32 bits: la valeur stockée serait {shortest}"
                            ));
                            not_representable += 1;
                        }
                        current.as_f64().is_some_and(|c| (c as f32) == stored)
                    }
                    (Some("double"), Some(x)) => current.as_f64().is_some_and(|c| c == x),
                    _ => proposed_equals_current(&current, wanted),
                };
                push(if equal { "unchanged" } else { "modified" }, entry);
            }
        }
    }
    let count = |k: &str| counts.get(k).copied().unwrap_or(0);
    let valid = count("type_mismatch") + count("unknown_variable") + count("not_comparable") == 0;
    json!({
        "mode": "proposed",
        "read_only": true,
        "valid": valid,
        "would_change": count("modified") > 0,
        "proposed_count": proposed.len(),
        "modified": count("modified"),
        "unchanged": count("unchanged"),
        "type_mismatch": count("type_mismatch"),
        "unknown_variable": count("unknown_variable"),
        "not_comparable": count("not_comparable"),
        "not_representable": not_representable,
        "changes": changes,
        "float_tolerance": FLOAT_TOLERANCE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, value: Value) -> Value {
        json!({"name": name, "owner": "/Script/X.Y", "type": "int", "value": value})
    }

    #[test]
    fn floats_use_a_tolerance_integers_are_exact() {
        assert!(values_equal(&json!(500.0), &json!(500.0000004)));
        assert!(!values_equal(&json!(500.0), &json!(500.00001)));
        assert!(values_equal(&json!(1000), &json!(1000)));
        assert!(!values_equal(&json!(1000), &json!(1001)));
        // grands entiers: pas de perte via f64
        assert!(!values_equal(
            &json!(9007199254740993_i64),
            &json!(9007199254740992_i64)
        ));
        assert!(values_equal(
            &json!({"v": [1.0, 2.0]}),
            &json!({"v": [1.0000001, 2.0]})
        ));
        assert!(!values_equal(&json!("a"), &json!("A")));
        assert!(!values_equal(&json!(true), &json!(1)));
    }

    #[test]
    fn diff_lists_differences_only_in_each_side_and_what_was_not_compared() {
        let a = vec![
            member("MaxHealth", json!(1000)),
            member("Same", json!(true)),
            member("Range", json!(500.0)),
            member("OnlyA", json!(1)),
            member(
                "Big",
                json!({"not_expanded": true, "kind": "Array", "length": 3}),
            ),
            json!({"name": "Locked", "owner": "/Script/X.Y", "unreadable": "Exception: non"}),
        ];
        let b = vec![
            member("MaxHealth", json!(5000)),
            member("Same", json!(true)),
            member("Range", json!(500.0000001)),
            member("OnlyB", json!(2)),
            member(
                "Big",
                json!({"not_expanded": true, "kind": "Array", "length": 3}),
            ),
            member("Locked", json!(7)),
        ];
        let d = diff_items(&a, &b);
        assert_eq!(d["identical"], 2);
        assert_eq!(d["different"], 1);
        assert_eq!(d["differences"][0]["name"], "MaxHealth");
        assert_eq!(
            (
                d["differences"][0]["a"].clone(),
                d["differences"][0]["b"].clone()
            ),
            (json!(1000), json!(5000))
        );
        assert_eq!(
            d["only_in_a_names"],
            json!([{"name": "OnlyA", "owner": "/Script/X.Y"}])
        );
        assert_eq!(
            d["only_in_b_names"],
            json!([{"name": "OnlyB", "owner": "/Script/X.Y"}])
        );
        assert_eq!(d["not_compared"], 2);
        let reasons: Vec<String> = d["not_compared_items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["reason"].as_str().unwrap().to_owned())
            .collect();
        assert!(reasons.iter().any(|r| r.contains("A non déplié")));
        assert!(reasons.iter().any(|r| r.starts_with("A illisible")));
    }

    #[test]
    fn nested_not_expanded_elements_are_not_compared_and_components_compare_whole_items() {
        let nested = json!({"kind": "Array", "length": 1, "items": [{"not_expanded": true, "kind": "Map", "length": 2}], "more": false});
        let d = diff_items(&[member("N", nested.clone())], &[member("N", nested)]);
        assert_eq!(
            (d["identical"].clone(), d["not_compared"].clone()),
            (json!(0), json!(1))
        );

        let comp = |class: &str| json!({"name": "StaticMesh", "owner": "", "is_inherited": true, "class": class});
        let d = diff_items(
            &[comp("StaticMeshComponent")],
            &[comp("SkeletalMeshComponent")],
        );
        assert_eq!(d["different"], 1);
        assert_eq!(d["differences"][0]["a"]["class"], "StaticMeshComponent");
        let d = diff_items(&[comp("A")], &[comp("A")]);
        assert_eq!(d["identical"], 1);
    }

    fn typed(name: &str, label: &str, category: &str, value: Value) -> Value {
        json!({"name": name, "owner": "", "type": label, "pin_category": category, "value": value})
    }

    #[test]
    fn proposed_values_are_classified_per_variable() {
        let items = vec![
            typed("Range", "real(float)", "real", json!(0.30000001192092896)),
            typed("Count", "int", "int", json!(3)),
            typed("Enabled", "bool", "bool", json!(true)),
            typed(
                "Item Data Table",
                "object<DataTable>",
                "object",
                json!("/Game/DT_A.DT_A"),
            ),
            typed("Mode", "byte<EMobility>", "byte", json!("EMobility.Static")),
            typed(
                "Tags",
                "Array of name",
                "name",
                json!({"not_expanded": true, "kind": "Array", "length": 2}),
            ),
            json!({"name": "Locked", "owner": "", "unreadable": "Exception: non"}),
        ];
        let proposed = json!({
            "Range": 0.3,             // même flottant 32 bits: inchangé
            "Count": 4,               // modifié
            "Enabled": "oui",         // mauvais type
            "Item Data Table": "/Game/DT_B.DT_B", // modifié (nom avec espace, adressage exact)
            "Mode": "Static",         // énumération sans préfixe: inchangé
            "Tags": ["a"],            // conteneur: non comparé
            "Locked": 1,              // illisible: non comparé
            "item data table": "x",   // casse différente: inconnu
        });
        let r = diff_proposed(
            &items,
            &["item data table".to_owned()],
            proposed.as_object().unwrap(),
        );
        let status = |n: &str| {
            r["changes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == n)
                .unwrap()["status"]
                .clone()
        };
        assert_eq!(status("Range"), "unchanged");
        assert_eq!(status("Count"), "modified");
        assert_eq!(status("Enabled"), "type_mismatch");
        assert_eq!(status("Item Data Table"), "modified");
        assert_eq!(status("Mode"), "unchanged");
        assert_eq!(status("Tags"), "not_comparable");
        assert_eq!(status("Locked"), "not_comparable");
        assert_eq!(status("item data table"), "unknown_variable");
        assert_eq!(
            (
                r["modified"].clone(),
                r["unchanged"].clone(),
                r["type_mismatch"].clone()
            ),
            (json!(2), json!(2), json!(1))
        );
        assert_eq!(r["valid"], false);
        assert_eq!(r["would_change"], true);
        assert_eq!(r["read_only"], true);
    }

    #[test]
    fn a_fully_compatible_proposal_is_valid_and_integers_reject_fractions() {
        let items = vec![
            typed("Count", "int", "int", json!(3)),
            typed("Speed", "real(double)", "real", json!(500.0)),
        ];
        let ok = diff_proposed(
            &items,
            &[],
            json!({"Count": 3, "Speed": 600}).as_object().unwrap(),
        );
        assert_eq!(
            (
                ok["valid"].clone(),
                ok["modified"].clone(),
                ok["unchanged"].clone()
            ),
            (json!(true), json!(1), json!(1))
        );
        let bad = diff_proposed(&items, &[], json!({"Count": 3.5}).as_object().unwrap());
        assert_eq!(bad["changes"][0]["status"], "type_mismatch");
        assert!(bad["changes"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("un entier"));
        assert_eq!(bad["valid"], false);
    }

    #[test]
    fn a_float32_proposal_is_rounded_to_f32_and_compared_exactly() {
        let single = |v: f64| {
            let mut i = typed("Range", "real(float)", "real", json!(v));
            i["precision"] = json!("float");
            i
        };
        let status = |current: f64, proposed: Value| {
            let r = diff_proposed(
                &[single(current)],
                &[],
                json!({"Range": proposed}).as_object().unwrap(),
            );
            r["changes"][0].clone()
        };
        // 0.3 proposé, 0.30000001192092896 lu: même float 32 bits
        let same = status(0.30000001192092896, json!(0.3));
        assert_eq!(same["status"], "unchanged");
        assert_eq!(same["proposed_stored"], json!(0.30000001192092896));
        // 0.3000001 serait « inchangé » avec l'ancienne tolérance de 1e-6; en f32 c'est une vraie modification
        assert_eq!(
            status(0.30000001192092896, json!(0.3000001))["status"],
            "modified"
        );
        // au-delà de la précision f32 (2^24), l'arrondi rend l'entier voisin identique
        assert_eq!(status(16777216.0, json!(16777217))["status"], "unchanged");
        // un entier JSON est accepté pour un real
        assert_eq!(status(500.0, json!(500))["status"], "unchanged");
        // hors plage f32: refus explicite, pas d'infini silencieux
        let big = status(1.0, json!(1e39));
        assert_eq!(big["status"], "type_mismatch");
        assert!(big["reason"].as_str().unwrap().contains("float 32 bits"));
    }

    #[test]
    fn a_double_proposal_is_compared_exactly() {
        let mut item = typed("Speed", "real(double)", "real", json!(500.0));
        item["precision"] = json!("double");
        let status = |v: Value| {
            diff_proposed(
                &[item.clone()],
                &[],
                json!({"Speed": v}).as_object().unwrap(),
            )["changes"][0]
                .clone()
        };
        assert_eq!(status(json!(500)).get("status"), Some(&json!("unchanged")));
        assert_eq!(
            status(json!(500.0)).get("status"),
            Some(&json!("unchanged"))
        );
        // l'ancienne tolérance de 1e-6 aurait répondu « inchangé »
        let near = status(json!(500.0000004));
        assert_eq!(near["status"], "modified");
        assert!(near.get("proposed_stored").is_none() && near.get("warning").is_none());
    }

    #[test]
    fn without_a_known_precision_the_absolute_tolerance_still_applies() {
        let item = typed("Speed", "real(double)", "real", json!(500.0)); // pas de champ precision
        let r = diff_proposed(
            &[item],
            &[],
            json!({"Speed": 500.0000004}).as_object().unwrap(),
        );
        assert_eq!(r["changes"][0]["status"], "unchanged");
    }

    #[test]
    fn a_float32_proposal_warns_only_when_rounding_changes_the_digits_typed() {
        let mut item = typed("Range", "real(float)", "real", json!(0.30000001192092896));
        item["precision"] = json!("float");
        let run = |v: Value| {
            diff_proposed(
                &[item.clone()],
                &[],
                json!({"Range": v}).as_object().unwrap(),
            )
        };
        // 0.3 se relit 0.3 en float 32 bits: aucune alerte
        let ok = run(json!(0.3));
        assert!(ok["changes"][0].get("warning").is_none(), "{ok}");
        assert_eq!(ok["not_representable"], 0);
        // 0.3000001 aussi (sa plus courte écriture f32 est elle-même)
        assert!(run(json!(0.3000001))["changes"][0].get("warning").is_none());
        // la valeur exacte lue (copiée depuis ue_get_defaults) n'est pas une perte
        assert!(run(json!(0.30000001192092896))["changes"][0]
            .get("warning")
            .is_none());
        // 16777217 devient 16777216: alerte
        let lossy = run(json!(16777217));
        let c = &lossy["changes"][0];
        assert_eq!(c["not_representable"], true);
        assert_eq!(c["shortest_stored"], "16777216");
        assert_eq!(c["proposed_stored"], json!(16777216.0));
        assert!(c["warning"].as_str().unwrap().contains("16777216"));
        assert_eq!(lossy["not_representable"], 1);
        // 0.123456789 perd des chiffres: alerte avec la valeur réellement stockée
        let digits = run(json!(0.123456789));
        assert_eq!(digits["changes"][0]["not_representable"], true);
        assert_eq!(digits["changes"][0]["shortest_stored"], "0.12345679");
        // informatif: ne rend pas la proposition invalide
        assert_eq!(lossy["valid"], true);
    }

    #[test]
    fn two_blueprint_diff_is_exact_when_precision_is_known() {
        let mut a = typed("Speed", "real(double)", "real", json!(500.0));
        a["precision"] = json!("double");
        let mut b = a.clone();
        b["value"] = json!(500.0000004);
        assert_eq!(diff_items(&[a], &[b])["different"], 1);
        // sans précision: tolérance
        let c = typed("Scale", "real(double)", "real", json!(1.0));
        let mut d = c.clone();
        d["value"] = json!(1.0000004);
        assert_eq!(diff_items(&[c], &[d])["identical"], 1);
    }
}
