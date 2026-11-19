//! Moving a dump to a new table prefix and site URL as it streams.
//!
//! The dump is the one `speedyspec_db::Connection::export` writes: `DROP TABLE IF EXISTS`, `CREATE TABLE` and
//! `INSERT INTO <table> (<columns>) VALUES (…),(…)`, with strings in single quotes and backslash escapes, `X'…'` for
//! binary columns and bare numbers. Each statement is rewritten on its own, so a dump of any size streams through:
//! the table prefix is renamed in table names (once, at the start), values of the prefixed columns that start with
//! the old prefix get the new one, and the old site URL is replaced in every string, serialized values kept valid.

use std::borrow::Cow;

use crate::serialized;

/// What a restore changes in the dump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    pub from_prefix: String,
    pub to_prefix: String,
    /// The old and the new site URL.
    pub urls: Option<(String, String)>,
    /// `(table without prefix, column, keys)` whose values carry the prefix: with `keys`, only the prefix followed by
    /// exactly one of them; without, every value that starts with the prefix.
    pub prefixed_columns: Vec<(String, String, Option<Vec<String>>)>,
}

impl Move {
    pub fn changes_nothing(&self) -> bool {
        self.from_prefix == self.to_prefix && self.urls.as_ref().is_none_or(|(old, new)| old == new)
    }
}

/// `statement` as the move leaves it; warnings name tables without the old prefix. Unchanged statements are borrowed.
pub fn rewrite<'a>(statement: &'a [u8], plan: &Move, warnings: &mut Vec<String>) -> Cow<'a, [u8]> {
    if plan.changes_nothing() {
        return Cow::Borrowed(statement);
    }
    match Rewriter::new(statement, plan, warnings).run() {
        Some(out) if out != statement => Cow::Owned(out),
        Some(_) => Cow::Borrowed(statement),
        None => {
            // A statement of a kind the move rewrites, in a form it cannot read, runs unmoved: say so.
            let text = String::from_utf8_lossy(statement);
            let start = text.trim_start();
            let kind = start
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            if matches!(kind.as_str(), "INSERT" | "CREATE" | "DROP") {
                let shown: String = start.chars().take(60).collect();
                warnings.push(format!(
                    "the statement {shown:?}… could not be read for the move; it runs as it is"
                ));
            }
            Cow::Borrowed(statement)
        }
    }
}

