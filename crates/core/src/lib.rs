//! Cœur du serveur : schémas, validation, lecture/écriture des DataTables, sandbox.
//! Aucune dépendance MCP.

pub mod schema;
pub mod validate;
pub mod value;

pub use schema::{Field, FieldType, Schema, SchemaError};
pub use validate::{validate_row, ValidationError};
pub use value::{Cell, RawRow, Row, Value};
