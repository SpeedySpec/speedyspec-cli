//! Rust mirrors of the contracts for maintaining an installed app: `Install`, `AppRuntime`,
//! `DatabaseConnection` and `AppTask`.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value as Json;
use speedyspec_hooks::{Key, Value};

use crate::contracts::{ContractError, Decoder, NS, ObjectReader, Props, destination_problem};
use crate::kubernetes::value_to_json;

/// The longest a task may be given to run: a day.
const LONGEST_TASK: i64 = 86_400;

/// One installed app, as the CLI hands it to the hooks that maintain it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    pub name: String,
    pub app: String,
    pub parts: Vec<String>,
    pub driver: String,
    /// The install's directory on this machine; `None` where its files are elsewhere.
    pub root: Option<PathBuf>,
}

/// How to load an app in the runner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppRuntime {
    /// Relative to the install root; inside it.
    pub entry: Option<String>,
    pub ini: Vec<(String, String)>,
    pub server: Vec<(String, String)>,
}

/// Where an app's database is. `Debug` never shows the password.
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseConnection {
    pub host: String,
    pub database: String,
    pub user: String,
    pub password: String,
    pub port: u16,
    pub socket: Option<String>,
    pub table_prefix: String,
    pub charset: String,
    /// `mysql` (MySQL and MariaDB).
    pub engine: String,
}

impl fmt::Debug for DatabaseConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseConnection")
            .field("host", &self.host)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("port", &self.port)
            .field("socket", &self.socket)
            .field("table_prefix", &self.table_prefix)
            .field("charset", &self.charset)
            .field("engine", &self.engine)
            .finish()
    }
}

/// PHP to run inside an app, in the runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppTask {
    /// Absolute; it exists.
    pub file: PathBuf,
    pub input: Json,
    pub loads_app: bool,
    pub timeout: Duration,
}

/// What `speedyspec_app_database` may answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseOrTask {
    Connection(DatabaseConnection),
    /// A task whose result is the connection, as a JSON object.
    Task(AppTask),
}

fn integer(d: &Decoder<'_>, props: &Props, name: &str) -> Result<i64, ContractError> {
    match d.get(props, name, "")? {
        Value::Int(number) => Ok(*number),
        _ => Err(d.err(name, "expected an int")),
    }
}

/// A PHP array of string keys to string values (the empty array included).
fn string_map(
    d: &Decoder<'_>,
    props: &Props,
    name: &str,
) -> Result<Vec<(String, String)>, ContractError> {
    let Value::Array(items) = d.get(props, name, "")? else {
        return Err(d.err(name, "expected an array of strings keyed by name"));
    };
    items
        .iter()
        .map(|(key, value)| match (key, value) {
            (Key::Str(key), Value::Str(value)) => Ok((key.clone(), value.clone())),
            _ => Err(d.err(name, "expected an array of strings keyed by name")),
        })
        .collect()
}

