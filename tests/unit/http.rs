use super::*;
use crate::config::ConfiguredCommand;

unsafe extern "C" {
    fn getuid() -> u32;
    fn getgid() -> u32;
}

fn command() -> ConfiguredCommand {
    ConfiguredCommand {
        id: "test".to_owned(),
        api_key: "first-key".to_owned(),
        uid: unsafe { getuid() },
        gid: unsafe { getgid() },
        executable: "/bin/true".to_owned(),
        arguments: Vec::new(),
        timeout: Duration::from_secs(1),
    }
}

#[test]
fn rejects_bad_routes() {
    assert!(parse_route("/start/test/extra").is_err());
    assert!(parse_route("/start/not%20valid").is_err());
    assert!(parse_route("/status/nope").is_err());
}

#[test]
fn accepts_http_1_0_requests() {
    assert!(parse_request(
        b"GET /status/1 HTTP/1.0\r\nAuthorization: Bearer 0707070707070707070707070707070707070707070707070707070707070707"
    )
    .is_ok());
}

fn send_request(config: &RwLock<Config>, deployments: &Arc<Deployments>, request: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::scope(|scope| {
        scope.spawn(|| {
            let (stream, _) = listener.accept().unwrap();
            handle_connection(stream, config, deployments);
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    })
}

fn response_body(response: &str) -> &str {
    response.split_once("\r\n\r\n").unwrap().1
}

#[test]
fn endpoints_require_the_matching_command_key() {
    let first = command();
    let mut second = first.clone();
    second.id = "other".to_owned();
    second.api_key = "second-key".to_owned();
    let config = RwLock::new(Config {
        port: 8080,
        commands: vec![first, second],
    });
    let deployments = Arc::new(Deployments::new());
    let first_key = "first-key";
    let second_key = "second-key";

    let denied = send_request(
        &config,
        &deployments,
        &format!("POST /start/test HTTP/1.1\r\nAuthorization: Bearer {second_key}\r\n\r\n"),
    );
    assert!(denied.starts_with("HTTP/1.1 404"));

    let started = send_request(
        &config,
        &deployments,
        &format!("POST /start/test HTTP/1.1\r\nAuthorization: Bearer {first_key}\r\n\r\n"),
    );
    assert!(started.starts_with("HTTP/1.1 200"));
    let id: u64 = response_body(&started).parse().unwrap();

    let denied_status = send_request(
        &config,
        &deployments,
        &format!("GET /status/{id} HTTP/1.1\r\nAuthorization: Bearer {second_key}\r\n\r\n"),
    );
    assert!(denied_status.starts_with("HTTP/1.1 404"));

    for _ in 0..100 {
        let status = send_request(
            &config,
            &deployments,
            &format!("GET /status/{id} HTTP/1.1\r\nAuthorization: Bearer {first_key}\r\n\r\n"),
        );
        if response_body(&status) == "success" {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("deployment did not succeed");
}

#[test]
fn retains_status_access_for_a_reloaded_command_key() {
    let config = RwLock::new(Config {
        port: 8080,
        commands: vec![command()],
    });
    let deployments = Arc::new(Deployments::new());
    let old_key = "first-key";
    let new_key = "new-key";
    let started = send_request(
        &config,
        &deployments,
        &format!("POST /start/test HTTP/1.1\r\nAuthorization: Bearer {old_key}\r\n\r\n"),
    );
    let id: u64 = response_body(&started).parse().unwrap();
    config.write().unwrap().commands[0].api_key = new_key.to_owned();

    let old_status = send_request(
        &config,
        &deployments,
        &format!("GET /status/{id} HTTP/1.1\r\nAuthorization: Bearer {old_key}\r\n\r\n"),
    );
    assert!(old_status.starts_with("HTTP/1.1 200"));

    let new_status = send_request(
        &config,
        &deployments,
        &format!("GET /status/{id} HTTP/1.1\r\nAuthorization: Bearer {new_key}\r\n\r\n"),
    );
    assert!(new_status.starts_with("HTTP/1.1 404"));
}
