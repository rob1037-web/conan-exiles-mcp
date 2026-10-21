//! Encodages des CSV du Devkit à travers les outils MCP: UTF-16 LE avec BOM (export dès qu'un texte
//! n'est pas ASCII), UTF-8 avec BOM, ASCII. Fixtures synthétiques de `fixtures/encodings/`.

mod common;

use common::*;
use serde_json::json;
use std::fs;

const UTF16: &[u8] = include_bytes!("../../../fixtures/encodings/lore_utf16le_bom_crlf.csv");
const UTF8_BOM: &[u8] = include_bytes!("../../../fixtures/encodings/lore_utf8_bom_crlf.csv");
const ASCII: &[u8] = include_bytes!("../../../fixtures/encodings/lore_ascii_lf.csv");

const SCHEMA: &str = r#"
name = "DT_Lore"
description = "Table de textes (fixture d'encodage)"
key_column = "---"

[[fields]]
name = "Name"
type = "string"

[[fields]]
name = "Text"
type = "string"

[[fields]]
name = "Category"
type = "string"
"#;
const FILE: &str = "DT_Lore.csv";

async fn setup(content: &[u8]) -> (Env, Session) {
    let env = Env::new();
    fs::write(env.schema_root.join("DT_Lore.toml"), SCHEMA).unwrap();
    fs::create_dir_all(&env.csv_root).unwrap();
    fs::write(env.csv_root.join(FILE), content).unwrap();
    let s = connect(&env.config()).await;
    (env, s)
}

fn file(env: &Env) -> Vec<u8> {
    fs::read(env.csv_root.join(FILE)).unwrap()
}

fn backups(env: &Env) -> usize {
    fs::read_dir(env.csv_root.join(".backup"))
        .map(|d| d.count())
        .unwrap_or(0)
}

/// Contenu qu'enverrait Claude: texte UTF-8, fins de ligne LF.
fn client_text(bytes: &[u8]) -> String {
    let units: Vec<u16>;
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        units = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    } else {
        String::from_utf8(bytes.to_vec()).unwrap()
    };
    text.trim_start_matches('\u{feff}').replace("\r\n", "\n")
}

#[tokio::test]
async fn read_datatable_reports_the_detected_encoding_and_keeps_the_texts_intact() {
    for (bytes, id, label, bom, newline) in [
        (UTF16, "utf-16le-bom", "UTF-16 LE avec BOM", true, "CRLF"),
        (UTF8_BOM, "utf-8-bom", "UTF-8 avec BOM", true, "CRLF"),
        (ASCII, "utf-8", "UTF-8 sans BOM", false, "LF"),
    ] {
        let (_env, s) = setup(bytes).await;
        let r = s.call("read_datatable", json!({"path": FILE})).await;
        assert!(!is_error(&r), "{id}: {}", text(&r));
        let v = structured(&r);
        assert_eq!(
            v["format"],
            json!({"encoding": id, "encoding_label": label, "bom": bom, "newline": newline, "final_newline": true}),
            "{id}"
        );
        assert_eq!(v["columns"], json!(["---", "Name", "Text", "Category"]));
        assert_eq!(v["rows"][0][0], "Lore_001");
        // FText NSLOCTEXT: virgule gardée, guillemets du CSV défaits
        let name = v["rows"][0][1].as_str().unwrap();
        assert!(
            name.starts_with("NSLOCTEXT(\"\", \"C975A22C44D0E510FAE1558B3736E1CB\", \""),
            "{name}"
        );
        assert!(name.contains(", "), "{name}");
    }
    let (_env, s) = setup(UTF16).await;
    let r = s.call("read_datatable", json!({"path": FILE})).await;
    assert_eq!(
        structured(&r)["rows"][0][1],
        r#"NSLOCTEXT("", "C975A22C44D0E510FAE1558B3736E1CB", "Épée, longue")"#
    );
}

#[tokio::test]
async fn validate_rows_reads_a_utf16_table() {
    let (_env, s) = setup(UTF16).await;
    let r = s.call("validate_rows", json!({"table": "DT_Lore"})).await;
    assert!(!is_error(&r), "{}", text(&r));
}

