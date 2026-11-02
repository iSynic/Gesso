//! Local IPC between an agent process (the MCP server, the CLI) and a running desktop app: a
//! per-user local socket (a named pipe on Windows), carrying length-prefixed JSON frames. The
//! client opens with a handshake that checks [`PROTOCOL_VERSION`]; after that each frame it sends
//! is a [`Request`] and each frame back is its [`Response`].

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::thread::JoinHandle;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream};
use serde_json::{Value, json};

use crate::{PROTOCOL_VERSION, Request, Response};

/// Largest frame accepted (a whole-page screenshot in base64 fits easily).
const MAX_FRAME: u32 = 256 << 20;

/// The socket name for this user, so two users on one machine don't meet.
pub fn socket_name() -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "user".into());
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    format!("ai-design-engine-{user}.sock")
}

pub fn write_frame(w: &mut impl Write, v: &impl serde::Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(v).map_err(io::Error::other)?;
    let len = u32::try_from(bytes.len()).ok().filter(|&n| n <= MAX_FRAME).ok_or_else(|| io::Error::other("frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

pub fn read_frame<T: serde::de::DeserializeOwned>(r: &mut impl Read) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A connection to a running app.
pub struct Client {
    stream: Stream,
    /// What the app said about itself in the handshake.
    pub server: Value,
}

impl Client {
    /// Connects to the app listening on `name` (default: [`socket_name`]). Fails quickly when no
    /// app is running.
    pub fn connect(name: Option<&str>, client: &str) -> io::Result<Client> {
        let name = name.map_or_else(socket_name, str::to_string);
        let mut stream = Stream::connect(name.to_ns_name::<GenericNamespaced>()?)?;
        write_frame(&mut stream, &json!({ "hello": PROTOCOL_VERSION, "client": client }))?;
        let server: Value = read_frame(&mut stream)?;
        if let Some(e) = server.get("error").and_then(Value::as_str) {
            return Err(io::Error::other(e.to_string()));
        }
        Ok(Client { stream, server })
    }

    pub fn call(&mut self, req: &Request) -> io::Result<Response> {
        write_frame(&mut self.stream, req)?;
        read_frame(&mut self.stream)
    }
}

/// Answers requests from IPC connections, on any thread.
pub type Handler = Arc<dyn Fn(Request) -> Response + Send + Sync>;

/// Listens on `name` (default: [`socket_name`]) on a background thread; each connection gets its
/// own thread and calls `handler` per request. `info` goes back in the handshake. Fails if
/// another app is already listening.
pub fn serve(name: Option<&str>, info: Value, handler: Handler) -> io::Result<JoinHandle<()>> {
    let name = name.map_or_else(socket_name, str::to_string);
    let listener = ListenerOptions::new().name(name.to_ns_name::<GenericNamespaced>()?).create_sync()?;
    std::thread::Builder::new().name("agent-ipc".into()).spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let (handler, info) = (handler.clone(), info.clone());
            let _ = std::thread::Builder::new().name("agent-ipc-conn".into()).spawn(move || connection(conn, &info, &handler));
        }
    })
}

fn connection(mut conn: Stream, info: &Value, handler: &Handler) {
    let Ok(hello) = read_frame::<Value>(&mut conn) else { return };
    let version = hello.get("hello").and_then(Value::as_u64);
    if version != Some(u64::from(PROTOCOL_VERSION)) {
        let msg = format!("protocol version {} is not supported (this app speaks {PROTOCOL_VERSION}); update the client", version.map_or("?".into(), |v| v.to_string()));
        let _ = write_frame(&mut conn, &json!({ "error": msg }));
        return;
    }
    let mut reply = json!({ "hello": PROTOCOL_VERSION });
    if let (Some(r), Value::Object(i)) = (reply.as_object_mut(), info) {
        r.extend(i.clone());
    }
    if write_frame(&mut conn, &reply).is_err() {
        return;
    }
    loop {
        let resp = match read_frame::<Value>(&mut conn) {
            Ok(v) => match serde_json::from_value::<Request>(v) {
                Ok(req) => handler(req),
                Err(e) => Response::err(format!("bad request: {e}")),
            },
            Err(_) => return,
        };
        if write_frame(&mut conn, &resp).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &json!({"a": [1, 2]})).unwrap_or_default();
        let v: Value = read_frame(&mut buf.as_slice()).unwrap_or_default();
        assert_eq!(v, json!({"a": [1, 2]}));
        let mut bad = (MAX_FRAME + 1).to_le_bytes().to_vec();
        bad.extend([0; 8]);
        assert!(read_frame::<Value>(&mut bad.as_slice()).is_err());
    }

    #[test]
    fn serve_and_call() {
        let name = format!("ai-design-engine-test-{}.sock", std::process::id());
        let handler: Handler = Arc::new(|req: Request| Response::ok(json!({ "got": req.name() })));
        let started = serve(Some(&name), json!({ "server": "test" }), handler);
        assert!(started.is_ok(), "{started:?}");
        let mut c = Client::connect(Some(&name), "test").unwrap_or_else(|e| unreachable!("connect: {e}"));
        assert_eq!(c.server["server"], "test");
        for _ in 0..3 {
            let r = c.call(&Request::GetState(crate::Empty {})).unwrap_or_default();
            assert_eq!(r.result, Some(json!({ "got": "get_state" })));
        }
        assert!(Client::connect(Some("ai-design-engine-nobody-here.sock"), "test").is_err());
    }
}
