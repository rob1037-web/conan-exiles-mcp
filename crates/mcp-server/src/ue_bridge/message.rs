//! Messages du protocole « Python Remote Execution » (voir docs/ue-remote-execution.md, §2 et §3).
//!
//! Un message est un objet JSON UTF-8 : `version` (1), `magic` ("ue_py"), `type`, `source`, et
//! optionnellement `dest` et `data`. Comme le client de référence (remote_execution.py, lignes
//! 542-551), `dest` et `data` sont omis quand ils sont vides.

use serde_json::{json, Map, Value};

pub const PROTOCOL_VERSION: u64 = 1;
pub const PROTOCOL_MAGIC: &str = "ue_py";

pub const TYPE_PING: &str = "ping";
pub const TYPE_PONG: &str = "pong";
pub const TYPE_OPEN_CONNECTION: &str = "open_connection";
pub const TYPE_CLOSE_CONNECTION: &str = "close_connection";
pub const TYPE_COMMAND: &str = "command";
pub const TYPE_COMMAND_RESULT: &str = "command_result";

/// Mode d'exécution d'un script littéral de plusieurs instructions.
pub const MODE_EXEC_FILE: &str = "ExecuteFile";

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub kind: String,
    pub source: String,
    pub dest: Option<String>,
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MessageError {
    #[error("message illisible: {0}")]
    Json(String),
    #[error("le message n'est pas un objet JSON")]
    NotAnObject,
    #[error("champ {0:?} absent ou du mauvais type")]
    Field(&'static str),
    #[error("version du protocole {got} (attendu {PROTOCOL_VERSION})")]
    Version { got: String },
    #[error("magic {got:?} (attendu {PROTOCOL_MAGIC:?})")]
    Magic { got: String },
}

impl Message {
    pub fn new(kind: &str, source: &str, dest: Option<&str>, data: Option<Value>) -> Message {
        Message {
            kind: kind.to_owned(),
            source: source.to_owned(),
            dest: dest.map(str::to_owned),
            data,
        }
    }

    /// JSON UTF-8. `dest` et `data` vides sont omis (comme le client de référence).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut obj = Map::new();
        obj.insert("version".into(), json!(PROTOCOL_VERSION));
        obj.insert("magic".into(), json!(PROTOCOL_MAGIC));
        obj.insert("type".into(), json!(self.kind));
        obj.insert("source".into(), json!(self.source));
        if let Some(dest) = self.dest.as_deref().filter(|d| !d.is_empty()) {
            obj.insert("dest".into(), json!(dest));
        }
        if let Some(data) = &self.data {
            let empty = matches!(data, Value::Null)
                || data.as_object().is_some_and(Map::is_empty)
                || data.as_array().is_some_and(Vec::is_empty);
            if !empty {
                obj.insert("data".into(), data.clone());
            }
        }
        // Une `Value` se sérialise toujours; le repli vide ne sert qu'à éviter un unwrap.
        serde_json::to_vec(&Value::Object(obj)).unwrap_or_default()
    }

    /// Analyse un message complet; vérifie `version` et `magic` (lignes 574-592 du client de référence).
    pub fn from_bytes(bytes: &[u8]) -> Result<Message, MessageError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| MessageError::Json(e.to_string()))?;
        Message::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Message, MessageError> {
        let obj = value.as_object().ok_or(MessageError::NotAnObject)?;
        match obj.get("version").and_then(Value::as_u64) {
            Some(PROTOCOL_VERSION) => {}
            _ => {
                return Err(MessageError::Version {
                    got: obj.get("version").map_or("absent".into(), Value::to_string),
                })
            }
        }
        match obj.get("magic").and_then(Value::as_str) {
            Some(PROTOCOL_MAGIC) => {}
            other => {
                return Err(MessageError::Magic {
                    got: other.unwrap_or("absent").to_owned(),
                })
            }
        }
        let text = |key: &'static str| {
            obj.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or(MessageError::Field(key))
        };
        Ok(Message {
            kind: text("type")?,
            source: text("source")?,
            dest: obj
                .get("dest")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .map(str::to_owned),
            data: obj.get("data").filter(|d| !d.is_null()).cloned(),
        })
    }

    /// Ignoré s'il vient de nous, ou s'il est adressé à quelqu'un d'autre (lignes 519-529).
    pub fn passes_receive_filter(&self, node_id: &str) -> bool {
        self.source != node_id && self.dest.as_deref().is_none_or(|d| d == node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_matches_the_reference_client() {
        let ping = Message::new(TYPE_PING, "node-1", None, None);
        let v: Value = serde_json::from_slice(&ping.to_bytes()).unwrap();
        assert_eq!(
            v,
            json!({"version": 1, "magic": "ue_py", "type": "ping", "source": "node-1"})
        );

        let open = Message::new(
            TYPE_OPEN_CONNECTION,
            "node-1",
            Some("editor-9"),
            Some(json!({"command_ip": "127.0.0.1", "command_port": 4242})),
        );
        let v: Value = serde_json::from_slice(&open.to_bytes()).unwrap();
        assert_eq!(v["dest"], "editor-9");
        assert_eq!(
            v["data"],
            json!({"command_ip": "127.0.0.1", "command_port": 4242})
        );

        // data vide ou dest vide: omis
        let m = Message::new(TYPE_CLOSE_CONNECTION, "n", Some(""), Some(json!({})));
        let v: Value = serde_json::from_slice(&m.to_bytes()).unwrap();
        assert!(v.get("dest").is_none() && v.get("data").is_none());
    }

    #[test]
    fn unicode_is_kept_as_utf8_not_escaped() {
        let m = Message::new(
            TYPE_COMMAND,
            "n",
            Some("d"),
            Some(json!({"command": "print('é⚔')"})),
        );
        let bytes = m.to_bytes();
        assert!(String::from_utf8(bytes.clone()).unwrap().contains("é⚔"));
        assert_eq!(
            Message::from_bytes(&bytes).unwrap().data.unwrap()["command"],
            "print('é⚔')"
        );
    }

    #[test]
    fn round_trip() {
        let m = Message::new(
            TYPE_COMMAND,
            "a",
            Some("b"),
            Some(json!({"unattended": true})),
        );
        assert_eq!(Message::from_bytes(&m.to_bytes()).unwrap(), m);
    }

    #[test]
    fn parsing_rejects_wrong_version_magic_and_missing_fields() {
        let bad = |s: &str| Message::from_bytes(s.as_bytes()).unwrap_err();
        assert!(matches!(
            bad(r#"{"version":2,"magic":"ue_py","type":"pong","source":"x"}"#),
            MessageError::Version { .. }
        ));
        assert!(matches!(
            bad(r#"{"magic":"ue_py","type":"pong","source":"x"}"#),
            MessageError::Version { .. }
        ));
        assert!(matches!(
            bad(r#"{"version":1,"magic":"nope","type":"pong","source":"x"}"#),
            MessageError::Magic { .. }
        ));
        assert!(matches!(
            bad(r#"{"version":1,"magic":"ue_py","source":"x"}"#),
            MessageError::Field("type")
        ));
        assert!(matches!(
            bad(r#"{"version":1,"magic":"ue_py","type":"pong","source":""}"#),
            MessageError::Field("source")
        ));
        assert!(matches!(bad("[1]"), MessageError::NotAnObject));
        assert!(matches!(bad("{pas du json"), MessageError::Json(_)));
    }

    #[test]
    fn receive_filter_ignores_self_and_other_destinations() {
        let m = |source: &str, dest: Option<&str>| Message::new(TYPE_PONG, source, dest, None);
        assert!(m("editor", None).passes_receive_filter("me"));
        assert!(m("editor", Some("me")).passes_receive_filter("me"));
        assert!(!m("me", None).passes_receive_filter("me"));
        assert!(!m("editor", Some("someone-else")).passes_receive_filter("me"));
    }
}