fn word(text: &str, allow_empty: bool) -> bool {
    (allow_empty || !text.is_empty())
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub fn decode_app_runtime(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<AppRuntime, ContractError> {
    let d = Decoder {
        reader,
        contract: "AppRuntime",
    };
    let props = d.object(value, "AppRuntime", "")?;
    let entry = d.optional_string(&props, "entry", "")?;
    if let Some(problem) = entry.as_deref().and_then(destination_problem) {
        return Err(d.err("entry", problem));
    }
    Ok(AppRuntime {
        entry,
        ini: string_map(&d, &props, "ini")?,
        server: string_map(&d, &props, "server")?,
    })
}

pub fn decode_database_connection(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<DatabaseConnection, ContractError> {
    let d = Decoder {
        reader,
        contract: "DatabaseConnection",
    };
    let props = d.object(value, "DatabaseConnection", "")?;
    let non_empty = |name: &str| -> Result<String, ContractError> {
        let text = d.string(&props, name, "")?;
        if text.is_empty() {
            return Err(d.err(name, "must not be empty"));
        }
        Ok(text)
    };
    let host = non_empty("host")?;
    let database = non_empty("database")?;
    let user = non_empty("user")?;
    let password = d.string(&props, "password", "")?;
    let port = integer(&d, &props, "port")?;
    let port = u16::try_from(port)
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| d.err("port", "must be 1 to 65535"))?;
    let socket = d.optional_string(&props, "socket", "")?;
    let table_prefix = d.string(&props, "tablePrefix", "")?;
    if !word(&table_prefix, true) {
        return Err(d.err("tablePrefix", "must be letters, digits and underscores"));
    }
    let charset = d.string(&props, "charset", "")?;
    if !word(&charset, false) {
        return Err(d.err("charset", "must be letters, digits and underscores"));
    }
    let engine = d.string(&props, "engine", "")?;
    if engine != "mysql" {
        return Err(d.err(
            "engine",
            "must be \"mysql\" (MySQL and MariaDB are the databases supported)",
        ));
    }
    Ok(DatabaseConnection {
        host,
        database,
        user,
        password,
        port,
        socket,
        table_prefix,
        charset,
        engine,
    })
}

pub fn decode_app_task(value: &Value, reader: &dyn ObjectReader) -> Result<AppTask, ContractError> {
    let d = Decoder {
        reader,
        contract: "AppTask",
    };
    let props = d.object(value, "AppTask", "")?;
    let file = PathBuf::from(d.string(&props, "file", "")?);
    if !file.is_absolute() {
        return Err(d.err("file", "must be an absolute path (build it from __DIR__)"));
    }
    if !file.is_file() {
        return Err(d.err("file", "is not a file"));
    }
    let input = value_to_json(d.get(&props, "input", "")?, reader)
        .map_err(|problem| d.err("input", &format!("must be JSON-encodable: {problem}")))?;
    let loads_app = d.boolean(&props, "loadsApp", "")?;
    let seconds = integer(&d, &props, "timeoutSeconds")?;
    if !(1..=LONGEST_TASK).contains(&seconds) {
        return Err(d.err("timeoutSeconds", "must be 1 to 86400"));
    }
    Ok(AppTask {
        file,
        input,
        loads_app,
        timeout: Duration::from_secs(seconds.unsigned_abs()),
    })
}

pub fn decode_database_or_task(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<DatabaseOrTask, ContractError> {
    match reader.class_of(value).as_deref() {
        Some(class) if class == format!("{NS}DatabaseConnection") => {
            decode_database_connection(value, reader).map(DatabaseOrTask::Connection)
        }
        Some(class) if class == format!("{NS}AppTask") => {
            decode_app_task(value, reader).map(DatabaseOrTask::Task)
        }
        _ => Err(ContractError {
            contract: "DatabaseConnection",
            field: String::new(),
            problem: format!("expected an object of class {NS}DatabaseConnection or {NS}AppTask"),
        }),
    }
}

/// A connection from a task's result: an object with `host`, `database`, `user` and `password`,
/// and optionally `port`, `socket`, `tablePrefix` and `charset`, as `DatabaseConnection` has them.
pub fn database_connection_from_json(json: &Json) -> Result<DatabaseConnection, String> {
    let text = |name: &str| -> Result<String, String> {
        json[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("the task's result has no {name} string"))
    };
    let optional = |name: &str, default: &str| -> Result<String, String> {
        match &json[name] {
            Json::Null => Ok(default.to_owned()),
            Json::String(text) => Ok(text.clone()),
            _ => Err(format!(
                "the task's result has a {name} that is not a string"
            )),
        }
    };
    let port = match &json["port"] {
        Json::Null => 3306,
        number => number
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0)
            .ok_or("the task's result has a port that is not 1 to 65535")?,
    };
    let table_prefix = optional("tablePrefix", "")?;
    let charset = optional("charset", "utf8mb4")?;
    if !word(&table_prefix, true) || !word(&charset, false) {
        return Err(
            "the task's result has a tablePrefix or charset that is not letters, digits and \
             underscores"
                .to_owned(),
        );
    }
    let host = text("host")?;
    let database = text("database")?;
    if host.is_empty() || database.is_empty() {
        return Err("the task's result has an empty host or database".to_owned());
    }
    Ok(DatabaseConnection {
        host,
        database,
        user: text("user")?,
        password: text("password")?,
        port,
        socket: json["socket"].as_str().map(str::to_owned),
        table_prefix,
        charset,
        engine: "mysql".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_reader::FakeReader;
    use speedyspec_hooks::{Key, Value};

    const RUNTIME: &str = "SpeedySpec\\Cli\\Contracts\\AppRuntime";
    const DATABASE: &str = "SpeedySpec\\Cli\\Contracts\\DatabaseConnection";
    const TASK: &str = "SpeedySpec\\Cli\\Contracts\\AppTask";

    fn s(v: &str) -> Value {
        Value::Str(v.to_owned())
    }

    fn map(entries: &[(&str, &str)]) -> Value {
        Value::Array(
            entries
                .iter()
                .map(|(k, v)| (Key::Str((*k).to_owned()), s(v)))
                .collect(),
        )
    }

    fn runtime(r: &FakeReader, entry: Value, ini: Value, server: Value) -> Value {
        r.object(
            RUNTIME,
            vec![("entry", entry), ("ini", ini), ("server", server)],
        )
    }

    fn database(r: &FakeReader, overrides: Vec<(&str, Value)>) -> Value {
        let mut props: Vec<(&str, Value)> = vec![
            ("host", s("db")),
            ("database", s("blog")),
            ("user", s("blog")),
            ("password", s("hunter2-secret")),
            ("port", Value::Int(3306)),
            ("socket", Value::Null),
            ("tablePrefix", s("wp_")),
            ("charset", s("utf8mb4")),
            ("engine", s("mysql")),
        ];
        for (name, value) in overrides {
            props.iter_mut().find(|(n, _)| *n == name).unwrap().1 = value;
        }
        r.object(DATABASE, props)
    }

    fn task(r: &FakeReader, file: &str, overrides: Vec<(&str, Value)>) -> Value {
        let mut props: Vec<(&str, Value)> = vec![
            ("file", s(file)),
            (
                "input",
                Value::Array(vec![(Key::Str("x".into()), Value::Int(1))]),
            ),
            ("loadsApp", Value::Bool(true)),
            ("timeoutSeconds", Value::Int(600)),
        ];
        for (name, value) in overrides {
            props.iter_mut().find(|(n, _)| *n == name).unwrap().1 = value;
        }
        r.object(TASK, props)
    }

    #[test]
    fn decodes_a_runtime() {
        let r = FakeReader::default();
        let value = runtime(
            &r,
            s("wp-load.php"),
            map(&[("memory_limit", "512M")]),
            map(&[("HTTP_HOST", "example.test")]),
        );
        let decoded = decode_app_runtime(&value, &r).unwrap();
        assert_eq!(decoded.entry.as_deref(), Some("wp-load.php"));
        assert_eq!(
            decoded.ini,
            vec![("memory_limit".to_owned(), "512M".to_owned())]
        );
        assert_eq!(
            decoded.server,
            vec![("HTTP_HOST".to_owned(), "example.test".to_owned())]
        );
        let none = runtime(&r, Value::Null, Value::Array(vec![]), Value::Array(vec![]));
        assert_eq!(decode_app_runtime(&none, &r).unwrap().entry, None);
    }

    #[test]
    fn runtime_entry_must_stay_inside_the_root() {
        let r = FakeReader::default();
        for bad in ["../wp-load.php", "/etc/passwd", ""] {
            let value = runtime(&r, s(bad), Value::Array(vec![]), Value::Array(vec![]));
            let error = decode_app_runtime(&value, &r).unwrap_err();
            assert_eq!(error.field, "entry", "{bad:?}: {error}");
        }
        let value = runtime(
            &r,
            s("x.php"),
            Value::Array(vec![(Key::Int(0), s("y"))]),
            Value::Array(vec![]),
        );
        assert_eq!(decode_app_runtime(&value, &r).unwrap_err().field, "ini");
    }

    #[test]
    fn decodes_a_database_connection() {
        let r = FakeReader::default();
        let decoded = decode_database_connection(&database(&r, vec![]), &r).unwrap();
        assert_eq!(decoded.host, "db");
        assert_eq!(decoded.database, "blog");
        assert_eq!(decoded.port, 3306);
        assert_eq!(decoded.table_prefix, "wp_");
        assert_eq!(decoded.password, "hunter2-secret");
    }

    #[test]
    fn database_connection_rules() {
        let r = FakeReader::default();
        for (field, value) in [
            ("port", Value::Int(0)),
            ("port", Value::Int(65_536)),
            ("charset", s("utf8; DROP")),
            ("tablePrefix", s("wp-")),
            ("engine", s("postgres")),
            ("host", s("")),
            ("database", s("")),
        ] {
            let error = decode_database_connection(&database(&r, vec![(field, value.clone())]), &r)
                .unwrap_err();
            assert_eq!(error.field, field, "{value:?}: {error}");
        }
        let socket = database(
            &r,
            vec![("host", s("localhost")), ("socket", s("/tmp/mysql.sock"))],
        );
        assert_eq!(
            decode_database_connection(&socket, &r)
                .unwrap()
                .socket
                .as_deref(),
            Some("/tmp/mysql.sock")
        );
    }

    #[test]
    fn debug_never_shows_the_password() {
        let r = FakeReader::default();
        let decoded = decode_database_connection(&database(&r, vec![]), &r).unwrap();
        let shown = format!("{decoded:?}");
        assert!(!shown.contains("hunter2-secret"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn decodes_a_task() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("task.php");
        std::fs::write(&file, "<?php return fn (array $i) => $i;").unwrap();
        let r = FakeReader::default();
        let decoded = decode_app_task(&task(&r, file.to_str().unwrap(), vec![]), &r).unwrap();
        assert_eq!(decoded.file, file);
        assert_eq!(decoded.input, serde_json::json!({"x": 1}));
        assert!(decoded.loads_app);
        assert_eq!(decoded.timeout, std::time::Duration::from_secs(600));
    }

    #[test]
    fn task_rules() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("task.php");
        std::fs::write(&file, "<?php").unwrap();
        let file = file.to_str().unwrap();
        let r = FakeReader::default();
        assert_eq!(
            decode_app_task(&task(&r, "relative.php", vec![]), &r)
                .unwrap_err()
                .field,
            "file"
        );
        let missing = dir.path().join("missing.php");
        assert_eq!(
            decode_app_task(&task(&r, missing.to_str().unwrap(), vec![]), &r)
                .unwrap_err()
                .field,
            "file"
        );
        for timeout in [0, 86_401] {
            let error = decode_app_task(
                &task(&r, file, vec![("timeoutSeconds", Value::Int(timeout))]),
                &r,
            )
            .unwrap_err();
            assert_eq!(error.field, "timeoutSeconds");
        }
        let opaque = r.object("Closure", vec![]);
        let error = decode_app_task(
            &task(
                &r,
                file,
                vec![("input", Value::Array(vec![(Key::Int(0), opaque)]))],
            ),
            &r,
        )
        .unwrap_err();
        assert_eq!(error.field, "input");
    }

    #[test]
    fn database_or_task() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("task.php");
        std::fs::write(&file, "<?php").unwrap();
        let r = FakeReader::default();
        assert!(matches!(
            decode_database_or_task(&database(&r, vec![]), &r).unwrap(),
            DatabaseOrTask::Connection(_)
        ));
        assert!(matches!(
            decode_database_or_task(&task(&r, file.to_str().unwrap(), vec![]), &r).unwrap(),
            DatabaseOrTask::Task(_)
        ));
        let error = decode_database_or_task(&s("nope"), &r).unwrap_err();
        assert!(error.problem.contains("DatabaseConnection or"), "{error}");
    }

    #[test]
    fn connection_from_a_task_result() {
        let json = serde_json::json!({"host": "db", "database": "blog", "user": "u", "password": "p", "port": 3307});
        let decoded = database_connection_from_json(&json).unwrap();
        assert_eq!(
            (
                decoded.port,
                decoded.charset.as_str(),
                decoded.table_prefix.as_str()
            ),
            (3307, "utf8mb4", "")
        );
        let missing = serde_json::json!({"host": "db"});
        assert!(
            database_connection_from_json(&missing)
                .unwrap_err()
                .contains("database")
        );
    }
}
