//! Rust mirrors of the contracts for finishing an install and managing its plugins and themes: `SetupPlan`,
//! `ExtensionLayout`, `Extension`, `ExtensionSource` and `ExtensionArchive`.

use std::collections::BTreeMap;

use speedyspec_hooks::{Key, Value};

use crate::app::{AppTask, decode_app_task};
use crate::contracts::{
    ContractError, Decoder, ObjectReader, Props, destination_problem, is_fetchable_url,
};

/// The kinds of extension the CLI manages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Plugin,
    Theme,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "plugin" => Some(Kind::Plugin),
            "theme" => Some(Kind::Theme),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Plugin => "plugin",
            Kind::Theme => "theme",
        }
    }
}

/// The tasks that finish an install, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupPlan {
    pub tasks: Vec<AppTask>,
}

/// Where an app keeps its plugins and themes; `inspect` is the plugin's closure.
#[derive(Clone, Debug)]
pub struct ExtensionLayout {
    /// Relative to the install root; inside it, and not the root itself.
    pub directories: BTreeMap<Kind, String>,
    pub inspect: Value,
}

/// One plugin or theme found in an install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extension {
    pub kind: Kind,
    pub slug: String,
    pub name: String,
    pub version: Option<String>,
}

/// A place plugins and themes come from; `resolve` is the plugin's closure.
#[derive(Clone, Debug)]
pub struct ExtensionSource {
    pub name: String,
    pub resolve: Value,
}

/// The zip archive of one version of a plugin or theme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionArchive {
    pub kind: Kind,
    pub slug: String,
    pub version: String,
    pub url: String,
    pub sha256: Option<String>,
}

/// Why `slug` cannot name a plugin or theme directory, if it cannot. A slug is 1 to 200 bytes of letters, digits,
/// `.`, `_` and `-`, starting with neither `.` nor `-`, so it is always one plain path segment.
pub fn slug_problem(slug: &str) -> Option<&'static str> {
    if slug.is_empty() || slug.len() > 200 {
        return Some("must be 1 to 200 characters");
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Some("may hold only letters, digits, '.', '_' and '-'");
    }
    if slug.starts_with('.') || slug.starts_with('-') {
        return Some("must not start with '.' or '-'");
    }
    None
}

fn version_problem(version: &str) -> Option<&'static str> {
    if version.is_empty() || version.len() > 64 {
        return Some("must be 1 to 64 characters");
    }
    if version
        .chars()
        .any(|c| c.is_whitespace() || c == '/' || c == '\\' || c == '\0')
    {
        return Some("must not contain whitespace or a slash");
    }
    None
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn kind(d: &Decoder<'_>, props: &Props, path: &str) -> Result<Kind, ContractError> {
    let text = d.string(props, "kind", path)?;
    Kind::parse(&text).ok_or_else(|| d.err(&join(path, "kind"), "must be plugin or theme"))
}

fn slug(d: &Decoder<'_>, props: &Props, path: &str) -> Result<String, ContractError> {
    let text = d.string(props, "slug", path)?;
    match slug_problem(&text) {
        Some(problem) => Err(d.err(&join(path, "slug"), problem)),
        None => Ok(text),
    }
}

fn closure(d: &Decoder<'_>, props: &Props, name: &str, path: &str) -> Result<Value, ContractError> {
    match d.get(props, name, path)? {
        v @ Value::Opaque(_) => Ok(v.clone()),
        _ => Err(d.err(&join(path, name), "expected a closure object")),
    }
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}.{name}")
    }
}

fn list<'v>(
    d: &Decoder<'_>,
    value: &'v Value,
    path: &str,
) -> Result<Vec<&'v Value>, ContractError> {
    if let Value::Array(items) = value
        && items
            .iter()
            .enumerate()
            .all(|(i, (k, _))| *k == Key::Int(i as i64))
    {
        return Ok(items.iter().map(|(_, v)| v).collect());
    }
    Err(d.err(path, "expected a list (an array keyed 0, 1, 2, ...)"))
}

