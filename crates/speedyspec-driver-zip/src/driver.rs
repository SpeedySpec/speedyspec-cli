//! The zip driver as the CLI sees it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use speedyspec_driver::{
    ContractError, Driver, DriverError, DriverFlag, DriverInput, ObjectReader, Prepared, Progress,
    ZipSource, decode_zip_source,
};
use speedyspec_hooks::{Key, Value};

use crate::fetch::{Fetch, HttpFetch};
use crate::install::install_sources;

/// Downloads the archives a plugin names and unpacks them under a base directory.
pub struct ZipDriver {
    fetch: Box<dyn Fetch>,
}

/// What `prepare` settled for one install.
struct Settings {
    base: PathBuf,
    force: bool,
}

impl ZipDriver {
    pub fn new() -> ZipDriver {
        Self::with_fetch(Box::new(HttpFetch::new()))
    }

    /// A driver that downloads through `fetch` (tests install without a network).
    pub fn with_fetch(fetch: Box<dyn Fetch>) -> ZipDriver {
        ZipDriver { fetch }
    }
}

impl Default for ZipDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver for ZipDriver {
    fn id(&self) -> &'static str {
        "zip"
    }

    fn flags(&self) -> Vec<DriverFlag> {
        vec![
            DriverFlag {
                name: "base",
                help: "Directory to install into",
                takes_value: true,
                default: Some("."),
            },
            DriverFlag {
                name: "force",
                help: "Merge into non-empty destinations, replacing files of the same path",
                takes_value: false,
                default: None,
            },
        ]
    }

    fn prepare(&self, input: &DriverInput<'_>) -> Result<Prepared, DriverError> {
        let given: &BTreeMap<String, String> = input.flags;
        let base = given.get("base").map_or(".", String::as_str);
        let base =
            std::path::absolute(base).map_err(|e| DriverError(format!("--base {base}: {e}")))?;
        // `absolute` keeps a trailing "." component; drop it so the path reads as a directory.
        let base: PathBuf = base.components().collect();
        Ok(Prepared {
            settings: vec![("base".to_owned(), base.to_string_lossy().into_owned())],
            state: Box::new(Settings {
                base,
                force: given.contains_key("force"),
            }),
        })
    }

    fn check_answer(&self, answer: &Value, reader: &dyn ObjectReader) -> Result<(), ContractError> {
        decode_sources(answer, reader).map(|_| ())
    }

    fn install(
        &self,
        prepared: &Prepared,
        answer: &Value,
        reader: &dyn ObjectReader,
        progress: &mut dyn Progress,
    ) -> Result<Value, DriverError> {
        let settings = prepared.state.downcast_ref::<Settings>().ok_or_else(|| {
            DriverError("the zip driver was handed another driver's state".to_owned())
        })?;
        let sources = decode_sources(answer, reader).map_err(|e| DriverError(e.to_string()))?;
        if sources.is_empty() {
            return Err(DriverError(
                "no plugin supplied anything to install".to_owned(),
            ));
        }
        let placed = install_sources(
            &sources,
            &settings.base,
            settings.force,
            self.fetch.as_ref(),
            progress,
        )
        .map_err(|e| DriverError(e.to_string()))?;
        let items = placed
            .into_iter()
            .enumerate()
            .map(|(index, placement)| {
                let fields = vec![
                    (Key::Str("url".to_owned()), Value::Str(placement.url)),
                    (
                        Key::Str("destination".to_owned()),
                        Value::Str(placement.destination),
                    ),
                    (
                        Key::Str("path".to_owned()),
                        Value::Str(placement.path.to_string_lossy().into_owned()),
                    ),
                ];
                (Key::Int(index as i64), Value::Array(fields))
            })
            .collect();
        Ok(Value::Array(items))
    }
}

/// The answer as checked sources: a list of `ZipSource`, no two for the same destination.
fn decode_sources(
    answer: &Value,
    reader: &dyn ObjectReader,
) -> Result<Vec<ZipSource>, ContractError> {
    let error = |field: String, problem: &str| ContractError {
        contract: "ZipSource",
        field,
        problem: problem.to_owned(),
    };
    let items = match answer {
        Value::Array(items)
            if items
                .iter()
                .enumerate()
                .all(|(i, (key, _))| *key == Key::Int(i as i64)) =>
        {
            items
        }
        _ => {
            return Err(error(
                String::new(),
                "expected a list (an array keyed 0, 1, 2, ...)",
            ));
        }
    };
    let mut seen: Vec<Vec<String>> = Vec::new();
    let mut sources = Vec::with_capacity(items.len());
    for (index, (_, item)) in items.iter().enumerate() {
        let source = decode_zip_source(item, reader).map_err(|e| ContractError {
            field: if e.field.is_empty() {
                format!("[{index}]")
            } else {
                format!("[{index}].{}", e.field)
            },
            ..e
        })?;
        let normal: Vec<String> = source
            .destination
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .map(str::to_owned)
            .collect();
        if seen.contains(&normal) {
            return Err(error(
                format!("[{index}].destination"),
                "another source already installs to this destination",
            ));
        }
        seen.push(normal);
        sources.push(source);
    }
    Ok(sources)
}
