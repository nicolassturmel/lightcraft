//! The browser is a view/controller of this process's existing UI Session. All library, decode,
//! render, Vision and persistence work stays on the desktop. No local browser engine or originals.
use lightcraft_ui_egui::{ControlRequest, remote::DesktopFrame};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
    },
    time::Duration,
};

const MAX_HEAD: usize = 16384;
const MAX_BODY: usize = 65536;
const MAX_JSON: usize = 2 * 1024 * 1024;
const MAX_IMAGE: usize = 16 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;
const COOKIE: &str = "lightcraft_view";
const HTML: &[u8] = include_bytes!("../web/index.html");
const JS: &[u8] = include_bytes!("../web/view.js");
const CSS: &[u8] = include_bytes!("../web/view.css");

#[derive(Clone, Copy)]
pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
}
struct State {
    token: String,
    tx: Sender<ControlRequest>,
    ctx: egui::Context,
    frame: Mutex<()>,
    share_origin: String,
}
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
struct Response {
    code: u16,
    mime: &'static str,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}
impl Response {
    fn bytes(code: u16, mime: &'static str, body: &[u8]) -> Self {
        Self { code, mime, body: body.to_vec(), headers: vec![] }
    }
    fn json(code: u16, value: Value) -> Self {
        let mut output = Limited { bytes: vec![], cap: MAX_JSON };
        if serde_json::to_writer(&mut output, &value).is_err() {
            return Self::bytes(413, "application/json", b"{\"ok\":false,\"error\":\"response exceeds the Web view limit\"}");
        }
        Self { code, mime: "application/json", body: output.bytes, headers: vec![] }
    }
    fn error(code: u16, message: impl Into<String>) -> Self {
        Self::json(code, json!({"ok":false,"error":message.into()}))
    }
    fn cookie(mut self, token: &str) -> Self {
        self.headers.push(("Set-Cookie".into(), format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict")));
        self
    }
}
struct Limited {
    bytes: Vec<u8>,
    cap: usize,
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.cap.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("bounded response is full"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn start(config: Config, tx: Sender<ControlRequest>, ctx: egui::Context) -> Result<String, String> {
    let listener = TcpListener::bind(SocketAddr::new(config.bind, config.port))
        .map_err(|e| format!("Web view cannot listen on {}:{}: {e}", config.bind, config.port))?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let mut secret = [0_u8; 32];
    getrandom::fill(&mut secret).map_err(|e| format!("Web session key unavailable: {e}"))?;
    let token = secret.iter().map(|v| format!("{v:02x}")).collect();
    let host = if address.ip().is_unspecified() { network_address().unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)) } else { address.ip() };
    let share_origin = format!("http://{}", SocketAddr::new(host, address.port()));
    let state = Arc::new(State { token, tx, ctx, frame: Mutex::new(()), share_origin });
    log::info!("desktop Web view listening on {address}; local view: http://127.0.0.1:{}/", address.port());
    std::thread::spawn(move || accept(listener, state));
    Ok(format!("http://127.0.0.1:{}/", address.port()))
}
fn network_address() -> Option<IpAddr> {
    // UDP connect only chooses a local route; no packet or photo is sent.
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip())
}
struct Connection(Arc<AtomicUsize>);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
fn accept(listener: TcpListener, state: Arc<State>) {
    let count = Arc::new(AtomicUsize::new(0));
    for mut stream in listener.incoming().flatten() {
        if count.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            count.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = write_response(&mut stream, Response::error(503, "too many Web connections"));
            continue;
        }
        let connection = Connection(count.clone());
        let state = state.clone();
        std::thread::spawn(move || {
            let _connection = connection;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
            let response = match stream.peer_addr() {
                Ok(peer) => match read_request(&mut stream) {
                    Ok(request) => route(request, peer, &state),
                    Err(e) => Response::error(400, e),
                },
                Err(e) => Response::error(400, e.to_string()),
            };
            let _ = write_response(&mut stream, response);
        });
    }
}

fn header_line(reader: &mut impl BufRead, total: &mut usize) -> Result<String, String> {
    let mut line = Vec::new();
    reader.take((MAX_HEAD.saturating_sub(*total) + 1) as u64).read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
    *total = total.saturating_add(line.len());
    if *total > MAX_HEAD || !line.ends_with(b"\r\n") {
        return Err("invalid or oversized HTTP headers".into());
    }
    std::str::from_utf8(line.strip_suffix(b"\r\n").ok_or("invalid HTTP line")?).map(str::to_string).map_err(|_| "invalid HTTP header encoding".into())
}

fn read_request(input: &mut impl Read) -> Result<Request, String> {
    let mut reader = BufReader::new(input);
    let mut total = 0_usize;
    let first = header_line(&mut reader, &mut total)?;
    let parts: Vec<_> = first.split_ascii_whitespace().collect();
    if parts.len() != 3 || parts.get(2) != Some(&"HTTP/1.1") {
        return Err("expected HTTP/1.1".into());
    }
    let method = parts.first().copied().ok_or("missing HTTP method")?.to_string();
    let target = parts.get(1).copied().filter(|v| v.starts_with('/') && v.len() <= 2048).ok_or("invalid HTTP target")?;
    let path = target.split('?').next().unwrap_or(target).to_string();
    let mut headers = BTreeMap::new();
    loop {
        let line = header_line(&mut reader, &mut total)?;
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or("invalid HTTP header")?;
        if !name.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'-') || name.is_empty() || value.bytes().any(|v| v < 32 && v != b'\t' || v == 127)
        {
            return Err("invalid HTTP header".into());
        }
        if headers.len() >= 64 || headers.insert(name.to_ascii_lowercase(), value.trim().to_string()).is_some() {
            return Err("duplicate or excessive HTTP headers".into());
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err("chunked requests are unsupported".into());
    }
    let size = headers.get("content-length").map(|v| v.parse::<usize>().map_err(|_| "invalid content length")).transpose()?.unwrap_or(0);
    if size > MAX_BODY {
        return Err("HTTP body exceeds 64 KiB".into());
    }
    let mut body = vec![0; size];
    reader.read_exact(&mut body).map_err(|_| "truncated HTTP body")?;
    Ok(Request { method, path, headers, body })
}

fn host(request: &Request) -> Option<&str> {
    let value = request.headers.get("host")?.as_str();
    if value.is_empty() || value.len() > 255 || !value.bytes().all(|v| v.is_ascii_alphanumeric() || b".:-[]".contains(&v)) {
        return None;
    }
    // Only a literal IP or localhost: arbitrary DNS names cannot rebind to the Mac.
    let name = if value.starts_with('[') { value.strip_prefix('[')?.split(']').next()? } else { value.split(':').next()? };
    if name == "localhost" || name.parse::<IpAddr>().is_ok() { Some(value) } else { None }
}
fn same_origin(request: &Request, authority: &str) -> bool {
    if request.headers.get("sec-fetch-site").is_some_and(|v| v != "same-origin" && v != "none") {
        return false;
    }
    request.headers.get("origin").is_none_or(|v| v == &format!("http://{authority}"))
}
fn equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0_u8, |difference, (a, b)| difference | (a ^ b)) == 0
}
fn authorized(request: &Request, state: &State) -> bool {
    let cookie = request
        .headers
        .get("cookie")
        .and_then(|v| v.split(';').find_map(|p| p.trim().split_once('=').filter(|(name, _)| *name == COOKIE).map(|(_, value)| value)));
    let bearer = request.headers.get("authorization").and_then(|v| v.strip_prefix("Bearer "));
    cookie.into_iter().chain(bearer).any(|v| equal(v, &state.token))
}
fn local_bootstrap(request: &Request, peer: SocketAddr, authority: &str) -> bool {
    if !peer.ip().is_loopback() {
        return false;
    }
    (authority.split(':').next().is_some_and(|v| v == "127.0.0.1" || v == "localhost") || authority.starts_with("[::1]"))
        && request.headers.get("sec-fetch-site").is_none_or(|v| v == "same-origin" || v == "none")
}
fn route(request: Request, peer: SocketAddr, state: &State) -> Response {
    let Some(authority) = host(&request) else {
        return Response::error(403, "use the Mac's IP address or localhost");
    };
    let public_asset = request.method == "GET" && matches!(request.path.as_str(), "/" | "/view.js" | "/view.css" | "/favicon.ico");
    if !public_asset && !same_origin(&request, authority) {
        return Response::error(403, "cross-origin desktop requests are refused");
    }
    if request.method == "GET" {
        match request.path.as_str() {
            "/" => {
                let response = Response::bytes(200, "text/html; charset=utf-8", HTML);
                return if local_bootstrap(&request, peer, authority) { response.cookie(&state.token) } else { response };
            }
            "/view.js" => return Response::bytes(200, "text/javascript; charset=utf-8", JS),
            "/view.css" => return Response::bytes(200, "text/css; charset=utf-8", CSS),
            "/favicon.ico" => return Response::bytes(204, "image/x-icon", b""),
            "/api/connect" if local_bootstrap(&request, peer, authority) || authorized(&request, state) => {
                return Response::json(200, json!({"ok":true,"mode":"desktop-view"})).cookie(&state.token);
            }
            _ => {}
        }
    }
    if !authorized(&request, state) {
        return Response::error(401, "Open the view on the Mac, or use its shared connection link.");
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/info") => Response::json(
            200,
            json!({"ok":true,"mode":"desktop-view","engine":"desktop","library":"desktop",
            "shareUrl":format!("{}/#key={}",state.share_origin,state.token)}),
        ),
        ("GET", "/api/frame") => frame(state),
        ("POST", "/api/rpc") => {
            if !request.headers.contains_key("origin") {
                return Response::error(403, "desktop actions require their browser origin");
            }
            if request.headers.get("content-type").is_none_or(|v| !v.split(';').next().is_some_and(|v| v.trim() == "application/json")) {
                return Response::error(415, "use application/json");
            }
            let body: Value = match serde_json::from_slice(&request.body) {
                Ok(value) => value,
                Err(_) => return Response::error(400, "invalid desktop request JSON"),
            };
            let Some(method) = body.get("method").and_then(Value::as_str) else {
                return Response::error(400, "missing desktop method");
            };
            if !matches!(
                method,
                "ui.webInput"
                    | "ui.inspect"
                    | "ui.widgets"
                    | "engine.execute"
                    | "engine.commands"
                    | "ui.menu.invoke"
                    | "ui.set"
                    | "ui.clickWidget"
                    | "ui.clipboard"
            ) {
                return Response::error(403, "this method is not exposed by the browser view");
            }
            let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Response::error(400, "desktop parameters must be an object");
            }
            match call(state, method, params) {
                Ok(value) => Response::json(200, value),
                Err(e) => Response::error(504, e),
            }
        }
        _ => Response::error(404, "unknown Web view endpoint"),
    }
}
fn call(state: &State, method: &str, params: Value) -> Result<Value, String> {
    let (request, reply) = ControlRequest::new(method, params);
    state.tx.send(request).map_err(|_| "the desktop has closed")?;
    state.ctx.request_repaint();
    reply.recv_timeout(Duration::from_secs(15)).map_err(|_| "the desktop did not answer; check the Mac window".into())
}
fn frame(state: &State) -> Response {
    let _guard = match state.frame.try_lock() {
        Ok(guard) => guard,
        Err(_) => return Response::error(503, "a desktop preview is being prepared"),
    };
    let (request, reply) = ControlRequest::desktop_frame();
    if state.tx.send(request).is_err() {
        return Response::error(503, "the desktop has closed");
    }
    state.ctx.request_repaint();
    let frame = match reply.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(frame)) => frame,
        Ok(Err(e)) => return Response::error(503, e),
        Err(_) => return Response::error(504, "desktop preview timed out"),
    };
    match encode_frame(frame) {
        Ok(response) => response,
        Err(e) => Response::error(503, e),
    }
}
fn encode_frame(frame: DesktopFrame) -> Result<Response, String> {
    let [w, h] = frame.image.size;
    let count = w
        .checked_mul(h)
        .filter(|n| w > 0 && h > 0 && w <= 1922 && h <= 1922 && *n == frame.image.pixels.len())
        .ok_or("invalid desktop preview size")?;
    if frame.viewport.iter().any(|v| !v.is_finite() || *v < 1.0 || *v > 32768.0) {
        return Err("invalid desktop viewport".into());
    }
    let mut rgba = Vec::with_capacity(count.saturating_mul(4));
    for pixel in frame.image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }
    let image = lightcraft_codecs::encode::EncodeImage::new(
        u32::try_from(w).map_err(|_| "invalid frame width")?,
        u32::try_from(h).map_err(|_| "invalid frame height")?,
        4,
        lightcraft_codecs::encode::Samples::U8(&rgba),
    );
    let body = lightcraft_codecs::encode::encode_jpeg(
        &image,
        90,
        lightcraft_codecs::encode::ChromaSubsampling::S444,
        &lightcraft_codecs::encode::EncodeMeta::default(),
    )
    .map_err(|e| e.to_string())?;
    if body.len() > MAX_IMAGE {
        return Err("desktop preview exceeds the transmission limit".into());
    }
    Ok(Response {
        code: 200,
        mime: "image/jpeg",
        body,
        headers: vec![
            ("X-LightCraft-Width".into(), frame.viewport[0].to_string()),
            ("X-LightCraft-Height".into(), frame.viewport[1].to_string()),
            ("X-LightCraft-Cursor".into(), frame.cursor.to_string()),
            ("X-LightCraft-State".into(), frame.state),
        ],
    })
}
fn write_response(stream: &mut impl Write, response: Response) -> std::io::Result<()> {
    let reason = match response.code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        503 => "Service Unavailable",
        _ => "Gateway Timeout",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' blob:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'\r\n",
        response.code,
        reason,
        response.mime,
        response.body.len()
    )?;
    for (name, value) in response.headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(&response.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> (State, std::sync::mpsc::Receiver<ControlRequest>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            State {
                token: "test-session".into(),
                tx,
                ctx: egui::Context::default(),
                frame: Mutex::new(()),
                share_origin: "http://192.0.2.2:8080".into(),
            },
            rx,
        )
    }
    fn request(method: &str, path: &str) -> Request {
        Request { method: method.into(), path: path.into(), headers: BTreeMap::from([("host".into(), "127.0.0.1:8080".into())]), body: vec![] }
    }
    #[test]
    fn bounded_parser_rejects_bad_headers_smuggling_and_huge_bodies() {
        for text in [
            "GET / HTTP/1.1\r\nHost: localhost\r\nHost: other\r\n\r\n",
            "POST /api/rpc HTTP/1.1\r\nHost: localhost\r\nContent-Length: 65537\r\n\r\n",
            "POST /api/rpc HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n",
            "POST /api/rpc HTTP/1.1\r\nHost: localhost\r\nContent-Length: 9\r\n\r\n{}",
        ] {
            assert!(read_request(&mut text.as_bytes()).is_err());
        }
        assert!(read_request(&mut std::io::repeat(b'x')).is_err());
        let r = read_request(&mut b"GET /api/frame?v=1 HTTP/1.1\r\nHost: localhost:8080\r\n\r\n".as_slice()).unwrap();
        assert_eq!(r.path, "/api/frame");
    }
    #[test]
    fn local_bootstrap_requires_localhost_and_remote_clients_need_the_key() {
        let (s, _) = state();
        let local = "127.0.0.1:12000".parse().unwrap();
        let remote = "192.0.2.5:12000".parse().unwrap();
        let r = route(request("GET", "/"), local, &s);
        assert_eq!(r.code, 200);
        assert!(r.headers.iter().any(|(name, _)| name == "Set-Cookie"));
        assert_eq!(route(request("GET", "/api/connect"), remote, &s).code, 401);
        let mut r = request("GET", "/api/connect");
        r.headers.insert("authorization".into(), "Bearer test-session".into());
        assert_eq!(route(r, remote, &s).code, 200);
        let mut r = request("GET", "/");
        r.headers.insert("host".into(), "rebind.example:8080".into());
        assert_eq!(route(r, local, &s).code, 403);
    }
    #[test]
    fn cross_origin_actions_and_unexposed_methods_never_reach_the_ui() {
        let (s, rx) = state();
        let peer = "127.0.0.1:12000".parse().unwrap();
        let mut r = request("POST", "/api/rpc");
        r.headers.insert("origin".into(), "https://foreign.example".into());
        r.headers.insert("cookie".into(), "lightcraft_view=test-session".into());
        assert_eq!(route(r, peer, &s).code, 403);
        assert!(rx.try_recv().is_err());
        let mut r = request("POST", "/api/rpc");
        r.headers.insert("origin".into(), "http://127.0.0.1:8080".into());
        r.headers.insert("cookie".into(), "lightcraft_view=test-session".into());
        r.headers.insert("content-type".into(), "application/json".into());
        r.body = br#"{"method":"ui.screenshot","params":{"path":"/some/original.jpg"}}"#.to_vec();
        assert_eq!(route(r, peer, &s).code, 403);
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn authenticated_browser_command_uses_the_existing_control_queue() {
        let (s, rx) = state();
        let peer = "127.0.0.1:12000".parse().unwrap();
        let handler = std::thread::spawn(move || {
            let request = rx.recv().unwrap();
            assert_eq!(request.method, "engine.execute");
            assert_eq!(request.params["command"], "develop.set");
            request.reply.send(json!({"ok":true,"result":{"changed":true}})).unwrap();
        });
        let mut r = request("POST", "/api/rpc");
        r.headers.insert("origin".into(), "http://127.0.0.1:8080".into());
        r.headers.insert("cookie".into(), "lightcraft_view=test-session".into());
        r.headers.insert("content-type".into(), "application/json".into());
        r.body = br#"{"method":"engine.execute","params":{"command":"develop.set","params":{"control":"light.exposure","value":1}}}"#.to_vec();
        let response = route(r, peer, &s);
        assert_eq!(response.code, 200);
        assert_eq!(serde_json::from_slice::<Value>(&response.body).unwrap()["result"]["changed"], true);
        handler.join().unwrap();
    }
    #[test]
    fn desktop_frame_is_bounded_and_encoded_without_source_pixels_or_files() {
        let frame = DesktopFrame {
            image: egui::ColorImage::filled([8, 4], egui::Color32::from_rgb(25, 80, 140)),
            viewport: [1600.0, 1000.0],
            cursor: "crosshair",
            state: "test-frame".into(),
        };
        let response = encode_frame(frame).unwrap();
        assert_eq!(response.mime, "image/jpeg");
        assert!(response.body.starts_with(&[255, 216]));
        assert!(response.headers.contains(&("X-LightCraft-Width".into(), "1600".into())));
        let bad = DesktopFrame {
            image: egui::ColorImage::filled([8, 4], egui::Color32::BLACK),
            viewport: [f32::NAN, 1000.0],
            cursor: "default",
            state: "test-frame".into(),
        };
        assert!(encode_frame(bad).is_err());
        let mut limit = Limited { bytes: vec![], cap: 5 };
        assert!(limit.write_all(b"sixsix").is_err());
        assert!(limit.bytes.is_empty());
    }
}
