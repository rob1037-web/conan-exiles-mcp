//! Cœur du serveur : schémas, validation, lecture/écriture des DataTables, sandbox.
//! Aucune dépendance MCP.

pub mod config;
pub mod csv;
pub mod diff;
pub mod sandbox;
pub mod schema;
pub mod store;
pub mod validate;
pub mod value;

pub use config::{Config, ConfigError, DEFAULT_CSV_ROOT};
pub use csv::{validate_table, CsvCell, CsvError, CsvRecord, Newline, Table, WriteError};
pub use diff::{ColumnChange, DiffRow, ModifiedRow, TableDiff};
pub use sandbox::{check_relative, PathRule, Sandbox, SandboxError};
pub use schema::{Field, FieldType, Schema, SchemaError};
pub use store::{CsvStore, StoreError, WriteOptions, WriteOutcome, BACKUP_DIR};
pub use validate::{validate_row, ValidationError};
pub use value::{Cell, RawRow, Row, Value};
