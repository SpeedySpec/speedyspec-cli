//! speedyspec's MySQL client: what `app db` and backups do to an app's database. MySQL and
//! MariaDB, through the `mysql` crate (pure Rust, so the binary stays static).
//!
//! Errors name the host, port, database and what failed, never the password.

pub mod split;

use std::fmt;
use std::io::{BufRead, Write};

use mysql::consts::ColumnType;
use mysql::prelude::Queryable;
use mysql::{Column, Conn, OptsBuilder, Value};
use speedyspec_driver::DatabaseConnection;

use crate::split::Statements;

/// An `INSERT` of an export grows to about this many bytes before the next one starts.
const INSERT_BYTES: usize = 1024 * 1024;
/// MySQL's character set number for binary data.
const BINARY_CHARSET: u16 = 63;

/// Why a database operation failed, as one line for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbError(pub String);

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DbError {}

/// Why an import stopped. Statements before it were applied: MySQL commits DDL at once, so an
/// import cannot be undone as a whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportError {
    /// The line of the dump the failing statement starts on.
    pub line: usize,
    /// 1-based.
    pub statement: u64,
    pub applied: u64,
    pub message: String,
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {} (statement {}): {}; the {} statement(s) before it were applied",
            self.line, self.statement, self.message, self.applied
        )
    }
}

impl std::error::Error for ImportError {}

/// A table and how big it is, as the server estimates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    pub rows_estimate: u64,
    pub data_bytes: u64,
    pub index_bytes: u64,
}

/// What a query did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryOutcome {
    /// It returned this many rows, which were written out.
    Rows(u64),
    /// It changed this many rows.
    Affected(u64),
}

/// What an export includes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportOptions {
    /// Only these tables; all of the connection's tables (within its prefix) when `None`.
    pub tables: Option<Vec<String>>,
}

/// `information_schema.TABLES`: name, rows, data bytes, index bytes.
type TableRow = (String, Option<u64>, Option<u64>, Option<u64>);

/// An open connection to an app's database.
pub struct Connection {
    conn: Conn,
    database: String,
    table_prefix: String,
}

fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// `text` as a MySQL string literal, escaped as `mysql_real_escape_string` does.
fn quote_bytes(text: &[u8], out: &mut Vec<u8>) {
    out.push(b'\'');
    for &b in text {
        match b {
            0 => out.extend_from_slice(b"\\0"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\'' => out.extend_from_slice(b"\\'"),
            b'"' => out.extend_from_slice(b"\\\""),
            0x1a => out.extend_from_slice(b"\\Z"),
            _ => out.push(b),
        }
    }
    out.push(b'\'');
}

fn hex(bytes: &[u8], out: &mut Vec<u8>) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    out.extend_from_slice(b"X'");
    for &b in bytes {
        out.push(DIGITS[usize::from(b >> 4)]);
        out.push(DIGITS[usize::from(b & 0x0f)]);
    }
    out.push(b'\'');
}

fn is_numeric(column: &Column) -> bool {
    matches!(
        column.column_type(),
        ColumnType::MYSQL_TYPE_TINY
            | ColumnType::MYSQL_TYPE_SHORT
            | ColumnType::MYSQL_TYPE_LONG
            | ColumnType::MYSQL_TYPE_INT24
            | ColumnType::MYSQL_TYPE_LONGLONG
            | ColumnType::MYSQL_TYPE_FLOAT
            | ColumnType::MYSQL_TYPE_DOUBLE
            | ColumnType::MYSQL_TYPE_DECIMAL
            | ColumnType::MYSQL_TYPE_NEWDECIMAL
            | ColumnType::MYSQL_TYPE_YEAR
    )
}

/// Whether a column holds raw bytes rather than text: a BLOB, BINARY or VARBINARY (the
/// binary character set on a string type), a BIT or a GEOMETRY. Dates, times and JSON also
/// arrive with the binary character set but are text.
fn is_binary(column: &Column) -> bool {
    match column.column_type() {
        ColumnType::MYSQL_TYPE_BIT | ColumnType::MYSQL_TYPE_GEOMETRY => true,
        ColumnType::MYSQL_TYPE_TINY_BLOB
        | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
        | ColumnType::MYSQL_TYPE_LONG_BLOB
        | ColumnType::MYSQL_TYPE_BLOB
        | ColumnType::MYSQL_TYPE_STRING
        | ColumnType::MYSQL_TYPE_VAR_STRING
        | ColumnType::MYSQL_TYPE_VARCHAR => column.character_set() == BINARY_CHARSET,
        _ => false,
    }
}

