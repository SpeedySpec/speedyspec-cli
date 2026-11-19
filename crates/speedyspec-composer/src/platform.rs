//! Checking a package's platform requirements against the embedded PHP: every `ext-*` must be loaded, and the `php`
//! constraint must allow its version. composer's constraint forms are read where they are simple (`^`, `~`, the
//! comparison operators, `*` and `x.y.*`, joined by spaces or commas for "and" and `||` or `|` for "or"); anything
//! else is reported as a warning rather than refused, so an unusual constraint never blocks an install.

use crate::ComposerError;
use crate::lock::Package;

/// The embedded PHP, as a package sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub php_version: String,
    /// Lowercase extension names.
    pub extensions: Vec<String>,
}

/// Up to three numeric parts; missing ones are 0.
fn parse_version(text: &str) -> Option<[u64; 3]> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let mut parts = [0u64; 3];
    let mut count = 0;
    for piece in text.split('.') {
        if count == 3 {
            // A fourth part (composer allows it) only refines; ignore it unless it is not a number.
            piece.parse::<u64>().ok()?;
            continue;
        }
        parts[count] = piece.parse().ok()?;
        count += 1;
    }
    (count > 0).then_some(parts)
}

/// How many parts `text` gives (`8` is 1, `8.1` is 2).
fn precision(text: &str) -> usize {
    text.trim().split('.').count().min(3)
}

fn bump(parts: [u64; 3], at: usize) -> [u64; 3] {
    let mut next = [0; 3];
    next[..at].copy_from_slice(&parts[..at]);
    next[at] = parts[at] + 1;
    next
}

/// One constraint without spaces: `^1.2`, `>=8.1`, `1.2.*`, `*`, `8.2.3`. `None` when it is not a form read here.
fn allows_one(constraint: &str, version: [u64; 3]) -> Option<bool> {
    let c = constraint.trim();
    if c == "*" {
        return Some(true);
    }
    for (op, cmp) in [
        (">=", 0u8),
        ("<=", 1),
        ("!=", 2),
        ("==", 3),
        (">", 4),
        ("<", 5),
        ("=", 3),
    ] {
        if let Some(rest) = c.strip_prefix(op) {
            let v = parse_version(rest)?;
            return Some(match cmp {
                0 => version >= v,
                1 => version <= v,
                2 => version != v,
                3 => version == v,
                4 => version > v,
                _ => version < v,
            });
        }
    }
    if let Some(rest) = c.strip_prefix('^') {
        let low = parse_version(rest)?;
        // The first non-zero part may not change (all zeros: the last given part).
        let at = low
            .iter()
            .take(precision(rest))
            .position(|p| *p != 0)
            .unwrap_or(precision(rest) - 1);
        return Some(version >= low && version < bump(low, at));
    }
    if let Some(rest) = c.strip_prefix('~') {
        let low = parse_version(rest)?;
        let at = precision(rest).saturating_sub(2);
        return Some(version >= low && version < bump(low, at));
    }
    if let Some(rest) = c.strip_suffix(".*") {
        let low = parse_version(rest)?;
        let at = precision(rest) - 1;
        return Some(version >= low && version < bump(low, at));
    }
    let exact = parse_version(c)?;
    Some(version == exact)
}

/// Whether `constraint` allows `version`; `None` when the constraint is not a form read here.
pub fn allows(constraint: &str, version: &str) -> Option<bool> {
    let version = parse_version(version.split(['-', '+']).next().unwrap_or(version))?;
    let mut any = false;
    for alternative in constraint.split("||").flat_map(|a| a.split('|')) {
        let mut all = true;
        let mut terms = 0;
        for term in alternative
            .split([' ', ','])
            .filter(|t| !t.trim().is_empty())
        {
            terms += 1;
            all &= allows_one(term, version)?;
        }
        if terms == 0 {
            return None;
        }
        any |= all;
    }
    Some(any)
}

/// Checks every package against `platform`. Gives back the warnings (constraints not read here); a requirement that is
/// not met is an error naming the package.
pub fn check(packages: &[Package], platform: &Platform) -> Result<Vec<String>, ComposerError> {
    let mut warnings = Vec::new();
    for package in packages {
        for (name, constraint) in &package.require {
            if let Some(extension) = name.strip_prefix("ext-") {
                if !platform
                    .extensions
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(extension))
                {
                    return Err(ComposerError::new(format!(
                        "{} {} needs the PHP extension {extension}, which speedyspec's PHP does not have",
                        package.name, package.version
                    )));
                }
            } else if name == "php" {
                match allows(constraint, &platform.php_version) {
                    Some(true) => {}
                    Some(false) => {
                        return Err(ComposerError::new(format!(
                            "{} {} needs PHP {constraint}; speedyspec's PHP is {}",
                            package.name, package.version, platform.php_version
                        )));
                    }
                    None => warnings.push(format!(
                        "{} {}: cannot read its PHP constraint {constraint:?}; not checked",
                        package.name, package.version
                    )),
                }
            }
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constraint_forms() {
        let cases = [
            ("^8.0", "8.5.12", true),
            ("^7.2.5 || ^8.0", "8.5.12", true),
            ("^7.4", "8.5.12", false),
            ("^0.3", "0.3.9", true),
            ("^0.3", "0.4.0", false),
            ("^0.0.3", "0.0.4", false),
            ("~8.1", "8.5.0", true),
            ("~8.1.2", "8.2.0", false),
            ("~8.1.2", "8.1.9", true),
            (">=8.1 <8.5", "8.5.12", false),
            (">=8.1,<9", "8.5.12", true),
            (">8.5.12", "8.5.12", false),
            ("<=8.5.12", "8.5.12", true),
            ("!=8.5.12", "8.5.12", false),
            ("8.5.*", "8.5.12", true),
            ("8.4.*", "8.5.12", false),
            ("*", "8.5.12", true),
            ("8.5.12", "8.5.12", true),
            ("^8.0|^9.0", "9.1.0", true),
        ];
        for (constraint, version, expected) in cases {
            assert_eq!(
                allows(constraint, version),
                Some(expected),
                "{constraint} vs {version}"
            );
        }
        for unread in ["1.0 - 2.0", "dev-main", "^8.0@dev", ""] {
            assert_eq!(allows(unread, "8.5.12"), None, "{unread:?}");
        }
    }

    fn package(require: &[(&str, &str)]) -> Package {
        Package {
            name: "acme/thing".into(),
            version: "1.0.0".into(),
            kind: "library".into(),
            dist: crate::lock::Dist {
                url: "https://example.com/a.zip".into(),
                shasum: None,
            },
            autoload: Default::default(),
            require: require
                .iter()
                .map(|(a, b)| ((*a).into(), (*b).into()))
                .collect(),
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn checks_extensions_and_php() {
        let platform = Platform {
            php_version: "8.5.12".into(),
            extensions: vec!["json".into(), "curl".into()],
        };
        assert!(
            check(&[package(&[("ext-json", "*"), ("php", "^8.0")])], &platform)
                .unwrap()
                .is_empty()
        );
        let error = check(&[package(&[("ext-gmp", "*")])], &platform)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("acme/thing") && error.contains("gmp"),
            "{error}"
        );
        let error = check(&[package(&[("php", "^7.4")])], &platform)
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs PHP ^7.4"), "{error}");
        let warnings = check(&[package(&[("php", "1.0 - 2.0")])], &platform).unwrap();
        assert_eq!(warnings.len(), 1);
    }
}
