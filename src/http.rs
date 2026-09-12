use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::config::{Config, is_identifier, keys_equal, parse_api_key};
use crate::deployment::Deployments;

const HTTP_WORKERS: usize = 16;
const HTTP_QUEUE_SIZE: usize = 64;
const MAX_HEADER_SIZE: usize = 16 * 1024;

pub(crate) fn serve(listener: TcpListener, config: Arc<Config>, deployments: Arc<Deployments>) {
    let (sender, receiver) = mpsc::sync_channel(HTTP_QUEUE_SIZE);
    let receiver = Arc::new(Mutex::new(receiver));
    for _ in 0..HTTP_WORKERS {
        let config = Arc::clone(&config);
        let deployments = Arc::clone(&deployments);
        let receiver = Arc::clone(&receiver);
        thread::spawn(move || serve_connections(receiver, config, deployments));
    }

    for connection in listener.incoming() {
        match connection {
            Ok(stream) => enqueue_connection(&sender, stream),
            Err(_) => continue,
        }
    }
}

fn enqueue_connection(sender: &SyncSender<TcpStream>, stream: TcpStream) {
    match sender.try_send(stream) {
        Ok(()) => {}
        Err(TrySendError::Full(mut stream)) | Err(TrySendError::Disconnected(mut stream)) => {
            let _ = write_response(
                &mut stream,
                503,
                "Service Unavailable",
                "service unavailable",
            );
        }
    }
}

fn serve_connections(
    receiver: Arc<Mutex<Receiver<TcpStream>>>,
    config: Arc<Config>,
    deployments: Arc<Deployments>,
) {
    loop {
        let stream = match receiver.lock().expect("HTTP queue poisoned").recv() {
            Ok(stream) => stream,
            Err(_) => return,
        };
        handle_connection(stream, &config, &deployments);
    }
}

struct Request {
    method: String,
    path: String,
    api_key: Option<[u8; 32]>,
}

enum Route<'a> {
    Start(&'a str),
    Status(u64),
}

fn handle_connection(mut stream: TcpStream, config: &Config, deployments: &Arc<Deployments>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(()) => {
            let _ = write_response(&mut stream, 400, "Bad Request", "bad request");
            return;
        }
    };
    let route = match parse_route(&request.path) {
        Ok(route) => route,
        Err(()) => {
            let _ = write_response(&mut stream, 404, "Not Found", "not found");
            return;
        }
    };
    let Some(api_key) = request.api_key else {
        let _ = write_response(&mut stream, 401, "Unauthorized", "unauthorized");
        return;
    };
    if !config.authorizes_any(&api_key) {
        let _ = write_response(&mut stream, 401, "Unauthorized", "unauthorized");
        return;
    }

    match route {
        Route::Start(command_id) => {
            let Some(command_index) = config.command_index(command_id) else {
                let _ = write_response(&mut stream, 404, "Not Found", "not found");
                return;
            };
            if !keys_equal(&api_key, &config.commands[command_index].api_key) {
                let _ = write_response(&mut stream, 404, "Not Found", "not found");
            } else if request.method != "POST" {
                let _ =
                    write_response(&mut stream, 405, "Method Not Allowed", "method not allowed");
            } else {
                match deployments.start(command_index) {
                    Ok(id) => {
                        let _ = write_response(&mut stream, 200, "OK", &id.to_string());
                    }
                    Err(()) => {
                        let _ = write_response(
                            &mut stream,
                            503,
                            "Service Unavailable",
                            "service unavailable",
                        );
                    }
                }
            }
        }
        Route::Status(id) => {
            if request.method != "GET" {
                let _ =
                    write_response(&mut stream, 405, "Method Not Allowed", "method not allowed");
                return;
            }
            let Some((command_index, status)) = deployments.status(id) else {
                let _ = write_response(&mut stream, 404, "Not Found", "not found");
                return;
            };
            if !keys_equal(&api_key, &config.commands[command_index].api_key) {
                let _ = write_response(&mut stream, 404, "Not Found", "not found");
                return;
            }
            let _ = write_response(&mut stream, 200, "OK", status.as_str());
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Result<Request, ()> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HEADER_SIZE {
            return Err(());
        }
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            if end + 4 != bytes.len() {
                return Err(());
            }
            return parse_request(&bytes[..end]);
        }
    }
}

fn parse_request(bytes: &[u8]) -> Result<Request, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().ok_or(())?;
    let mut parts = request_line.split(' ');
    let method = parts.next().filter(|value| !value.is_empty()).ok_or(())?;
    let path = parts.next().filter(|value| !value.is_empty()).ok_or(())?;
    let version = parts.next().ok_or(())?;
    if parts.next().is_some()
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || !path.starts_with('/')
    {
        return Err(());
    }

    let mut authorization = None;
    let mut authorization_seen = false;
    let mut content_length_seen = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(())?;
        if !is_header_name(name) || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
            return Err(());
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            if authorization_seen {
                return Err(());
            }
            authorization_seen = true;
            authorization = value
                .strip_prefix("Bearer ")
                .and_then(|key| parse_api_key(key).ok());
        } else if name.eq_ignore_ascii_case("content-length") {
            if content_length_seen || value != "0" {
                return Err(());
            }
            content_length_seen = true;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(());
        }
    }
    Ok(Request {
        method: method.to_owned(),
        path: path.to_owned(),
        api_key: authorization,
    })
}

fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn parse_route(path: &str) -> Result<Route<'_>, ()> {
    let mut parts = path.split('/');
    if parts.next() != Some("") {
        return Err(());
    }
    let endpoint = parts.next().ok_or(())?;
    let value = parts.next().ok_or(())?;
    if parts.next().is_some() {
        return Err(());
    }
    match endpoint {
        "start" if is_identifier(value) => Ok(Route::Start(value)),
        "status" => value.parse().map(Route::Status).map_err(|_| ()),
        _ => Err(()),
    }
}

fn write_response(stream: &mut TcpStream, status: u16, reason: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
#[path = "../tests/unit/http.rs"]
mod tests;