/// The text form of a value from the text protocol, which sends everything but NULL as bytes.
fn value_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::NULL => None,
        Value::Bytes(bytes) => Some(bytes.clone()),
        other => Some(other.as_sql(true).into_bytes()),
    }
}

/// A value as SQL for an `INSERT`.
fn sql_value(value: &Value, column: &Column, out: &mut Vec<u8>) {
    match value_bytes(value) {
        None => out.extend_from_slice(b"NULL"),
        Some(bytes) if is_binary(column) => hex(&bytes, out),
        Some(bytes) if is_numeric(column) => out.extend_from_slice(&bytes),
        Some(bytes) => quote_bytes(&bytes, out),
    }
}

/// A value as a tab-separated field: `NULL`, `0x…` for bytes, otherwise the text with tabs,
/// newlines and backslashes escaped.
fn tsv_value(value: &Value, column: &Column, out: &mut Vec<u8>) {
    match value_bytes(value) {
        None => out.extend_from_slice(b"NULL"),
        Some(bytes) if is_binary(column) => {
            out.extend_from_slice(b"0x");
            for b in bytes {
                out.extend_from_slice(format!("{b:02x}").as_bytes());
            }
        }
        Some(bytes) => {
            for b in bytes {
                match b {
                    b'\t' => out.extend_from_slice(b"\\t"),
                    b'\n' => out.extend_from_slice(b"\\n"),
                    b'\\' => out.extend_from_slice(b"\\\\"),
                    _ => out.push(b),
                }
            }
        }
    }
}

/// The first words of a statement, upper-cased. A leading conditional comment (`/*!40000 …*/`,
/// `/*M!100101 …*/`), which the server runs as part of the statement, is looked inside, and a
/// backquote ends a word as whitespace does (`USE`shop``).
fn first_words(statement: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(&statement[..statement.len().min(128)]).to_ascii_uppercase();
    let mut rest = text.trim_start();
    for opener in ["/*M!", "/*!"] {
        if let Some(inner) = rest.strip_prefix(opener) {
            rest = inner.trim_start_matches(|c: char| c.is_ascii_digit());
            break;
        }
    }
    rest.split(|c: char| c.is_ascii_whitespace() || c == '`')
        .filter(|word| !word.is_empty())
        .take(2)
        .map(str::to_owned)
        .collect()
}

/// Whether a statement leaves the database the import was asked to fill.
fn switches_database(statement: &[u8]) -> bool {
    let words = first_words(statement);
    let word = |i: usize| words.get(i).map(String::as_str);
    match word(0) {
        Some("USE") => true,
        Some("CREATE" | "DROP") => matches!(word(1), Some("DATABASE" | "SCHEMA")),
        _ => false,
    }
}

