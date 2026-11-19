//! The classmap scanner: the classes, interfaces, traits and enums a PHP file declares, with their namespaces, found
//! the way composer's own scanner finds them, without running PHP. Text outside `<?php … ?>`, comments, strings,
//! heredocs and nowdocs are skipped, and `X::class`, `new class`, `->class` and `function enum()` are not
//! declarations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The fully qualified names `source` declares.
pub fn declared(source: &str) -> Vec<String> {
    let tokens = tokenize(source);
    let mut found = Vec::new();
    let mut namespace = String::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i].as_str();
        if token.eq_ignore_ascii_case("namespace") {
            // `namespace\foo()` is a relative name, not a declaration.
            if tokens.get(i + 1).map(String::as_str) == Some("\\") {
                i += 1;
                continue;
            }
            let mut name = String::new();
            let mut j = i + 1;
            while let Some(t) = tokens.get(j) {
                if t == ";" || t == "{" {
                    break;
                }
                name.push_str(t);
                j += 1;
            }
            namespace = name;
            i = j;
            continue;
        }
        let is_kind = ["class", "interface", "trait", "enum"]
            .iter()
            .any(|k| token.eq_ignore_ascii_case(k));
        if is_kind {
            let before = i.checked_sub(1).map(|p| tokens[p].as_str());
            let blocked = matches!(before, Some("::" | "->" | "?->" | "\\"))
                || before.is_some_and(|b| {
                    b.eq_ignore_ascii_case("new") || b.eq_ignore_ascii_case("function")
                });
            let name = tokens.get(i + 1).map(String::as_str);
            let after = tokens.get(i + 2).map(String::as_str);
            let follows = after.is_some_and(|a| {
                a == "{"
                    || a == ":"
                    || a.eq_ignore_ascii_case("extends")
                    || a.eq_ignore_ascii_case("implements")
            });
            if !blocked && follows && name.is_some_and(is_identifier) {
                let name = name.unwrap_or_default();
                found.push(if namespace.is_empty() {
                    name.to_owned()
                } else {
                    format!("{namespace}\\{name}")
                });
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    found
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii())
}

/// The code's significant tokens: words (with `\` kept as its own token) and punctuation, nothing else.
fn tokenize(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut in_php = false;
    while i < bytes.len() {
        if !in_php {
            match find(bytes, i, b"<?") {
                Some(at) => {
                    i = at + 2;
                    if bytes[i..].len() >= 3 && bytes[i..i + 3].eq_ignore_ascii_case(b"php") {
                        i += 3;
                    } else if bytes.get(i) == Some(&b'=') {
                        i += 1;
                    }
                    in_php = true;
                }
                None => break,
            }
            continue;
        }
        let b = bytes[i];
        match b {
            b'?' if bytes.get(i + 1) == Some(&b'>') => {
                in_php = false;
                i += 2;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => i = line_end(bytes, i),
            b'#' if bytes.get(i + 1) == Some(&b'[') => {
                // An attribute: skip to its closing bracket.
                i = skip_brackets(bytes, i + 1);
            }
            b'#' => i = line_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = find(bytes, i + 2, b"*/").map_or(bytes.len(), |at| at + 2);
            }
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i),
            b'<' if bytes[i..].starts_with(b"<<<") => i = skip_heredoc(bytes, i),
            b if b.is_ascii_whitespace() => i += 1,
            b if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80)
                {
                    i += 1;
                }
                tokens.push(source[start..i].to_owned());
            }
            b'$' => {
                // A variable is never a declaration keyword; keep it as one token.
                let start = i;
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80)
                {
                    i += 1;
                }
                tokens.push(source[start..i].to_owned());
            }
            b':' if bytes.get(i + 1) == Some(&b':') => {
                tokens.push("::".to_owned());
                i += 2;
            }
            b'-' if bytes.get(i + 1) == Some(&b'>') => {
                tokens.push("->".to_owned());
                i += 2;
            }
            b'?' if bytes[i..].starts_with(b"?->") => {
                tokens.push("?->".to_owned());
                i += 3;
            }
            _ => {
                tokens.push((b as char).to_string());
                i += 1;
            }
        }
    }
    tokens
}

fn find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() && bytes[i] != b'\n' {
        // `?>` ends a line comment too.
        if bytes[i] == b'?' && bytes.get(i + 1) == Some(&b'>') {
            return i;
        }
        i += 1;
    }
    i
}

