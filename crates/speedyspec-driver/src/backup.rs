//! Rust mirrors of the contracts that describe an install's backups: `BackupSpec`, `SiteUrl` and `PrefixedColumn`.

use speedyspec_hooks::{Key, Value};

use crate::contracts::{ContractError, Decoder, ObjectReader, Props, destination_problem};

/// What a backup of an install holds and keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupSpec {
    /// Relative to the install root.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Each inside an included path.
    pub preserve: Vec<String>,
    pub site_url: Option<SiteUrl>,
    pub prefixed_columns: Vec<PrefixedColumn>,
    pub app_version: Option<String>,
    /// `(table without prefix, column)` whose values keep the old URL on a move.
    pub keep_urls: Vec<(String, String)>,
}

/// The row that holds the site URL; the table without its prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteUrl {
    pub table: String,
    pub key_column: String,
    pub key: String,
    pub value_column: String,
}

/// A column whose values may start with the table prefix; the table without its prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixedColumn {
    pub table: String,
    pub column: String,
    /// The keys, without the prefix, that are built from it; `None` for every value that starts with the prefix.
    pub keys: Option<Vec<String>>,
}

/// `[A-Za-z0-9_]+`: a table or column name that never needs more than backquotes.
pub fn is_sql_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn path_list(d: &Decoder<'_>, props: &Props, name: &str) -> Result<Vec<String>, ContractError> {
    let value = d.get(props, name, "")?;
    let Value::Array(items) = value else {
        return Err(d.err(name, "expected a list of paths"));
    };
    items
        .iter()
        .enumerate()
        .map(|(i, (key, item))| {
            let field = format!("{name}[{i}]");
            if *key != Key::Int(i as i64) {
                return Err(d.err(name, "expected a list of paths"));
            }
            let Value::Str(path) = item else {
                return Err(d.err(&field, "expected a string"));
            };
            match destination_problem(path) {
                Some(problem) => Err(d.err(&field, problem)),
                None => Ok(path.trim_end_matches('/').to_owned()),
            }
        })
        .collect()
}

fn name_at(
    d: &Decoder<'_>,
    props: &Props,
    name: &str,
    path: &str,
) -> Result<String, ContractError> {
    let text = d.string(props, name, path)?;
    if is_sql_name(&text) {
        Ok(text)
    } else {
        let field = if path.is_empty() {
            name.to_owned()
        } else {
            format!("{path}.{name}")
        };
        Err(d.err(
            &field,
            "must be letters, digits and underscores (a table without its prefix)",
        ))
    }
}

fn inside(path: &str, included: &str) -> bool {
    included == "." || path == included || path.starts_with(&format!("{included}/"))
}