#[tokio::test]
async fn write_datatable_dry_run_and_real_write_keep_the_utf16_format() {
    let (env, s) = setup(UTF16).await;
    let edited = client_text(UTF16).replace("Bouclier", "Bouclier rond");
    let args = |dry_run: bool| json!({"table": "DT_Lore", "content": edited, "dry_run": dry_run, "confirm": !dry_run});
    let r = s.call("write_datatable", args(true)).await;
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(
        t.contains(
            "Encodage: UTF-16 LE avec BOM, fins de ligne CRLF (conservés du fichier existant)"
        ),
        "{t}"
    );
    let v = structured(&r);
    assert_eq!(v["encoding"], "utf-16le-bom");
    assert_eq!(v["newline"], "CRLF");
    assert_eq!(v["diff"]["modified"], 1);
    assert_eq!(v["diff"]["format_changed"], false);
    assert_eq!(file(&env), UTF16, "le dry-run n'écrit rien");

    let r = s.call("write_datatable", args(false)).await;
    assert!(!is_error(&r), "{}", text(&r));
    let written = file(&env);
    assert!(written.starts_with(&[0xFF, 0xFE]));
    assert_ne!(written, UTF16);
    assert_eq!(client_text(&written), edited);
    assert!(
        written.windows(4).any(|w| w == [0x0D, 0x00, 0x0A, 0x00]),
        "CRLF UTF-16"
    );
    assert_eq!(backups(&env), 1);

    // le même contenu une seconde fois: aucun changement
    let r = s.call("write_datatable", args(false)).await;
    assert!(text(&r).starts_with("AUCUN CHANGEMENT"), "{}", text(&r));
    assert_eq!(file(&env), written);
    assert_eq!(backups(&env), 1);
}

#[tokio::test]
async fn rewriting_the_content_read_is_identical_to_the_byte_in_every_encoding() {
    for bytes in [UTF16, UTF8_BOM, ASCII] {
        let (env, s) = setup(bytes).await;
        let r = s
            .call(
                "write_datatable",
                json!({"table": "DT_Lore", "content": client_text(bytes), "dry_run": false, "confirm": true}),
            )
            .await;
        assert!(!is_error(&r), "{}", text(&r));
        assert!(text(&r).starts_with("AUCUN CHANGEMENT"), "{}", text(&r));
        assert_eq!(file(&env), bytes);
        assert_eq!(backups(&env), 0);
    }
}

#[tokio::test]
async fn row_tools_keep_the_existing_encoding_and_report_it() {
    let (env, s) = setup(UTF16).await;
    let add = json!({"table": "DT_Lore", "key": "Lore_900", "values": {
        "Name": r#"NSLOCTEXT("", "Lore_900_Name", "Ñandú, \"x\" d\'or")"#,
        "Text": "NSLOCTEXT(\"\", \"Lore_900_Text\", \"a, b\")", "Category": "Récit"}});
    let r = s.call("add_row", add.clone()).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        text(&r).contains("Encodage: UTF-16 LE avec BOM"),
        "{}",
        text(&r)
    );
    assert_eq!(file(&env), UTF16);
    let mut real = add.clone();
    real["dry_run"] = json!(false);
    real["confirm"] = json!(true);
    assert!(!is_error(&s.call("add_row", real).await));
    assert!(file(&env).starts_with(&[0xFF, 0xFE]));

    let r = s
        .call(
            "delete_row",
            json!({"table": "DT_Lore", "key": "Lore_900", "dry_run": false, "confirm": true}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(
        file(&env),
        UTF16,
        "ajout puis suppression: fichier d'origine à l'octet près"
    );
    assert_eq!(structured(&r)["encoding"], "utf-16le-bom");
}

#[tokio::test]
async fn a_new_file_is_utf8_without_bom_and_says_so() {
    let (env, s) = setup(ASCII).await;
    let r = s
        .call(
            "write_datatable",
            json!({"table": "DT_Lore", "path": "nouveau.csv", "content": client_text(UTF16),
                   "dry_run": false, "confirm": true}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        text(&r).contains("Encodage: UTF-8 sans BOM, fins de ligne LF (nouveau fichier: format par défaut du Devkit)"),
        "{}",
        text(&r)
    );
    let bytes = fs::read(env.csv_root.join("nouveau.csv")).unwrap();
    assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]) && !bytes.starts_with(&[0xFF, 0xFE]));
    assert_eq!(bytes, client_text(UTF16).as_bytes());
}

