# autodeploy-server

`autodeploy-server` is a small HTTP service for running a configured deployment command. It listens on `0.0.0.0:8080` and is intended to be placed behind Nginx or another TLS-terminating reverse proxy.

## Install

Build the binary with Rust:

```sh
cargo build --release
```

Run it as root, passing the configuration path:

```sh
sudo ./target/release/autodeploy-server /etc/autodeploy-server.conf
```

The configuration must be owned by `root:root` and have exactly `0700` permissions:

```sh
sudo chown root:root /etc/autodeploy-server.conf
sudo chmod 700 /etc/autodeploy-server.conf
```

The service refuses to start if these requirements are not met.

## Configuration

Each command has a unique ID and API key. Every setting shown below is required, including `arguments`; use `[]` when no arguments are needed.

```ini
[command production]
api_key = 8fc23f24979c4697b74e0af7f0c0bb1fd359aab35f3731656c70d3ab3ec4c0e8
user = deploy
group = deploy
executable = /usr/local/sbin/deploy-production
arguments = ["--branch", "main"]
timeout = 300
```

- Command IDs, users, and groups may contain letters, digits, `_`, and `-`.
- API keys are exactly 64 hexadecimal characters. Generate a key with `openssl rand -hex 32`.
- The executable must be an absolute path.
- Arguments are a comma-separated list of quoted strings. Quotes and commas cannot be included in an argument.
- `timeout` is a positive number of seconds. A command that exceeds it is terminated and reported as a failure.
- Commands cannot run as the root user or root group.

## API

All requests require the API key belonging to the command. Send it using the `Authorization` header:

```text
Authorization: Bearer <api-key>
```

Start a deployment:

```sh
curl -X POST \
  -H 'Authorization: Bearer 8fc23f24979c4697b74e0af7f0c0bb1fd359aab35f3731656c70d3ab3ec4c0e8' \
  http://127.0.0.1:8080/start/production
```

The response body is only the deployment ID number.

Check its status with the same command's API key:

```sh
curl \
  -H 'Authorization: Bearer 8fc23f24979c4697b74e0af7f0c0bb1fd359aab35f3731656c70d3ab3ec4c0e8' \
  http://127.0.0.1:8080/status/DEPLOYMENT_ID
```

The response is one of `queued`, `running`, `success`, or `failure`.

Requests for a command already running are queued. Once it exits, all requests accumulated during that execution run together in one follow-up process and receive the same result. The most recent 65,536 deployment IDs and statuses exist only while the server is running.

## Security

Do not expose this service directly to the Internet. Put it behind a TLS reverse proxy, restrict access to trusted callers, and protect each command API key as a secret. The service uses plaintext HTTP and does not provide TLS itself.
