//! `SpeedySpec\Cli\http_get`: plugins fetch over HTTP through the CLI's own downloader, so they get its root
//! certificates (the static PHP's CA bundle depends on the machine that built it) and its rules for what may be
//! fetched (`https://`, or plain `http://` to this machine; no downgrade on a redirect). Registered in the CLI's
//! process only, never in the runner: an app's task gets no network from the CLI.

use std::io::{self, Write};
use std::rc::Rc;

use speedyspec_fetch::{Fetch, HttpFetch};
use speedyspec_php::NativeArea;
use speedyspec_php::ext_php_rs::builders::FunctionBuilder;
use speedyspec_php::native::{self, NativeError};
use speedyspec_php_ext::Value;

/// The default limit on a body: 8 MiB, more than any API answer a plugin reads.
const DEFAULT_MAX: i64 = 8 * 1024 * 1024;

/// The area that adds `http_get`.
pub struct HttpArea;

/// A buffer that refuses to grow past `max` bytes.
struct Limited {
    body: Vec<u8>,
    max: usize,
    over: bool,
}

impl Write for Limited {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.body.len() + data.len() > self.max {
            self.over = true;
            return Err(io::Error::other("the body is over the limit"));
        }
        self.body.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn http_get(args: &[Value]) -> Result<Value, NativeError> {
    let fail = |message: String| NativeError::throw("RuntimeException", message);
    let Some(Value::Str(url)) = args.first() else {
        return Err(NativeError::throw(
            "TypeError",
            "http_get(): the URL must be a string",
        ));
    };
    let max = match args.get(1) {
        None | Some(Value::Null) => DEFAULT_MAX,
        Some(Value::Int(max)) if *max > 0 => *max,
        Some(_) => {
            return Err(NativeError::throw(
                "ValueError",
                "http_get(): the limit must be a positive int",
            ));
        }
    };
    let mut body = Limited {
        body: Vec::new(),
        max: usize::try_from(max).unwrap_or(usize::MAX),
        over: false,
    };
    match HttpFetch::new().fetch(url, &mut body) {
        Ok(_) => Ok(match String::from_utf8(body.body) {
            Ok(text) => Value::Str(text),
            Err(bytes) => Value::Bytes(bytes.into_bytes()),
        }),
        Err(_) if body.over => Err(fail(format!("{url}: the body is more than {max} bytes"))),
        Err(error) => Err(fail(error.to_string())),
    }
}

impl NativeArea for HttpArea {
    fn functions(&self) -> Vec<FunctionBuilder<'static>> {
        vec![native::function("SpeedySpec\\Cli\\http_get", http_get)]
    }
}

/// The CLI process's own areas: the shared ones and `http_get`.
pub fn cli_areas() -> Vec<Rc<dyn NativeArea>> {
    let mut areas = crate::boot::native_areas();
    areas.push(Rc::new(HttpArea));
    areas
}
