//! The manifest at the head of every backup: what it is a backup of, so a restore can check it and move it.

use serde_json::{Value as Json, json};

/// The archive format; a newer one is refused rather than misread.
pub const FORMAT: u64 = 1;

/// What a backup says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub app: String,
    pub parts: Vec<String>,
    pub cli_version: String,
    pub app_version: Option<String>,
    /// `(kind, slug, version)` of the install's plugins and themes, as far as its app's plugin can tell.
    pub extensions: Vec<(String, String, Option<String>)>,
    pub engine: String,
    pub table_prefix: String,
    pub site_url: Option<String>,
    pub source_install: String,
    pub source_driver: String,
    /// RFC 3339, UTC.
    pub created_at: String,
}

impl Manifest {
    pub fn to_json(&self) -> Json {
        json!({
            "format": FORMAT,
            "app": self.app,
            "parts": self.parts,
            "cli_version": self.cli_version,
            "app_version": self.app_version,
            "extensions": self.extensions.iter().map(|(kind, slug, version)| {
                json!({"kind": kind, "slug": slug, "version": version})
            }).collect::<Vec<_>>(),
            "database": {"engine": self.engine, "table_prefix": self.table_prefix},
            "site_url": self.site_url,
            "source": {"install": self.source_install, "driver": self.source_driver},
            "created_at": self.created_at,
        })
    }

    pub fn from_json(json: &Json) -> Result<Manifest, String> {
        let format = json["format"]
            .as_u64()
            .ok_or("the manifest has no format")?;
        if format > FORMAT {
            return Err(format!(
                "the backup was made by a newer speedyspec (format {format}); update speedyspec"
            ));
        }
        let text = |value: &Json, what: &str| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("the manifest has no {what}"))
        };
        let optional = |value: &Json| value.as_str().map(str::to_owned);
        Ok(Manifest {
            app: text(&json["app"], "app")?,
            parts: json["parts"]
                .as_array()
                .map(|all| {
                    all.iter()
                        .filter_map(|p| p.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            cli_version: text(&json["cli_version"], "cli_version")?,
            app_version: optional(&json["app_version"]),
            extensions: json["extensions"]
                .as_array()
                .map(|all| {
                    all.iter()
                        .filter_map(|e| {
                            Some((
                                e["kind"].as_str()?.to_owned(),
                                e["slug"].as_str()?.to_owned(),
                                optional(&e["version"]),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            engine: text(&json["database"]["engine"], "database engine")?,
            table_prefix: text(&json["database"]["table_prefix"], "table prefix")?,
            site_url: optional(&json["site_url"]),
            source_install: text(&json["source"]["install"], "source install")?,
            source_driver: text(&json["source"]["driver"], "source driver")?,
            created_at: text(&json["created_at"], "created_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Manifest {
        Manifest {
            app: "wordpress".into(),
            parts: vec![],
            cli_version: "1.0.1".into(),
            app_version: Some("7.1.5".into()),
            extensions: vec![("plugin".into(), "akismet".into(), Some("5.3.5".into()))],
            engine: "mysql".into(),
            table_prefix: "wp_".into(),
            site_url: Some("https://old.example".into()),
            source_install: "blog".into(),
            source_driver: "zip".into(),
            created_at: "2026-11-19T10:00:00Z".into(),
        }
    }

    #[test]
    fn round_trips() {
        let m = sample();
        assert_eq!(Manifest::from_json(&m.to_json()).unwrap(), m);
    }

    #[test]
    fn a_newer_format_is_refused() {
        let mut json = sample().to_json();
        json["format"] = json!(2);
        assert!(
            Manifest::from_json(&json)
                .unwrap_err()
                .contains("newer speedyspec")
        );
    }
}