pub fn decode_backup_spec(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<BackupSpec, ContractError> {
    let d = Decoder {
        reader,
        contract: "BackupSpec",
    };
    let props = d.object(value, "BackupSpec", "")?;
    let include = path_list(&d, &props, "include")?;
    if include.is_empty() {
        return Err(d.err("include", "must name at least one path"));
    }
    let exclude = path_list(&d, &props, "exclude")?;
    let preserve = path_list(&d, &props, "preserve")?;
    // Keeping or leaving out the whole install would leave a backup with no files, or a restore that removes it.
    for (name, paths) in [("exclude", &exclude), ("preserve", &preserve)] {
        if let Some(i) = paths.iter().position(|p| p == "." || p.is_empty()) {
            return Err(d.err(
                &format!("{name}[{i}]"),
                "must name a path inside the install, not the install itself",
            ));
        }
    }
    if let Some(i) = preserve
        .iter()
        .position(|p| !include.iter().any(|inc| inside(p, inc)))
    {
        return Err(d.err(
            &format!("preserve[{i}]"),
            "must lie inside an included path",
        ));
    }
    let site_url = match d.get(&props, "siteUrl", "")? {
        Value::Null => None,
        url => {
            let at = d.object(url, "SiteUrl", "siteUrl")?;
            Some(SiteUrl {
                table: name_at(&d, &at, "table", "siteUrl")?,
                key_column: name_at(&d, &at, "keyColumn", "siteUrl")?,
                key: d.string(&at, "key", "siteUrl")?,
                value_column: name_at(&d, &at, "valueColumn", "siteUrl")?,
            })
        }
    };
    let Value::Array(columns) = d.get(&props, "prefixedColumns", "")? else {
        return Err(d.err(
            "prefixedColumns",
            "expected a list of PrefixedColumn objects",
        ));
    };
    let prefixed_columns = columns
        .iter()
        .enumerate()
        .map(|(i, (_, item))| {
            let path = format!("prefixedColumns[{i}]");
            let at = d.object(item, "PrefixedColumn", &path)?;
            let keys = match d.get(&at, "keys", &path)? {
                Value::Null => None,
                Value::Array(items) => Some(
                    items
                        .iter()
                        .enumerate()
                        .map(|(k, (_, key))| match key {
                            Value::Str(key) if is_sql_name(key) => Ok(key.clone()),
                            _ => Err(d.err(
                                &format!("{path}.keys[{k}]"),
                                "must be letters, digits and underscores (a key without the prefix)",
                            )),
                        })
                        .collect::<Result<Vec<_>, ContractError>>()?,
                ),
                _ => return Err(d.err(&format!("{path}.keys"), "expected a list of keys or null")),
            };
            Ok(PrefixedColumn {
                table: name_at(&d, &at, "table", &path)?,
                column: name_at(&d, &at, "column", &path)?,
                keys,
            })
        })
        .collect::<Result<Vec<_>, ContractError>>()?;
    let app_version = d.optional_string(&props, "appVersion", "")?;
    if let Some(version) = &app_version
        && (version.is_empty() || version.len() > 64 || version.chars().any(char::is_whitespace))
    {
        return Err(d.err(
            "appVersion",
            "must be 1 to 64 characters with no whitespace",
        ));
    }
    // Since contracts 1.4; a BackupSpec from 1.3 has no such property and keeps none.
    let keep_urls = match props.iter().find(|(k, _)| k == "keepUrls").map(|(_, v)| v) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, (_, item))| {
                let path = format!("keepUrls[{i}]");
                let at = d.object(item, "TableColumn", &path)?;
                Ok((
                    name_at(&d, &at, "table", &path)?,
                    name_at(&d, &at, "column", &path)?,
                ))
            })
            .collect::<Result<Vec<_>, ContractError>>()?,
        Some(_) => return Err(d.err("keepUrls", "expected a list of TableColumn objects")),
    };
    Ok(BackupSpec {
        include,
        exclude,
        preserve,
        site_url,
        prefixed_columns,
        app_version,
        keep_urls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_reader::FakeReader;
    use speedyspec_hooks::{Key, Value};

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

    fn strings(items: &[&str]) -> Value {
        list(items.iter().map(|i| s(i)).collect())
    }

    fn site_url(r: &FakeReader, table: &str) -> Value {
        r.object(
            &format!("{NS}SiteUrl"),
            vec![
                ("table", s(table)),
                ("keyColumn", s("option_name")),
                ("key", s("siteurl")),
                ("valueColumn", s("option_value")),
            ],
        )
    }

    fn column(r: &FakeReader, table: &str, column: &str) -> Value {
        r.object(
            &format!("{NS}PrefixedColumn"),
            vec![
                ("table", s(table)),
                ("column", s(column)),
                ("keys", Value::Null),
            ],
        )
    }

    fn keyed(r: &FakeReader, keys: Value) -> Value {
        r.object(
            &format!("{NS}PrefixedColumn"),
            vec![
                ("table", s("options")),
                ("column", s("option_name")),
                ("keys", keys),
            ],
        )
    }

    fn spec(r: &FakeReader, overrides: Vec<(&str, Value)>) -> Value {
        let mut props: Vec<(&str, Value)> = vec![
            ("include", strings(&["."])),
            ("exclude", strings(&["wp-content/cache"])),
            ("preserve", strings(&["wp-config.php"])),
            ("siteUrl", site_url(r, "options")),
            (
                "prefixedColumns",
                list(vec![column(r, "options", "option_name")]),
            ),
            ("appVersion", s("7.1.5")),
        ];
        for (name, value) in overrides {
            props.iter_mut().find(|(n, _)| *n == name).unwrap().1 = value;
        }
        r.object(&format!("{NS}BackupSpec"), props)
    }

    #[test]
    fn decodes_a_backup_spec() {
        let r = FakeReader::default();
        let decoded = decode_backup_spec(&spec(&r, vec![]), &r).unwrap();
        assert_eq!(decoded.include, ["."]);
        assert_eq!(decoded.exclude, ["wp-content/cache"]);
        assert_eq!(decoded.preserve, ["wp-config.php"]);
        assert_eq!(
            decoded.site_url,
            Some(SiteUrl {
                table: "options".into(),
                key_column: "option_name".into(),
                key: "siteurl".into(),
                value_column: "option_value".into(),
            })
        );
        assert_eq!(
            decoded.prefixed_columns,
            [PrefixedColumn {
                table: "options".into(),
                column: "option_name".into(),
                keys: None,
            }]
        );
        assert_eq!(decoded.app_version.as_deref(), Some("7.1.5"));
        let bare = decode_backup_spec(
            &spec(
                &r,
                vec![("siteUrl", Value::Null), ("appVersion", Value::Null)],
            ),
            &r,
        )
        .unwrap();
        assert_eq!((bare.site_url, bare.app_version), (None, None));
    }

    #[test]
    fn backup_spec_rules() {
        let r = FakeReader::default();
        let cases: Vec<(Vec<(&str, Value)>, &str)> = vec![
            (vec![("include", strings(&[]))], "include"),
            (vec![("include", strings(&["../out"]))], "include[0]"),
            (vec![("exclude", strings(&["/abs"]))], "exclude[0]"),
            (
                vec![
                    ("include", strings(&["wp-content"])),
                    ("preserve", strings(&["wp-config.php"])),
                ],
                "preserve[0]",
            ),
            (
                vec![("siteUrl", site_url(&r, "wp_options; drop"))],
                "siteUrl.table",
            ),
            (
                vec![(
                    "prefixedColumns",
                    list(vec![column(&r, "usermeta", "meta key")]),
                )],
                "prefixedColumns[0].column",
            ),
            (vec![("prefixedColumns", s("no"))], "prefixedColumns"),
            (vec![("preserve", strings(&["."]))], "preserve[0]"),
            (vec![("exclude", strings(&["."]))], "exclude[0]"),
            (vec![("appVersion", s("7 1"))], "appVersion"),
            (vec![("appVersion", s(""))], "appVersion"),
        ];
        for (overrides, field) in cases {
            let error = decode_backup_spec(&spec(&r, overrides), &r).unwrap_err();
            assert_eq!(error.field, field, "{error}");
        }
    }

    #[test]
    fn prefixed_keys() {
        let r = FakeReader::default();
        let decoded = decode_backup_spec(
            &spec(
                &r,
                vec![(
                    "prefixedColumns",
                    list(vec![keyed(&r, strings(&["user_roles"]))]),
                )],
            ),
            &r,
        )
        .unwrap();
        assert_eq!(
            decoded.prefixed_columns[0].keys,
            Some(vec!["user_roles".to_owned()])
        );
        let bad = decode_backup_spec(
            &spec(
                &r,
                vec![(
                    "prefixedColumns",
                    list(vec![keyed(&r, strings(&["user roles"]))]),
                )],
            ),
            &r,
        )
        .unwrap_err();
        assert_eq!(bad.field, "prefixedColumns[0].keys[0]", "{bad}");
    }

    #[test]
    fn columns_that_keep_their_urls() {
        let r = FakeReader::default();
        // A 1.3 BackupSpec has no keepUrls: none are kept.
        assert!(
            decode_backup_spec(&spec(&r, vec![]), &r)
                .unwrap()
                .keep_urls
                .is_empty()
        );
        let guid = r.object(
            &format!("{NS}TableColumn"),
            vec![("table", s("posts")), ("column", s("guid"))],
        );
        let mut with = spec(&r, vec![]);
        if let Value::Opaque(_) = &with {
            let props: Vec<(&str, Value)> = vec![
                ("include", strings(&["."])),
                ("exclude", strings(&[])),
                ("preserve", strings(&[])),
                ("siteUrl", Value::Null),
                ("prefixedColumns", list(vec![])),
                ("appVersion", Value::Null),
                ("keepUrls", list(vec![guid])),
            ];
            with = r.object(&format!("{NS}BackupSpec"), props);
        }
        let decoded = decode_backup_spec(&with, &r).unwrap();
        assert_eq!(decoded.keep_urls, [("posts".to_owned(), "guid".to_owned())]);
    }

    #[test]
    fn a_preserve_path_inside_an_included_directory_is_fine() {
        let r = FakeReader::default();
        let decoded = decode_backup_spec(
            &spec(
                &r,
                vec![
                    ("include", strings(&["wp-content", "wp-config.php"])),
                    (
                        "preserve",
                        strings(&["wp-content/uploads/keep", "wp-config.php"]),
                    ),
                ],
            ),
            &r,
        )
        .unwrap();
        assert_eq!(decoded.preserve.len(), 2);
    }
}