fn skip_quoted(bytes: &[u8], from: usize) -> usize {
    let quote = bytes[from];
    let mut i = from + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

fn skip_brackets(bytes: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'\'' | b'"' => {
                i = skip_quoted(bytes, i);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// `<<<ID`, `<<<"ID"` or `<<<'ID'`, through the line where `ID` closes it (indented closers included).
fn skip_heredoc(bytes: &[u8], from: usize) -> usize {
    let mut i = from + 3;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let quoted = matches!(bytes.get(i), Some(b'"' | b'\''));
    if quoted {
        i += 1;
    }
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    let id = &bytes[start..i];
    if id.is_empty() {
        return from + 3;
    }
    let mut line = line_end(bytes, i) + 1;
    while line < bytes.len() {
        let mut j = line;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if bytes[j..].starts_with(id) {
            let after = bytes.get(j + id.len());
            if !after.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
                return j + id.len();
            }
        }
        line = line_end(bytes, line) + 1;
    }
    bytes.len()
}

/// The classmap of `paths` (files or directories, scanned for `.php`, `.inc` and `.hh` files), relative to `base`,
/// class name to file. A name declared twice keeps its first file.
pub fn scan(base: &Path, paths: &[PathBuf]) -> BTreeMap<String, PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        collect(&base.join(path), &mut files);
    }
    files.sort();
    let mut map = BTreeMap::new();
    for file in files {
        let Ok(source) = fs::read(&file) else {
            continue;
        };
        let source = String::from_utf8_lossy(&source);
        let relative = file.strip_prefix(base).unwrap_or(&file).to_path_buf();
        for class in declared(&source) {
            map.entry(class).or_insert_with(|| relative.clone());
        }
    }
    map
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.filter_map(Result::ok) {
                collect(&entry.path(), out);
            }
        }
    } else if meta.is_file()
        && path
            .extension()
            .is_some_and(|e| e == "php" || e == "inc" || e == "hh")
    {
        out.push(path.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_declarations_with_their_namespace() {
        let source = r#"<?php
namespace Acme\Http;

use Foo\Bar;

#[Attribute(Attribute::TARGET_CLASS)]
final class Client extends Base implements Thing {}
interface Sender {}
trait Retries {}
enum Method: string { case Get = 'GET'; }
abstract class Shape {}
"#;
        assert_eq!(
            declared(source),
            [
                "Acme\\Http\\Client",
                "Acme\\Http\\Sender",
                "Acme\\Http\\Retries",
                "Acme\\Http\\Method",
                "Acme\\Http\\Shape"
            ]
        );
    }

    #[test]
    fn global_and_braced_namespaces() {
        let source = "<?php\nnamespace A { class One {} }\nnamespace { class Two {} }\n";
        assert_eq!(declared(source), ["A\\One", "Two"]);
        assert_eq!(declared("<?php class Plain {}"), ["Plain"]);
    }

    #[test]
    fn what_is_not_a_declaration() {
        let source = r#"<?php
// class InComment {}
# class InHash {}
/* class InBlock {} */
$a = 'class InSingle {}';
$b = "class InDouble {}";
$c = Foo::class;
$d = new class {};
$e = $obj->class;
$f = <<<EOT
class InHeredoc {}
EOT;
$g = <<<'EOT'
    class InNowdoc {}
    EOT;
function enum() {}
namespace\call();
?>
class InHtml {}
<?php class Real {}
"#;
        assert_eq!(declared(source), ["Real"]);
    }

    #[test]
    fn scans_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/Sub")).unwrap();
        fs::write(
            dir.path().join("src/A.php"),
            "<?php namespace X; class A {}",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/Sub/B.inc"),
            "<?php namespace X\\Sub; interface B {}",
        )
        .unwrap();
        fs::write(dir.path().join("src/notes.txt"), "<?php class Nope {}").unwrap();
        let map = scan(dir.path(), &[PathBuf::from("src")]);
        assert_eq!(map.get("X\\A"), Some(&PathBuf::from("src/A.php")));
        assert_eq!(map.get("X\\Sub\\B"), Some(&PathBuf::from("src/Sub/B.inc")));
        assert_eq!(map.len(), 2);
    }
}
