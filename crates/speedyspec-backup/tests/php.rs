//! The serialized search and replace against PHP itself: values PHP's own `serialize()` wrote are rewritten in Rust,
//! and PHP's `unserialize()` must read every one, with the URL changed and nothing else.
//!
//! PHP boots once per process, so this test has its own `main()` (`harness = false`).

use std::rc::Rc;

use speedyspec_backup::serialized::replace;
use speedyspec_hooks::Value;
use speedyspec_hooks_memory::MemoryHooks;
use speedyspec_php::{BootOptions, Php};

fn bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Bytes(b) => b.clone(),
        other => panic!("expected a string, got {other:?}"),
    }
}

fn main() {
    let php = Php::boot(BootOptions {
        hooks: Rc::new(MemoryHooks::new()),
        preload: &[],
        areas: Vec::new(),
    })
    .expect("PHP boots");

    // Each case: PHP code returning the value, and PHP code checking the unserialized `$v` (true when right).
    let cases: &[(&str, &str, &str)] = &[
        (
            "string",
            "'https://old.example/path'",
            "$v === 'https://new.example.org/path'",
        ),
        (
            "array",
            "['siteurl' => 'https://old.example', 'n' => 3, 'list' => ['https://old.example/a', null, true, 1.5]]",
            "$v['siteurl'] === 'https://new.example.org' && $v['n'] === 3 && $v['list'][0] === 'https://new.example.org/a' && $v['list'][1] === null && $v['list'][2] === true && $v['list'][3] === 1.5",
        ),
        (
            "object with a reference",
            "(function () { $o = new stdClass(); $o->url = 'https://old.example'; $o->same = $o; return $o; })()",
            "$v->url === 'https://new.example.org' && $v->same === $v",
        ),
        (
            "nested serialization",
            "serialize(['inner' => 'https://old.example'])",
            "unserialize($v)['inner'] === 'https://new.example.org'",
        ),
        (
            "multibyte",
            "'é ü https://old.example ✓'",
            "$v === 'é ü https://new.example.org ✓'",
        ),
        (
            "widget-like options",
            "[2 => ['title' => 'Links', 'text' => '<a href=\"https://old.example/x\">x</a>'], '_multiwidget' => 1]",
            "$v[2]['text'] === '<a href=\"https://new.example.org/x\">x</a>' && $v['_multiwidget'] === 1",
        ),
    ];

    let mut failed = false;
    for (name, make, check) in cases {
        let serialized = php
            .eval("make", &format!("return serialize({make});"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let original = bytes(&serialized);
        let rewritten = replace(
            &original,
            b"https://old.example",
            b"https://new.example.org",
        );
        let checker = php
            .eval(
                "check",
                &format!(
                    "return static function (string $s): string {{ $v = @unserialize($s); if ($v === false) {{ return 'unserialize failed'; }} return ({check}) ? 'ok' : 'wrong: ' . var_export($v, true); }};"
                ),
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let text = match String::from_utf8(rewritten.into_owned()) {
            Ok(s) => Value::Str(s),
            Err(e) => Value::Bytes(e.into_bytes()),
        };
        let verdict = php
            .call(&checker, &[text])
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        if verdict == Value::Str("ok".into()) {
            println!("ok: {name}");
        } else {
            eprintln!("FAIL: {name}: {verdict:?}");
            failed = true;
        }
    }
    drop(php);
    if failed {
        std::process::exit(1);
    }
}