/// A statement as the text the server is sent. A quoted string that is not valid UTF-8 (raw
/// binary in a dump) is rewritten as a hex literal of the same bytes.
pub fn statement_text(statement: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(statement) {
        return text.to_owned();
    }
    let mut out = Vec::with_capacity(statement.len());
    let mut i = 0;
    while i < statement.len() {
        let b = statement[i];
        if b == b'\'' || b == b'"' {
            // Decode the literal; keep it as written if it is valid UTF-8.
            let start = i;
            let mut decoded = Vec::new();
            i += 1;
            while i < statement.len() {
                let c = statement[i];
                if c == b'\\' && i + 1 < statement.len() {
                    let e = statement[i + 1];
                    decoded.push(match e {
                        b'0' => 0,
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'b' => 0x08,
                        b'Z' => 0x1a,
                        _ => e,
                    });
                    i += 2;
                } else if c == b && statement.get(i + 1) == Some(&b) {
                    decoded.push(b);
                    i += 2;
                } else if c == b {
                    i += 1;
                    break;
                } else {
                    decoded.push(c);
                    i += 1;
                }
            }
            if std::str::from_utf8(&decoded).is_ok() {
                out.extend_from_slice(&statement[start..i]);
            } else {
                hex(&decoded, &mut out);
            }
        } else if b == b'`' {
            let start = i;
            i += 1;
            while i < statement.len() {
                if statement[i] == b'`' && statement.get(i + 1) == Some(&b'`') {
                    i += 2;
                } else if statement[i] == b'`' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            out.extend_from_slice(&statement[start..i]);
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a connection stays on this machine (a socket or a loopback address), where it being
/// unencrypted does not matter. The client does not use TLS yet.
pub fn stays_on_this_machine(settings: &DatabaseConnection) -> bool {
    if settings.socket.is_some() {
        return true;
    }
    let host = settings.host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

impl Connection {
    /// Connects and sets the connection's character set.
    pub fn open(settings: &DatabaseConnection) -> Result<Connection, DbError> {
        let mut opts = OptsBuilder::new()
            .user(Some(settings.user.clone()))
            .pass(Some(settings.password.clone()))
            .db_name(Some(settings.database.clone()))
            .prefer_socket(false);
        let place = match &settings.socket {
            Some(socket) => {
                opts = opts.socket(Some(socket.clone()));
                socket.clone()
            }
            None => {
                opts = opts
                    .ip_or_hostname(Some(settings.host.clone()))
                    .tcp_port(settings.port);
                format!("{}:{}", settings.host, settings.port)
            }
        };
        let mut conn = Conn::new(opts).map_err(|error| match &error {
            mysql::Error::MySqlError(e) if e.code == 1045 || e.code == 1044 => DbError(format!(
                "access denied for user {}@{} to database {}",
                settings.user, place, settings.database
            )),
            mysql::Error::MySqlError(e) if e.code == 1049 => DbError(format!(
                "database {} does not exist on {place}",
                settings.database
            )),
            mysql::Error::MySqlError(e) => DbError(format!("{place}: {}", e.message)),
            other => DbError(format!("could not connect to {place}: {other}")),
        })?;
        conn.query_drop(format!("SET NAMES {}", settings.charset))
            .map_err(|e| DbError(format!("{place}: could not set the character set: {e}")))?;
        Ok(Connection {
            conn,
            database: settings.database.clone(),
            table_prefix: settings.table_prefix.clone(),
        })
    }

    fn failed(&self, what: &str, error: mysql::Error) -> DbError {
        let detail = match error {
            mysql::Error::MySqlError(e) => e.message,
            other => other.to_string(),
        };
        DbError(format!("database {}: {what}: {detail}", self.database))
    }

    /// The base tables whose names start with the connection's table prefix, by name.
    pub fn tables(&mut self) -> Result<Vec<TableInfo>, DbError> {
        let like = format!(
            "{}%",
            self.table_prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows: Vec<TableRow> = self
            .conn
            .exec(
                "SELECT TABLE_NAME, TABLE_ROWS, DATA_LENGTH, INDEX_LENGTH FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE' AND TABLE_NAME LIKE ? \
                 ORDER BY TABLE_NAME",
                (self.database.clone(), like),
            )
            .map_err(|e| self.failed("could not list the tables", e))?;
        Ok(rows
            .into_iter()
            .map(|(name, rows, data, index)| TableInfo {
                name,
                rows_estimate: rows.unwrap_or(0),
                data_bytes: data.unwrap_or(0),
                index_bytes: index.unwrap_or(0),
            })
            .collect())
    }

    /// The data and index bytes of the connection's tables.
    pub fn size(&mut self) -> Result<(u64, u64), DbError> {
        Ok(self.tables()?.iter().fold((0, 0), |(data, index), t| {
            (data + t.data_bytes, index + t.index_bytes)
        }))
    }

    /// `CHECK TABLE` for each table: `(table, status, message)`, the status being the server's
    /// last word on it (`OK` when it is fine).
    pub fn check(&mut self) -> Result<Vec<(String, String, String)>, DbError> {
        let mut checked = Vec::new();
        for table in self.tables()? {
            let rows: Vec<(String, String, String, String)> = self
                .conn
                .query(format!("CHECK TABLE {}", quote_identifier(&table.name)))
                .map_err(|e| self.failed("could not check the tables", e))?;
            let status = rows
                .iter()
                .rev()
                .find(|row| row.2 == "status")
                .map_or_else(|| "unknown".to_owned(), |row| row.3.clone());
            let message = rows
                .iter()
                .filter(|row| row.2 != "status")
                .map(|row| format!("{}: {}", row.2, row.3))
                .collect::<Vec<_>>()
                .join("; ");
            checked.push((table.name, status, message));
        }
        Ok(checked)
    }

    /// Runs one statement. Rows go to `out` tab-separated, with a header line.
    pub fn query(&mut self, sql: &str, out: &mut dyn Write) -> Result<QueryOutcome, DbError> {
        let write_failed = |e: std::io::Error| DbError(format!("could not write the result: {e}"));
        let database = self.database.clone();
        let mut result = self
            .conn
            .query_iter(sql)
            .map_err(|e| DbError(format!("database {database}: {}", mysql_message(e))))?;
        let mut outcome = QueryOutcome::Affected(0);
        while let Some(set) = result.iter() {
            let columns: Vec<Column> = set.columns().as_ref().to_vec();
            if columns.is_empty() {
                outcome = QueryOutcome::Affected(set.affected_rows());
                continue;
            }
            let header: Vec<String> = columns.iter().map(|c| c.name_str().into_owned()).collect();
            writeln!(out, "{}", header.join("\t")).map_err(write_failed)?;
            let mut count = 0;
            for row in set {
                let row =
                    row.map_err(|e| DbError(format!("database {database}: {}", mysql_message(e))))?;
                let mut line = Vec::new();
                for (i, value) in row.unwrap().iter().enumerate() {
                    if i > 0 {
                        line.push(b'\t');
                    }
                    tsv_value(value, &columns[i], &mut line);
                }
                line.push(b'\n');
                out.write_all(&line).map_err(write_failed)?;
                count += 1;
            }
            outcome = QueryOutcome::Rows(count);
        }
        Ok(outcome)
    }

    /// Writes a dump of the tables: each dropped, created and filled, rows streamed from the
    /// server so no table is held in memory.
    pub fn export(&mut self, out: &mut dyn Write, options: &ExportOptions) -> Result<(), DbError> {
        let write_failed = |e: std::io::Error| DbError(format!("could not write the dump: {e}"));
        let tables = match &options.tables {
            Some(tables) => tables.clone(),
            None => self.tables()?.into_iter().map(|t| t.name).collect(),
        };
        // The dump says utf8mb4, so read it in utf8mb4 whatever the app's own connection uses: a
        // utf8 (utf8mb3) or latin1 connection would turn four-byte characters into '?'.
        self.conn
            .query_drop("SET NAMES utf8mb4")
            .map_err(|e| self.failed("could not set the character set", e))?;
        let server: Option<String> = self
            .conn
            .query_first("SELECT VERSION()")
            .map_err(|e| self.failed("could not read the server version", e))?;
        writeln!(
            out,
            "-- speedyspec {} dump of database {}\n-- Server version {}\n\n\
             SET NAMES utf8mb4;\nSET FOREIGN_KEY_CHECKS=0;\nSET UNIQUE_CHECKS=0;\n\
             SET SQL_MODE='NO_AUTO_VALUE_ON_ZERO';\n",
            env!("CARGO_PKG_VERSION"),
            self.database,
            server.unwrap_or_default()
        )
        .map_err(write_failed)?;
        for table in tables {
            let quoted = quote_identifier(&table);
            let create: Option<(String, String)> = self
                .conn
                .query_first(format!("SHOW CREATE TABLE {quoted}"))
                .map_err(|e| self.failed(&format!("could not read table {table}"), e))?;
            let (_, create) = create.ok_or_else(|| DbError(format!("table {table} vanished")))?;
            writeln!(out, "DROP TABLE IF EXISTS {quoted};\n{create};\n").map_err(write_failed)?;

            // Generated columns are computed by the server and refuse a value on insert.
            let stored: Vec<String> = self
                .conn
                .exec(
                    "SELECT COLUMN_NAME FROM information_schema.COLUMNS \
                     WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                     AND EXTRA NOT REGEXP '(VIRTUAL|STORED|PERSISTENT) GENERATED' \
                     ORDER BY ORDINAL_POSITION",
                    (self.database.clone(), table.clone()),
                )
                .map_err(|e| self.failed(&format!("could not read the columns of {table}"), e))?;
            let column_list = stored
                .iter()
                .map(|c| quote_identifier(c))
                .collect::<Vec<_>>()
                .join(",");
            let database = self.database.clone();
            let mut result = self
                .conn
                .query_iter(format!("SELECT {column_list} FROM {quoted}"))
                .map_err(|e| {
                    DbError(format!(
                        "database {database}: table {table}: {}",
                        mysql_message(e)
                    ))
                })?;
            let Some(set) = result.iter() else {
                continue;
            };
            let columns: Vec<Column> = set.columns().as_ref().to_vec();
            let mut insert: Vec<u8> = Vec::new();
            for row in set {
                let row = row.map_err(|e| {
                    DbError(format!(
                        "database {database}: table {table}: {}",
                        mysql_message(e)
                    ))
                })?;
                if insert.is_empty() {
                    insert.extend_from_slice(
                        format!("INSERT INTO {quoted} ({column_list}) VALUES\n(").as_bytes(),
                    );
                } else {
                    insert.extend_from_slice(b",\n(");
                }
                for (i, value) in row.unwrap().iter().enumerate() {
                    if i > 0 {
                        insert.push(b',');
                    }
                    sql_value(value, &columns[i], &mut insert);
                }
                insert.push(b')');
                if insert.len() >= INSERT_BYTES {
                    insert.extend_from_slice(b";\n");
                    out.write_all(&insert).map_err(write_failed)?;
                    insert.clear();
                }
            }
            if !insert.is_empty() {
                insert.extend_from_slice(b";\n");
                out.write_all(&insert).map_err(write_failed)?;
            }
            writeln!(out).map_err(write_failed)?;
        }
        writeln!(out, "SET FOREIGN_KEY_CHECKS=1;\nSET UNIQUE_CHECKS=1;").map_err(write_failed)?;
        out.flush().map_err(write_failed)
    }

    /// Runs every statement of a dump, in order, and says how many ran. It stops at the first
    /// that fails, or that would switch to another database.
    pub fn import(&mut self, input: impl BufRead) -> Result<u64, ImportError> {
        self.import_rewriting(input, &mut |statement| statement.to_vec())
    }

    /// As [`Connection::import`], each statement passed through `rewrite` before it runs (a restore moving the
    /// site to a new prefix and URL). The checks apply to the rewritten statement.
    pub fn import_rewriting(
        &mut self,
        input: impl BufRead,
        rewrite: &mut dyn FnMut(&[u8]) -> Vec<u8>,
    ) -> Result<u64, ImportError> {
        let mut applied = 0u64;
        for item in Statements::new(input) {
            let (line, statement) = item.map_err(|e| ImportError {
                line: e.line,
                statement: applied + 1,
                applied,
                message: e.message,
            })?;
            let statement = rewrite(&statement);
            let number = applied + 1;
            if switches_database(&statement) {
                return Err(ImportError {
                    line,
                    statement: number,
                    applied,
                    message: format!(
                        "the dump switches to another database ({}); import only dumps of tables",
                        String::from_utf8_lossy(&statement[..statement.len().min(40)])
                    ),
                });
            }
            self.conn
                .query_drop(statement_text(&statement))
                .map_err(|e| ImportError {
                    line,
                    statement: number,
                    applied,
                    message: mysql_message(e),
                })?;
            applied += 1;
        }
        Ok(applied)
    }
}

fn mysql_message(error: mysql::Error) -> String {
    match error {
        mysql::Error::MySqlError(e) => e.message,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_utf8_literals_become_hex() {
        assert_eq!(statement_text(b"SELECT 'plain'"), "SELECT 'plain'");
        assert_eq!(
            statement_text(b"INSERT INTO t VALUES (_binary '\\0\xff\\n\\'','ok', `a``b`)"),
            "INSERT INTO t VALUES (_binary X'00ff0a27','ok', `a``b`)"
        );
    }

    #[test]
    fn switching_database_is_spotted() {
        assert!(switches_database(b"USE `x`"));
        assert!(switches_database(
            b"create database /*!32312 IF NOT EXISTS*/ `x`"
        ));
        assert!(switches_database(b"DROP SCHEMA x"));
        assert!(!switches_database(b"CREATE TABLE users (id int)"));
        assert!(!switches_database(b"USERS"));
        assert!(switches_database(b"USE`x`"));
        assert!(switches_database(
            b"/*!40000 DROP DATABASE IF EXISTS `shop`*/"
        ));
        assert!(switches_database(b"/*!32312 USE `shop`*/"));
        assert!(!switches_database(b"/*!40101 SET NAMES utf8mb4 */"));
    }

    #[test]
    fn loopback_and_sockets_stay_on_this_machine() {
        let at = |host: &str, socket: Option<&str>| DatabaseConnection {
            host: host.to_owned(),
            database: "d".to_owned(),
            user: "u".to_owned(),
            password: String::new(),
            port: 3306,
            socket: socket.map(str::to_owned),
            table_prefix: String::new(),
            charset: "utf8mb4".to_owned(),
            engine: "mysql".to_owned(),
        };
        for host in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "127.8.9.1",
            "::1",
            "[::1]",
        ] {
            assert!(stays_on_this_machine(&at(host, None)), "{host}");
        }
        for host in [
            "db.example.test",
            "10.0.0.5",
            "localhost.example.test",
            "::2",
        ] {
            assert!(!stays_on_this_machine(&at(host, None)), "{host}");
        }
        assert!(stays_on_this_machine(&at(
            "db.example.test",
            Some("/tmp/mysql.sock")
        )));
    }

    #[test]
    fn strings_are_escaped_like_the_server_does() {
        let mut out = Vec::new();
        quote_bytes(b"a'b\"c\\d\0e\nf\rg\x1ah", &mut out);
        assert_eq!(out, b"'a\\'b\\\"c\\\\d\\0e\\nf\\rg\\Zh'".to_vec());
    }
}