pub fn decode_setup_plan(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<SetupPlan, ContractError> {
    let d = Decoder {
        reader,
        contract: "SetupPlan",
    };
    let props = d.object(value, "SetupPlan", "")?;
    let tasks = list(&d, d.get(&props, "tasks", "")?, "tasks")?
        .into_iter()
        .enumerate()
        .map(|(i, task)| {
            decode_app_task(task, reader).map_err(|e| ContractError {
                contract: "SetupPlan",
                field: if e.field.is_empty() {
                    format!("tasks[{i}]")
                } else {
                    format!("tasks[{i}].{}", e.field)
                },
                problem: e.problem,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SetupPlan { tasks })
}

pub fn decode_extension_layout(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<ExtensionLayout, ContractError> {
    let d = Decoder {
        reader,
        contract: "ExtensionLayout",
    };
    let props = d.object(value, "ExtensionLayout", "")?;
    let Value::Array(items) = d.get(&props, "directories", "")? else {
        return Err(d.err(
            "directories",
            "expected an array of directories keyed by kind",
        ));
    };
    if items.is_empty() {
        return Err(d.err("directories", "must name at least one kind"));
    }
    let mut directories = BTreeMap::new();
    for (key, dir) in items {
        let (Key::Str(name), Value::Str(dir)) = (key, dir) else {
            return Err(d.err(
                "directories",
                "expected an array of directories keyed by kind",
            ));
        };
        let Some(kind) = Kind::parse(name) else {
            return Err(d.err("directories", "a kind must be plugin or theme"));
        };
        let field = format!("directories.{name}");
        if dir == "." {
            return Err(d.err(
                &field,
                "must be a directory inside the install, not the install itself",
            ));
        }
        if let Some(problem) = destination_problem(dir) {
            return Err(d.err(&field, problem));
        }
        directories.insert(kind, dir.clone());
    }
    Ok(ExtensionLayout {
        directories,
        inspect: closure(&d, &props, "inspect", "")?,
    })
}

pub fn decode_extension(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<Extension, ContractError> {
    let d = Decoder {
        reader,
        contract: "Extension",
    };
    let props = d.object(value, "Extension", "")?;
    let version = d.optional_string(&props, "version", "")?;
    if let Some(problem) = version.as_deref().and_then(version_problem) {
        return Err(d.err("version", problem));
    }
    Ok(Extension {
        kind: kind(&d, &props, "")?,
        slug: slug(&d, &props, "")?,
        name: d.string(&props, "name", "")?,
        version,
    })
}

pub fn decode_extension_sources(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<Vec<ExtensionSource>, ContractError> {
    let d = Decoder {
        reader,
        contract: "ExtensionSource",
    };
    list(&d, value, "")?
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            let path = format!("[{i}]");
            let props = d.object(item, "ExtensionSource", &path)?;
            let name = d.string(&props, "name", &path)?;
            if name.is_empty() {
                return Err(d.err(&join(&path, "name"), "must not be empty"));
            }
            Ok(ExtensionSource {
                name,
                resolve: closure(&d, &props, "resolve", &path)?,
            })
        })
        .collect()
}

pub fn decode_extension_archive(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<ExtensionArchive, ContractError> {
    let d = Decoder {
        reader,
        contract: "ExtensionArchive",
    };
    let props = d.object(value, "ExtensionArchive", "")?;
    let version = d.string(&props, "version", "")?;
    if let Some(problem) = version_problem(&version) {
        return Err(d.err("version", problem));
    }
    let url = d.string(&props, "url", "")?;
    if !is_fetchable_url(&url) {
        return Err(d.err(
            "url",
            "must be an https:// URL (or http:// to this machine)",
        ));
    }
    let sha256 = d.optional_string(&props, "sha256", "")?;
    if sha256.as_deref().is_some_and(|sha| !is_sha256(sha)) {
        return Err(d.err("sha256", "must be 64 lowercase hexadecimal characters"));
    }
    Ok(ExtensionArchive {
        kind: kind(&d, &props, "")?,
        slug: slug(&d, &props, "")?,
        version,
        url,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_reader::FakeReader;
    use speedyspec_hooks::{Key, Value};
    use std::any::Any;
    use std::rc::Rc;

    const NS: &str = "SpeedySpec\\Cli\\Contracts\\";

    fn s(v: &str) -> Value {
        Value::Str(v.to_owned())
    }

    fn list(items: Vec<Value>) -> Value {
        Value::Array(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (Key::Int(i as i64), v))
                .collect(),
        )
    }

    fn closure() -> Value {
        let rc: Rc<dyn Any> = Rc::new(1u8);
        Value::Opaque(rc)
    }

    fn task(r: &FakeReader, file: &str) -> Value {
        r.object(
            &format!("{NS}AppTask"),
            vec![
                ("file", s(file)),
                ("input", Value::Array(vec![])),
                ("loadsApp", Value::Bool(false)),
                ("timeoutSeconds", Value::Int(60)),
            ],
        )
    }

    fn layout(r: &FakeReader, directories: Vec<(&str, &str)>, inspect: Value) -> Value {
        let dirs = Value::Array(
            directories
                .into_iter()
                .map(|(k, v)| (Key::Str(k.to_owned()), s(v)))
                .collect(),
        );
        r.object(
            &format!("{NS}ExtensionLayout"),
            vec![("directories", dirs), ("inspect", inspect)],
        )
    }

    fn extension(r: &FakeReader, kind: &str, slug: &str, version: Value) -> Value {
        r.object(
            &format!("{NS}Extension"),
            vec![
                ("kind", s(kind)),
                ("slug", s(slug)),
                ("name", s("A Name")),
                ("version", version),
            ],
        )
    }

    fn source(r: &FakeReader, name: &str, resolve: Value) -> Value {
        r.object(
            &format!("{NS}ExtensionSource"),
            vec![("name", s(name)), ("resolve", resolve)],
        )
    }

    fn archive(r: &FakeReader, overrides: Vec<(&str, Value)>) -> Value {
        let mut props: Vec<(&str, Value)> = vec![
            ("kind", s("plugin")),
            ("slug", s("akismet")),
            ("version", s("5.3.1")),
            (
                "url",
                s("https://downloads.wordpress.org/plugin/akismet.5.3.1.zip"),
            ),
            ("sha256", Value::Null),
        ];
        for (name, value) in overrides {
            props.iter_mut().find(|(n, _)| *n == name).unwrap().1 = value;
        }
        r.object(&format!("{NS}ExtensionArchive"), props)
    }

    #[test]
    fn decodes_a_setup_plan() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.php");
        std::fs::write(&file, "<?php return fn () => 1;").unwrap();
        let r = FakeReader::default();
        let value = r.object(
            &format!("{NS}SetupPlan"),
            vec![(
                "tasks",
                list(vec![
                    task(&r, file.to_str().unwrap()),
                    task(&r, file.to_str().unwrap()),
                ]),
            )],
        );
        let plan = decode_setup_plan(&value, &r).unwrap();
        assert_eq!(plan.tasks.len(), 2);
        assert!(!plan.tasks[0].loads_app);
    }

    #[test]
    fn a_setup_plan_names_its_bad_task() {
        let r = FakeReader::default();
        let value = r.object(
            &format!("{NS}SetupPlan"),
            vec![("tasks", list(vec![task(&r, "relative.php")]))],
        );
        let error = decode_setup_plan(&value, &r).unwrap_err();
        assert_eq!(error.field, "tasks[0].file", "{error}");
        let value = r.object(&format!("{NS}SetupPlan"), vec![("tasks", s("no"))]);
        assert_eq!(decode_setup_plan(&value, &r).unwrap_err().field, "tasks");
    }

    #[test]
    fn decodes_a_layout() {
        let r = FakeReader::default();
        let value = layout(
            &r,
            vec![
                ("plugin", "wp-content/plugins"),
                ("theme", "wp-content/themes"),
            ],
            closure(),
        );
        let layout = decode_extension_layout(&value, &r).unwrap();
        assert_eq!(
            layout.directories.get(&Kind::Plugin).map(String::as_str),
            Some("wp-content/plugins")
        );
        assert_eq!(
            layout.directories.get(&Kind::Theme).map(String::as_str),
            Some("wp-content/themes")
        );
        assert!(matches!(layout.inspect, Value::Opaque(_)));
    }

    #[test]
    fn layout_rules() {
        let r = FakeReader::default();
        let cases: Vec<(Value, &str)> = vec![
            (layout(&r, vec![("module", "m")], closure()), "directories"),
            (
                layout(&r, vec![("plugin", "../out")], closure()),
                "directories.plugin",
            ),
            (
                layout(&r, vec![("plugin", "/abs")], closure()),
                "directories.plugin",
            ),
            (
                layout(&r, vec![("plugin", ".")], closure()),
                "directories.plugin",
            ),
            (layout(&r, vec![], closure()), "directories"),
            (
                layout(&r, vec![("plugin", "p")], s("not a closure")),
                "inspect",
            ),
        ];
        for (value, field) in cases {
            let error = decode_extension_layout(&value, &r).unwrap_err();
            assert_eq!(error.field, field, "{error}");
        }
    }

    #[test]
    fn decodes_an_extension() {
        let r = FakeReader::default();
        let found =
            decode_extension(&extension(&r, "theme", "twentytwentyfive", s("1.2")), &r).unwrap();
        assert_eq!(
            found,
            Extension {
                kind: Kind::Theme,
                slug: "twentytwentyfive".into(),
                name: "A Name".into(),
                version: Some("1.2".into()),
            }
        );
        let unversioned =
            decode_extension(&extension(&r, "plugin", "hello", Value::Null), &r).unwrap();
        assert_eq!(unversioned.version, None);
        for (value, field) in [
            (extension(&r, "module", "x", Value::Null), "kind"),
            (extension(&r, "plugin", "../x", Value::Null), "slug"),
            (extension(&r, "plugin", "x", s("1 2")), "version"),
        ] {
            assert_eq!(decode_extension(&value, &r).unwrap_err().field, field);
        }
    }

    #[test]
    fn decodes_an_archive() {
        let r = FakeReader::default();
        let sha = "a".repeat(64);
        let found = decode_extension_archive(&archive(&r, vec![("sha256", s(&sha))]), &r).unwrap();
        assert_eq!(found.kind, Kind::Plugin);
        assert_eq!(found.slug, "akismet");
        assert_eq!(found.version, "5.3.1");
        assert_eq!(found.sha256.as_deref(), Some(sha.as_str()));
    }

    #[test]
    fn archive_rules() {
        let r = FakeReader::default();
        let cases: Vec<(Vec<(&str, Value)>, &str)> = vec![
            (vec![("kind", s("module"))], "kind"),
            (vec![("slug", s("a/b"))], "slug"),
            (vec![("version", s(""))], "version"),
            (vec![("version", s("1/2"))], "version"),
            (vec![("version", s(&"9".repeat(65)))], "version"),
            (vec![("url", s("ftp://example.com/a.zip"))], "url"),
            (vec![("url", s("http://example.com/a.zip"))], "url"),
            (vec![("sha256", s("ABC"))], "sha256"),
            (vec![("sha256", s(&"A".repeat(64)))], "sha256"),
        ];
        for (overrides, field) in cases {
            let error = decode_extension_archive(&archive(&r, overrides), &r).unwrap_err();
            assert_eq!(error.field, field, "{error}");
        }
    }

    #[test]
    fn sources_keep_their_order() {
        let r = FakeReader::default();
        let value = list(vec![
            source(&r, "first", closure()),
            source(&r, "second", closure()),
        ]);
        let sources = decode_extension_sources(&value, &r).unwrap();
        let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["first", "second"]);
        let bad = list(vec![
            source(&r, "first", closure()),
            source(&r, "", closure()),
        ]);
        assert_eq!(
            decode_extension_sources(&bad, &r).unwrap_err().field,
            "[1].name"
        );
        let bad = list(vec![source(&r, "first", s("x"))]);
        assert_eq!(
            decode_extension_sources(&bad, &r).unwrap_err().field,
            "[0].resolve"
        );
    }

    #[test]
    fn slugs() {
        for good in [
            "akismet",
            "hello-dolly",
            "wp_super.cache",
            "A1",
            &"a".repeat(200),
        ] {
            assert_eq!(slug_problem(good), None, "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            ".hidden",
            "-flag",
            "a\0b",
            "a b",
            "é",
            &"a".repeat(201),
        ] {
            assert!(slug_problem(bad).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn kinds() {
        assert_eq!(Kind::parse("plugin"), Some(Kind::Plugin));
        assert_eq!(Kind::parse("theme"), Some(Kind::Theme));
        assert_eq!(Kind::parse("Plugin"), None);
        assert_eq!(Kind::Theme.as_str(), "theme");
    }
}
