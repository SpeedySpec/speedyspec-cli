//! Install records: what `app install` leaves on this machine so later commands find an install
//! by name. One JSON file per install, `$SPEEDYSPEC_HOME/installs/<name>.json`. Drivers whose
//! installs live elsewhere (Kubernetes) keep their own record there instead.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value as Json, json};
use speedyspec_driver::LocalInstall;

/// The record format; a newer one is refused rather than misread.
pub const SCHEMA: u64 = 1;

/// One install on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub app: String,
    pub parts: Vec<String>,
    pub driver: String,
    /// Where the install is: its base directory, for zip.
    pub location: String,
    pub cli_version: String,
    /// RFC 3339, UTC.
    pub installed_at: String,
}

impl Record {
    pub fn new(local: &LocalInstall, app: &str, parts: &[String], driver: &str) -> Record {
        Record {
            name: local.name.clone(),
            app: app.to_owned(),
            parts: parts.to_vec(),
            driver: driver.to_owned(),
            location: local.location.to_string_lossy().into_owned(),
            cli_version: crate::self_cmd::VERSION.to_owned(),
            installed_at: now_rfc3339(),
        }
    }

    fn to_json(&self) -> Json {
        json!({
            "schema": SCHEMA,
            "name": self.name,
            "app": self.app,
            "parts": self.parts,
            "driver": self.driver,
            "location": self.location,
            "cli_version": self.cli_version,
            "installed_at": self.installed_at,
            "status": "installed",
        })
    }

    fn from_json(json: &Json) -> Result<Record, String> {
        let schema = json["schema"].as_u64().ok_or("it has no schema")?;
        if schema > SCHEMA {
            return Err(format!(
                "it was written by a newer speedyspec (record schema {schema}); update speedyspec"
            ));
        }
        let text = |key: &str| {
            json[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("it has no {key}"))
        };
        let parts = json["parts"]
            .as_array()
            .ok_or("it has no parts")?
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_owned)
                    .ok_or("a part is not a string")
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Record {
            name: text("name")?,
            app: text("app")?,
            parts,
            driver: text("driver")?,
            location: text("location")?,
            cli_version: text("cli_version")?,
            installed_at: text("installed_at")?,
        })
    }
}

/// Where install records live, if there is a speedyspec home.
pub fn dir() -> Option<PathBuf> {
    speedyspec_plugin::home_dir()
        .dir
        .map(|home| home.join("installs"))
}

fn path_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// The record of install `name`; `Ok(None)` when there is none.
pub fn find(name: &str) -> Result<Option<Record>, String> {
    let Some(dir) = dir() else {
        return Ok(None);
    };
    let path = path_in(&dir, name);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let json: Json = serde_json::from_str(&text)
                .map_err(|e| format!("{}: not a valid install record: {e}", path.display()))?;
            Record::from_json(&json)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// The record of install `name`, or a message saying there is none.
pub fn read(name: &str) -> Result<Record, String> {
    find(name)?.ok_or_else(|| format!("no install named {name}; app installs lists them"))
}

/// Why installing `local` would clash with an existing record, if it would: same name, but another
/// app or another place. The same name, app and place is a re-install.
pub fn clash(local: &LocalInstall, app: &str) -> Result<Option<String>, String> {
    let Some(existing) = find(&local.name)? else {
        return Ok(None);
    };
    let here = local.location.to_string_lossy();
    if existing.app == app && existing.location == here {
        return Ok(None);
    }
    let path = dir()
        .map(|dir| path_in(&dir, &local.name))
        .unwrap_or_default();
    Ok(Some(format!(
        "an install named {} already exists (app {}, at {}); choose another --name, or remove {}",
        local.name,
        existing.app,
        existing.location,
        path.display()
    )))
}

/// Writes `record`, replacing any record of the same name, atomically.
pub fn write(record: &Record) -> Result<(), String> {
    let dir = dir().ok_or(
        "there is no speedyspec home to record the install in; set SPEEDYSPEC_HOME to an absolute \
         path",
    )?;
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut file = tempfile::Builder::new()
        .prefix(".record-")
        .tempfile_in(&dir)
        .map_err(|e| format!("cannot write in {}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(&record.to_json()).expect("a record is JSON");
    file.write_all(text.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|e| format!("cannot write the install record: {e}"))?;
    let path = path_in(&dir, &record.name);
    file.persist(&path)
        .map_err(|e| format!("cannot write {}: {}", path.display(), e.error))?;
    Ok(())
}

/// Every readable record, sorted by name; each unreadable file is reported on stderr and skipped.
pub fn list() -> Vec<Record> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let read = fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str::<Json>(&text).map_err(|e| e.to_string()))
            .and_then(|json| Record::from_json(&json));
        match read {
            Ok(record) => records.push(record),
            Err(e) => eprintln!("speedyspec: skipping {}: {e}", path.display()),
        }
    }
    records.sort_by(|a, b| a.name.cmp(&b.name));
    records
}

/// Now, as `2026-11-18T09:15:01Z`.
fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    rfc3339(seconds)
}

/// Seconds since 1970 as an RFC 3339 UTC timestamp (Howard Hinnant's civil-from-days).
fn rfc3339(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// `app installs`.
pub fn print_list() -> i32 {
    let mut out = std::io::stdout().lock();
    for record in list() {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}",
            record.name, record.app, record.driver, record.location
        );
    }
    eprintln!(
        "speedyspec: Kubernetes installs are kept in their cluster; app uninstall finds them there"
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_794_993_301), "2026-11-18T09:15:01Z");
    }

    #[test]
    fn newer_schema_is_refused() {
        let error = Record::from_json(&json!({"schema": 2})).unwrap_err();
        assert!(error.contains("newer speedyspec"), "{error}");
    }
}
