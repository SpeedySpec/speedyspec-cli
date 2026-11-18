//! A tiny HTTP/1.1 server on the loopback interface, for tests that must not touch the network.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// How a route answers.
#[derive(Clone, Debug)]
pub enum Behaviour {
    Normal,
    /// Declares the full `Content-Length`, sends only this many body bytes, then closes.
    TruncateAfter(usize),
    /// Answers `302` with this `Location`.
    RedirectTo(String),
}

/// What the server sends for one path.
#[derive(Clone, Debug)]
pub struct Route {
    pub status: u16,
    pub body: Vec<u8>,
    pub behaviour: Behaviour,
}

impl Route {
    pub fn ok(body: impl Into<Vec<u8>>) -> Route {
        Route {
            status: 200,
            body: body.into(),
            behaviour: Behaviour::Normal,
        }
    }

    pub fn status(status: u16) -> Route {
        Route {
            status,
            body: Vec::new(),
            behaviour: Behaviour::Normal,
        }
    }

    pub fn redirect(to: impl Into<String>) -> Route {
        Route {
            status: 302,
            body: Vec::new(),
            behaviour: Behaviour::RedirectTo(to.into()),
        }
    }

    pub fn truncated(body: impl Into<Vec<u8>>, after: usize) -> Route {
        Route {
            status: 200,
            body: body.into(),
            behaviour: Behaviour::TruncateAfter(after),
        }
    }
}

/// A request the server received.
#[derive(Clone, Debug)]
pub struct Request {
    pub path: String,
    /// Header names lower-cased.
    pub headers: HashMap<String, String>,
}

/// The running server; it stops when dropped.
pub struct Server {
    address: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Request>>>,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Starts a server answering `routes` (path, including the leading slash, to route). Any
    /// other path is a 404.
    pub fn start(routes: Vec<(&str, Route)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let address = listener.local_addr().expect("local address").to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::new(Mutex::new(
            routes
                .into_iter()
                .map(|(path, route)| (path.to_owned(), route))
                .collect(),
        ));
        let thread = {
            let (stop, requests, routes) = (stop.clone(), requests.clone(), routes.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        // A broken connection is the client's business (some tests cause it).
                        let _ = serve(stream, &routes, &requests);
                    }
                }
            })
        };
        Server {
            address,
            stop,
            requests,
            routes,
            thread: Some(thread),
        }
    }

    /// `http://127.0.0.1:<port>`, without a trailing slash.
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Adds or replaces a route while the server runs (for routes that must mention its URL).
    pub fn route(&self, path: &str, route: Route) {
        self.routes
            .lock()
            .expect("routes lock")
            .insert(path.to_owned(), route);
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept loop.
        let _ = TcpStream::connect(&self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    mut stream: TcpStream,
    routes: &Mutex<HashMap<String, Route>>,
    requests: &Mutex<Vec<Request>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
    if path.is_empty() {
        return Ok(());
    }
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    requests.lock().expect("requests lock").push(Request {
        path: path.clone(),
        headers,
    });

    let route = routes.lock().expect("routes lock").get(&path).cloned();
    let Some(route) = route else {
        return stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    };
    let mut head = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        route.status,
        route.body.len()
    );
    if let Behaviour::RedirectTo(to) = &route.behaviour {
        head.push_str(&format!("Location: {to}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    let body = match route.behaviour {
        Behaviour::TruncateAfter(n) => &route.body[..n.min(route.body.len())],
        _ => &route.body[..],
    };
    stream.write_all(body)?;
    stream.flush()
}
