//! Splitting dumps into statements, without a database.

use speedyspec_db::split::{SplitError, Statements};

/// `(line, statement)` pairs, statements as text where they are valid UTF-8.
fn split(sql: &[u8]) -> Result<Vec<(usize, String)>, SplitError> {
    Statements::new(sql)
        .map(|item| item.map(|(line, bytes)| (line, String::from_utf8_lossy(&bytes).into_owned())))
        .collect()
}

#[test]
fn a_semicolon_inside_each_quote_style_does_not_end_a_statement() {
    let got = split(b"INSERT INTO t VALUES ('a;b', \"c;d\");\nSELECT `we;ird` FROM t;\n").unwrap();
    assert_eq!(
        got,
        vec![
            (1, "INSERT INTO t VALUES ('a;b', \"c;d\")".to_owned()),
            (2, "SELECT `we;ird` FROM t".to_owned()),
        ]
    );
}

#[test]
fn escaped_and_doubled_quotes_stay_inside_the_string() {
    let got = split(b"SELECT 'It\\'s; ok', 'It''s; ok', \"say \\\"hi;\\\"\";").unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(
        got[0].1,
        "SELECT 'It\\'s; ok', 'It''s; ok', \"say \\\"hi;\\\"\""
    );
}

#[test]
fn comments_are_dropped_and_conditional_comments_kept() {
    let sql = b"-- a comment; with a semicolon\n# another; one\n/* block; comment */ SELECT 1;\n/*!40101 SET NAMES utf8mb4 */;\nSELECT 1--1;\n";
    let got = split(sql).unwrap();
    assert_eq!(
        got,
        vec![
            (3, "SELECT 1".to_owned()),
            (4, "/*!40101 SET NAMES utf8mb4 */".to_owned()),
            (5, "SELECT 1--1".to_owned()),
        ]
    );
}

#[test]
fn a_statement_is_numbered_by_its_first_line() {
    let got = split(b"\n\nCREATE TABLE t (\n  id int\n);\n\nSELECT 2;").unwrap();
    assert_eq!(got[0].0, 3);
    assert_eq!(got[0].1, "CREATE TABLE t (\n  id int\n)");
    assert_eq!(
        got[1],
        (7, "SELECT 2".to_owned()),
        "a last statement needs no semicolon"
    );
}

#[test]
fn bytes_that_are_not_utf8_are_kept() {
    let sql = b"INSERT INTO t VALUES (_binary '\x00\xff\n\\'');\n";
    let statements: Vec<_> = Statements::new(&sql[..]).collect::<Result<_, _>>().unwrap();
    assert_eq!(statements.len(), 1);
    assert_eq!(
        statements[0].1,
        b"INSERT INTO t VALUES (_binary '\x00\xff\n\\'')".to_vec()
    );
}

#[test]
fn delimiter_is_refused_with_its_line() {
    let error = split(b"SELECT 1;\n\nDELIMITER ;;\nCREATE TRIGGER x;;\n").unwrap_err();
    assert_eq!(error.line, 3);
    assert!(
        error.message.contains("stored routines are not supported"),
        "{error}"
    );
}

#[test]
fn an_unterminated_string_is_an_error_at_its_start() {
    let error = split(b"SELECT 1;\nSELECT 'never closed;\n").unwrap_err();
    assert_eq!(error.line, 2);
    assert!(error.message.contains("unterminated"), "{error}");
}

#[test]
fn a_real_mysqldump_file_splits() {
    let dump = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mysqldump-8.4.sql"
    ))
    .unwrap();
    let statements: Vec<_> = Statements::new(&dump[..])
        .collect::<Result<_, _>>()
        .unwrap();
    let starts: Vec<String> = statements
        .iter()
        .map(|(_, s)| String::from_utf8_lossy(&s[..s.len().min(12)]).into_owned())
        .collect();
    assert!(
        starts
            .iter()
            .filter(|s| s.starts_with("INSERT INTO"))
            .count()
            == 2,
        "{starts:?}"
    );
    assert!(
        starts.iter().any(|s| s.starts_with("LOCK TABLES")),
        "{starts:?}"
    );
    assert!(
        starts.iter().any(|s| s.starts_with("/*!40101")),
        "{starts:?}"
    );
}