#[tokio::test]
async fn a_truncated_utf16_file_is_a_clear_tool_error_naming_the_encoding() {
    let truncated = &UTF16[..UTF16.len() - 1];
    let (env, s) = setup(truncated).await;
    // (outil, arguments, code d'erreur: celui que cet outil renvoyait déjà pour un CSV illisible)
    for (tool, args, code) in [
        ("read_datatable", json!({"path": FILE}), "InvalidCsv"),
        ("validate_rows", json!({"table": "DT_Lore"}), "InvalidCsv"),
        (
            "delete_row",
            json!({"table": "DT_Lore", "key": "Lore_001", "dry_run": false, "confirm": true}),
            "ExistingUnreadable",
        ),
    ] {
        let r = s.call(tool, args).await;
        let t = text(&r);
        assert!(
            is_error(&r)
                && t.contains("UTF-16 LE avec BOM")
                && t.contains("impaire")
                && t.starts_with(&format!("ERREUR {code}:")),
            "{tool}: {t}"
        );
    }
    assert_eq!(file(&env), truncated);
    assert_eq!(backups(&env), 0);
}

// --- E3: syntaxe NSLOCTEXT -------------------------------------------------------------

/// Le cas E3: guillemets bruts dans le texte (Unreal l'afficherait brut).
const BAD: &str = r#"NSLOCTEXT("", "Lore_900_Text", "Il dit "non" puis part")"#;
const GOOD: &str =
    r#"NSLOCTEXT("", "Lore_900_Text", "Il dit \"non\" puis part, c\'est fini.\r\nFin.")"#;

fn row_with(text: &str) -> serde_json::Value {
    json!({"table": "DT_Lore", "key": "Lore_900", "values": {
        "Name": r#"NSLOCTEXT("", "Lore_900_Name", "Nom")"#, "Text": text, "Category": "Récit"}})
}

#[tokio::test]
async fn validate_rows_flags_a_malformed_nsloctext_with_the_expected_form() {
    let (_env, s) = setup(UTF16).await;
    let rows = |text: &str| {
        json!({"table": "DT_Lore", "rows": [
            ["Lore_900", r#"NSLOCTEXT("", "k", "Nom")"#, text, "Récit"]]})
    };
    let r = s.call("validate_rows", rows(BAD)).await;
    let t = text(&r);
    assert!(t.contains("MalformedLocText"), "{t}");
    for needle in ["trois littéraux", "\\\"", "\\r\\n", "Lore_900", "'Text'"] {
        assert!(t.contains(needle), "{needle}: {t}");
    }
    let v = structured(&r);
    assert_eq!(v["valid"], false);
    assert_eq!(v["errors"][0]["rule"], "MalformedLocText");
    assert_eq!(v["errors"][0]["column"], "Text");
    assert_eq!(v["errors"][0]["value"], BAD);
    // la forme correcte et les autres formes (LOCTABLE, texte brut) passent
    for ok in [
        GOOD,
        r#"LOCTABLE("/Game/Items/Strings/ST_X", "Key")"#,
        "Texte brut",
        "",
    ] {
        let r = s.call("validate_rows", rows(ok)).await;
        assert!(
            !is_error(&r) && structured(&r)["valid"] == true,
            "{ok}: {}",
            text(&r)
        );
    }
}

#[tokio::test]
async fn add_row_with_a_malformed_nsloctext_is_reported_then_refused_and_nothing_is_written() {
    let (env, s) = setup(UTF16).await;
    let mut args = row_with(BAD);
    let r = s.call("add_row", args.clone()).await; // dry-run par défaut
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        text(&r).contains("VALIDATION ÉCHOUÉE") && text(&r).contains("MalformedLocText"),
        "{}",
        text(&r)
    );
    args["dry_run"] = json!(false);
    args["confirm"] = json!(true);
    let r = s.call("add_row", args).await;
    assert!(
        is_error(&r)
            && text(&r).starts_with("ERREUR ValidationFailed:")
            && text(&r).contains("MalformedLocText"),
        "{}",
        text(&r)
    );
    assert_eq!(file(&env), UTF16);
    assert_eq!(backups(&env), 0);

    // la forme correcte s'écrit, en UTF-16 conservé
    let mut ok = row_with(GOOD);
    ok["dry_run"] = json!(false);
    ok["confirm"] = json!(true);
    let r = s.call("add_row", ok).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(file(&env).starts_with(&[0xFF, 0xFE]));
}
