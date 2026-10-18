//! Comparaison de valeurs par défaut lues dans le Devkit (Phase 4b, étape 2). Logique pure, sans E/S:
//! les deux listes d'éléments viennent du gabarit `defaults` (voir `ue_bridge::templates`).
//!
//! Règles: les flottants se comparent avec une tolérance absolue de 1e-6, les entiers exactement; un
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
                if values_equal(&x, &y) {
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
}
