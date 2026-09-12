mod config;
mod deployment;
mod http;

use std::env;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use config::Config;
use deployment::Deployments;

const LISTEN_HOST: &str = "0.0.0.0";

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
        Ok(config) => Arc::new(RwLock::new(config)),
        Err(error) => fail(&error),
    };
    let port = config.read().expect("configuration lock poisoned").port;
    config::watch(PathBuf::from(config_path), Arc::clone(&config), port);
    let deployments = Arc::new(Deployments::new());
    let listen_address = format!("{LISTEN_HOST}:{port}");
    let listener = match TcpListener::bind(&listen_address) {
        Ok(listener) => listener,
        Err(error) => fail(&format!("cannot bind {listen_address}: {error}")),
    };
    http::serve(listener, config, deployments);
}

fn fail(message: &str) -> ! {
    eprintln!("autodeploy-server: {message}");
    std::process::exit(1);
}
