mod config;
mod deployment;
mod http;

use std::env;
use std::net::TcpListener;
use std::sync::Arc;

use config::Config;
use deployment::Deployments;

const LISTEN_ADDRESS: &str = "0.0.0.0:8080";

unsafe extern "C" {
    fn getuid() -> u32;
}

fn main() {
    if unsafe { getuid() } != 0 {
        fail("must be run as root");
    }

    let arguments = env::args_os().collect::<Vec<_>>();
    let config_path = match arguments.as_slice() {
        [_, path] => path,
        _ => fail("usage: autodeploy-server <config-file>"),
    };
    let config = match Config::load(config_path) {
        Ok(config) => Arc::new(config),
        Err(error) => fail(&error),
    };
    let deployments = Arc::new(Deployments::new(config.commands.clone()));
    let listener = match TcpListener::bind(LISTEN_ADDRESS) {
        Ok(listener) => listener,
        Err(error) => fail(&format!("cannot bind {LISTEN_ADDRESS}: {error}")),
    };
    http::serve(listener, config, deployments);
}

fn fail(message: &str) -> ! {
    eprintln!("autodeploy-server: {message}");
    std::process::exit(1);
}