/// MySQL's escaping of a string literal, as `speedyspec_db` writes it.
fn quote(text: &[u8], out: &mut Vec<u8>) {
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

struct Rewriter<'a, 'w> {
    input: &'a [u8],
    pos: usize,
    out: Vec<u8>,
    plan: &'a Move,
    warnings: &'w mut Vec<String>,
}

impl<'a, 'w> Rewriter<'a, 'w> {
    fn new(input: &'a [u8], plan: &'a Move, warnings: &'w mut Vec<String>) -> Self {
        Rewriter {
            input,
            pos: 0,
            out: Vec::with_capacity(input.len()),
            plan,
            warnings,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    /// Copies whitespace and `--` / `#` comment lines.
    fn copy_space(&mut self) {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => {
                    self.out.push(b);
                    self.pos += 1;
                }
                Some(b'#') => self.copy_line(),
                Some(b'-') if self.input.get(self.pos + 1) == Some(&b'-') => self.copy_line(),
                _ => return,
            }
        }
    }

    fn copy_line(&mut self) {
        while let Some(b) = self.peek() {
            self.out.push(b);
            self.pos += 1;
            if b == b'\n' {
                return;
            }
        }
    }

    /// Copies `words` (case-insensitive, single spaces between them in the pattern) if they come next.
    fn copy_words(&mut self, words: &[&str]) -> bool {
        let start = (self.pos, self.out.len());
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                let before = self.pos;
                self.copy_space();
                if self.pos == before {
                    self.restore(start);
                    return false;
                }
            }
            let end = self.pos + word.len();
            match self.input.get(self.pos..end) {
                Some(found) if found.eq_ignore_ascii_case(word.as_bytes()) => {
                    self.out.extend_from_slice(found);
                    self.pos = end;
                }
                _ => {
                    self.restore(start);
                    return false;
                }
            }
        }
        true
    }

    fn restore(&mut self, (pos, len): (usize, usize)) {
        self.pos = pos;
        self.out.truncate(len);
    }

    /// A backquoted identifier, unquoted; not copied.
    fn identifier(&mut self) -> Option<String> {
        if self.peek()? != b'`' {
            return None;
        }
        let mut name = Vec::new();
        self.pos += 1;
        loop {
            let b = self.peek()?;
            self.pos += 1;
            if b == b'`' {
                if self.peek() == Some(b'`') {
                    name.push(b'`');
                    self.pos += 1;
                } else {
                    return String::from_utf8(name).ok();
                }
            } else {
                name.push(b);
            }
        }
    }

    fn push_identifier(&mut self, name: &str) {
        self.out.push(b'`');
        self.out
            .extend_from_slice(name.replace('`', "``").as_bytes());
        self.out.push(b'`');
    }

    /// The table name, renamed; gives back the name without the prefix (for the prefixed columns).
    fn table(&mut self) -> Option<Option<String>> {
        let name = self.identifier()?;
        match name.strip_prefix(&self.plan.from_prefix) {
            Some(rest) => {
                let renamed = format!("{}{rest}", self.plan.to_prefix);
                self.push_identifier(&renamed);
                Some(Some(rest.to_owned()))
            }
            None => {
                let warning = format!(
                    "table {name} does not start with the prefix {}; restored under its own name",
                    self.plan.from_prefix
                );
                if !self.warnings.contains(&warning) {
                    self.warnings.push(warning);
                }
                self.push_identifier(&name);
                Some(None)
            }
        }
    }

    fn copy_rest(&mut self) {
        self.out.extend_from_slice(&self.input[self.pos..]);
        self.pos = self.input.len();
    }

    fn run(mut self) -> Option<Vec<u8>> {
        self.copy_space();
        if self.copy_words(&["DROP", "TABLE", "IF", "EXISTS"]) {
            self.copy_space();
            self.table()?;
            self.copy_rest();
            return Some(self.out);
        }
        if self.copy_words(&["CREATE", "TABLE"]) {
            self.copy_space();
            self.table()?;
            self.definitions()?;
            return Some(self.out);
        }
        if !self.copy_words(&["INSERT", "INTO"]) {
            return None;
        }
        self.copy_space();
        let base = self.table()?;
        self.copy_space();
        // The column list.
        if self.peek()? != b'(' {
            return None;
        }
        self.out.push(b'(');
        self.pos += 1;
        let mut columns = Vec::new();
        loop {
            self.copy_space();
            let column = self.identifier()?;
            self.push_identifier(&column);
            columns.push(column);
            self.copy_space();
            match self.peek()? {
                b',' => {
                    self.out.push(b',');
                    self.pos += 1;
                }
                b')' => {
                    self.out.push(b')');
                    self.pos += 1;
                    break;
                }
                _ => return None,
            }
        }
        // For each column: `None` when it is plain, or its keys (`Some(None)` for every prefixed value).
        let prefixed: Vec<Option<Option<Vec<String>>>> = columns
            .iter()
            .map(|column| {
                base.as_ref().and_then(|base| {
                    self.plan
                        .prefixed_columns
                        .iter()
                        .find(|(table, c, _)| table == base && c == column)
                        .map(|(_, _, keys)| keys.clone())
                })
            })
            .collect();
        self.copy_space();
        if !self.copy_words(&["VALUES"]) {
            return None;
        }
        // The rows.
        loop {
            self.copy_space();
            if self.peek()? != b'(' {
                return None;
            }
            self.out.push(b'(');
            self.pos += 1;
            let mut index = 0;
            loop {
                self.copy_space();
                self.value(prefixed.get(index).and_then(Option::as_ref))?;
                index += 1;
                self.copy_space();
                match self.peek()? {
                    b',' => {
                        self.out.push(b',');
                        self.pos += 1;
                    }
                    b')' => {
                        self.out.push(b')');
                        self.pos += 1;
                        break;
                    }
                    _ => return None,
                }
            }
            self.copy_space();
            match self.peek() {
                Some(b',') => {
                    self.out.push(b',');
                    self.pos += 1;
                }
                Some(b';') | None => {
                    self.copy_rest();
                    return Some(self.out);
                }
                _ => return None,
            }
        }
    }

    /// The rest of a `CREATE TABLE`: the identifier after `REFERENCES` (another table) or `CONSTRAINT` (a name that is
    /// unique in the database) gets the new prefix when it has the old one. Column and index names are left alone,
    /// and string literals (`COMMENT '…'`, `DEFAULT '…'`) are copied as they are.
    fn definitions(&mut self) -> Option<()> {
        let mut last_word = String::new();
        while let Some(b) = self.peek() {
            match b {
                b'\'' | b'"' => {
                    let start = self.pos;
                    self.pos += 1;
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == b'\\' {
                            self.pos += 1;
                        } else if c == b {
                            break;
                        }
                    }
                    self.pos = self.pos.min(self.input.len());
                    self.out.extend_from_slice(&self.input[start..self.pos]);
                    last_word.clear();
                }
                b'`' => {
                    let name = self.identifier()?;
                    let after_keyword = last_word.eq_ignore_ascii_case("REFERENCES")
                        || last_word.eq_ignore_ascii_case("CONSTRAINT");
                    match name.strip_prefix(&self.plan.from_prefix) {
                        Some(rest) if after_keyword => {
                            let renamed = format!("{}{rest}", self.plan.to_prefix);
                            self.push_identifier(&renamed);
                        }
                        _ => self.push_identifier(&name),
                    }
                    last_word.clear();
                }
                b if b.is_ascii_alphabetic() => {
                    let start = self.pos;
                    while self
                        .peek()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
                    {
                        self.pos += 1;
                    }
                    let word = &self.input[start..self.pos];
                    self.out.extend_from_slice(word);
                    last_word = String::from_utf8_lossy(word).into_owned();
                }
                b if b.is_ascii_whitespace() => {
                    self.out.push(b);
                    self.pos += 1;
                }
                other => {
                    self.out.push(other);
                    self.pos += 1;
                    last_word.clear();
                }
            }
        }
        Some(())
    }

    /// One value of a row.
    /// `prefixed` is `None` for a plain column, or the column's keys (`Some(&None)` for every prefixed value).
    fn value(&mut self, prefixed: Option<&Option<Vec<String>>>) -> Option<()> {
        match self.peek()? {
            b'\'' => {
                let text = self.string()?;
                let mut text: Cow<'_, [u8]> = Cow::Owned(text);
                if let Some(keys) = prefixed
                    && let Some(rest) = text.strip_prefix(self.plan.from_prefix.as_bytes())
                    && keys
                        .as_ref()
                        .is_none_or(|keys| keys.iter().any(|k| k.as_bytes() == rest))
                {
                    let mut renamed = self.plan.to_prefix.as_bytes().to_vec();
                    renamed.extend_from_slice(rest);
                    text = Cow::Owned(renamed);
                }
                if let Some((old, new)) = &self.plan.urls {
                    let replaced =
                        serialized::replace(&text, old.as_bytes(), new.as_bytes()).into_owned();
                    text = Cow::Owned(replaced);
                }
                quote(&text, &mut self.out);
                Some(())
            }
            b'X' | b'x' if self.input.get(self.pos + 1) == Some(&b'\'') => {
                let start = self.pos;
                self.pos += 2;
                while self.peek()?.is_ascii_hexdigit() {
                    self.pos += 1;
                }
                if self.peek()? != b'\'' {
                    return None;
                }
                self.pos += 1;
                self.out.extend_from_slice(&self.input[start..self.pos]);
                Some(())
            }
            _ => {
                // NULL or a number: up to the next separator.
                let start = self.pos;
                while let Some(b) = self.peek() {
                    if b == b',' || b == b')' || b.is_ascii_whitespace() {
                        break;
                    }
                    if !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.')) {
                        return None;
                    }
                    self.pos += 1;
                }
                if self.pos == start {
                    return None;
                }
                self.out.extend_from_slice(&self.input[start..self.pos]);
                Some(())
            }
        }
    }

    /// A single-quoted string, unescaped.
    fn string(&mut self) -> Option<Vec<u8>> {
        self.pos += 1;
        let mut text = Vec::new();
        loop {
            let b = self.peek()?;
            self.pos += 1;
            match b {
                b'\\' => {
                    let e = self.peek()?;
                    self.pos += 1;
                    text.push(match e {
                        b'0' => 0,
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'b' => 0x08,
                        b'Z' => 0x1a,
                        other => other,
                    });
                }
                b'\'' if self.peek() == Some(b'\'') => {
                    text.push(b'\'');
                    self.pos += 1;
                }
                b'\'' => return Some(text),
                other => text.push(other),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Move {
        Move {
            from_prefix: "wp_".into(),
            to_prefix: "site_".into(),
            urls: Some((
                "https://old.example".into(),
                "https://new.example.org".into(),
            )),
            prefixed_columns: vec![
                (
                    "options".into(),
                    "option_name".into(),
                    Some(vec!["user_roles".into()]),
                ),
                ("usermeta".into(), "meta_key".into(), None),
            ],
        }
    }

    fn run(statement: &str) -> (String, Vec<String>) {
        let mut warnings = Vec::new();
        let out = rewrite(statement.as_bytes(), &plan(), &mut warnings);
        (String::from_utf8(out.into_owned()).unwrap(), warnings)
    }

    #[test]
    fn renames_tables_once_at_the_start() {
        assert_eq!(
            run("DROP TABLE IF EXISTS `wp_options`").0,
            "DROP TABLE IF EXISTS `site_options`"
        );
        assert_eq!(
            run("CREATE TABLE `wp_blog_wp_x` (\n  `id` int NOT NULL\n)").0,
            "CREATE TABLE `site_blog_wp_x` (\n  `id` int NOT NULL\n)"
        );
    }

    #[test]
    fn foreign_keys_follow_the_prefix_but_columns_do_not() {
        let (out, _) = run(
            "CREATE TABLE `wp_download_log` (\n  `wp_id` int,\n  `post` int COMMENT 'REFERENCES `wp_posts`',\n  \
             CONSTRAINT `wp_fk_post` FOREIGN KEY (`post`) REFERENCES `wp_posts` (`ID`)\n)",
        );
        assert_eq!(
            out,
            "CREATE TABLE `site_download_log` (\n  `wp_id` int,\n  `post` int COMMENT 'REFERENCES `wp_posts`',\n  \
             CONSTRAINT `site_fk_post` FOREIGN KEY (`post`) REFERENCES `site_posts` (`ID`)\n)"
        );
    }

    #[test]
    fn a_table_without_the_prefix_is_left_and_warned_about() {
        let (out, warnings) = run("DROP TABLE IF EXISTS `other_table`");
        assert_eq!(out, "DROP TABLE IF EXISTS `other_table`");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("other_table"), "{warnings:?}");
    }

    #[test]
    fn prefixed_columns_and_urls_in_values() {
        let (out, _) = run(
            "INSERT INTO `wp_options` (`option_id`,`option_name`,`option_value`) VALUES\n\
             (1,'wp_user_roles','a:0:{}'),\n\
             (2,'wpx_thing','x'),\n\
             (3,'siteurl','https://old.example'),\n\
             (4,'widget','a:1:{s:3:\\\"url\\\";s:19:\\\"https://old.example\\\";}')",
        );
        assert_eq!(
            out,
            "INSERT INTO `site_options` (`option_id`,`option_name`,`option_value`) VALUES\n\
             (1,'site_user_roles','a:0:{}'),\n\
             (2,'wpx_thing','x'),\n\
             (3,'siteurl','https://new.example.org'),\n\
             (4,'widget','a:1:{s:3:\\\"url\\\";s:23:\\\"https://new.example.org\\\";}')"
        );
    }

    #[test]
    fn named_keys_are_the_only_ones_renamed() {
        let (out, _) = run(
            "INSERT INTO `wp_options` (`option_name`,`option_value`) VALUES\n\
             ('wp_user_roles','a:0:{}'),('wp_page_for_privacy_policy','3'),('wp_user_roles_extra','x')",
        );
        assert_eq!(
            out,
            "INSERT INTO `site_options` (`option_name`,`option_value`) VALUES\n\
             ('site_user_roles','a:0:{}'),('wp_page_for_privacy_policy','3'),('wp_user_roles_extra','x')"
        );
        let (out, _) = run(
            "INSERT INTO `wp_usermeta` (`meta_key`) VALUES\n('wp_capabilities'),('wp_anything')",
        );
        assert_eq!(
            out,
            "INSERT INTO `site_usermeta` (`meta_key`) VALUES\n('site_capabilities'),('site_anything')"
        );
    }

    #[test]
    fn the_prefix_only_changes_in_prefixed_columns() {
        let (out, _) = run(
            "INSERT INTO `wp_posts` (`ID`,`post_title`) VALUES\n(1,'wp_user_roles in a title')",
        );
        assert_eq!(
            out,
            "INSERT INTO `site_posts` (`ID`,`post_title`) VALUES\n(1,'wp_user_roles in a title')"
        );
    }

    #[test]
    fn escapes_survive() {
        let (out, _) = run(
            "INSERT INTO `wp_posts` (`a`,`b`) VALUES\n('it\\'s \\\\ back\\0nul\\nline https://old.example','x')",
        );
        assert_eq!(
            out,
            "INSERT INTO `site_posts` (`a`,`b`) VALUES\n('it\\'s \\\\ back\\0nul\\nline https://new.example.org','x')"
        );
    }

    #[test]
    fn nulls_numbers_and_binary_pass_through() {
        let (out, _) = run(
            "INSERT INTO `wp_t` (`a`,`b`,`c`,`d`) VALUES\n(NULL,-1.5e3,X'68747470733a2f2f6f6c64',0)",
        );
        assert_eq!(
            out,
            "INSERT INTO `site_t` (`a`,`b`,`c`,`d`) VALUES\n(NULL,-1.5e3,X'68747470733a2f2f6f6c64',0)"
        );
    }

    #[test]
    fn other_statements_pass_through_and_a_no_op_borrows() {
        let mut warnings = Vec::new();
        let set = b"SET NAMES utf8mb4";
        assert!(matches!(
            rewrite(set, &plan(), &mut warnings),
            Cow::Borrowed(_)
        ));
        let none = Move {
            from_prefix: "wp_".into(),
            to_prefix: "wp_".into(),
            urls: None,
            prefixed_columns: Vec::new(),
        };
        let insert = b"INSERT INTO `wp_t` (`a`) VALUES\n('x')";
        assert!(matches!(
            rewrite(insert, &none, &mut warnings),
            Cow::Borrowed(_)
        ));
        assert!(none.changes_nothing());
    }

    #[test]
    fn a_statement_the_move_cannot_read_is_warned_about() {
        let (out, warnings) = run("INSERT INTO `wp_t` VALUES (1, 'https://old.example')");
        assert_eq!(out, "INSERT INTO `wp_t` VALUES (1, 'https://old.example')");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("could not be read for the move")),
            "{warnings:?}"
        );
    }

    #[test]
    fn leading_comments_are_kept() {
        let (out, _) = run("-- a comment\nDROP TABLE IF EXISTS `wp_options`");
        assert_eq!(out, "-- a comment\nDROP TABLE IF EXISTS `site_options`");
    }
}
