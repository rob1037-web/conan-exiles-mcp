//! Cœur du serveur : schémas, validation, lecture/écriture des DataTables, sandbox.
//! Aucune dépendance MCP.

pub mod schema;

pub use schema::{Field, FieldType, Schema, SchemaError};
