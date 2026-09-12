use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;

const MAX_COMMANDS: usize = 128;

#[derive(Clone)]
pub(crate) struct ConfiguredCommand {
    pub(crate) id: String,
    pub(crate) api_key: [u8; 32],
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) executable: String,
    pub(crate) arguments: Vec<String>,
}

pub(crate) struct Config {
    pub(crate) commands: Vec<ConfiguredCommand>,
}

impl Config {
    pub(crate) fn load(path: &std::ffi::OsStr) -> Result<Self, String> {
        let mut file =
            File::open(path).map_err(|error| format!("cannot open configuration: {error}"))?;
        validate_config_file(&file)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|error| format!("cannot read configuration: {error}"))?;
        parse_config(&contents)
    }

    pub(crate) fn command_index(&self, id: &str) -> Option<usize> {
        self.commands.iter().position(|command| command.id == id)
    }

    pub(crate) fn authorizes_any(&self, key: &[u8; 32]) -> bool {
        self.commands.iter().fold(0_u8, |matches, command| {
            matches | keys_equal(key, &command.api_key) as u8
        }) != 0
    }
}

fn validate_config_file(file: &File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect configuration: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("configuration must be a regular file".to_owned());
    }
    if metadata.uid() != 0 || metadata.gid() != 0 {
        return Err("configuration must be owned by root:root".to_owned());
    }
    if metadata.mode() & 0o7777 != 0o700 {
        return Err("configuration permissions must be 0700".to_owned());
    }
    Ok(())
}

struct PendingCommand {
    id: String,
    api_key: Option<[u8; 32]>,
    user: Option<String>,
    group: Option<String>,
    executable: Option<String>,
    arguments: Option<Vec<String>>,
}

impl PendingCommand {
    fn new(id: String) -> Self {
        Self {
            id,
            api_key: None,
            user: None,
            group: None,
            executable: None,
            arguments: None,
        }
    }

    fn finish(self) -> Result<ConfiguredCommand, String> {
        let user = required(self.user, "user", &self.id)?;
        let group = required(self.group, "group", &self.id)?;
        let uid = lookup_user(&user)?;
        let gid = lookup_group(&group)?;
        if uid == 0 || gid == 0 {
            return Err(format!("command {} must not use root", self.id));
        }
        let executable = required(self.executable, "executable", &self.id)?;
        if !executable.starts_with('/') || executable.as_bytes().contains(&0) {
            return Err(format!(
                "command {} executable must be an absolute path",
                self.id
            ));
        }
        Ok(ConfiguredCommand {
            id: self.id.clone(),
            api_key: required(self.api_key, "api_key", &self.id)?,
            uid,
            gid,
            executable,
            arguments: required(self.arguments, "arguments", &self.id)?,
        })
    }
}

fn required<T>(value: Option<T>, field: &str, command: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("command {command} is missing {field}"))
}

fn parse_config(contents: &str) -> Result<Config, String> {
    let mut commands = Vec::new();
    let mut current: Option<PendingCommand> = None;

    for (line_number, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let id = parse_section(line).map_err(|error| line_error(line_number, error))?;
            if commands.len() >= MAX_COMMANDS {
                return Err(line_error(line_number, "too many commands".to_owned()));
            }
            if let Some(command) = current.take() {
                commands.push(command.finish()?);
            }
            if commands
                .iter()
                .any(|command: &ConfiguredCommand| command.id == id)
            {
                return Err(line_error(line_number, "duplicate command id".to_owned()));
            }
            current = Some(PendingCommand::new(id));
            continue;
        }

        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| line_error(line_number, "expected key = value".to_owned()))?;
        let key = key.trim();
        let value = value.trim();
        let command = current.as_mut().ok_or_else(|| {
            line_error(
                line_number,
                "settings must be in a command section".to_owned(),
            )
        })?;
        match key {
            "api_key" => set_once(&mut command.api_key, parse_api_key(value), key, line_number)?,
            "user" => set_once(
                &mut command.user,
                parse_name(value, "user"),
                key,
                line_number,
            )?,
            "group" => set_once(
                &mut command.group,
                parse_name(value, "group"),
                key,
                line_number,
            )?,
            "executable" => set_once(
                &mut command.executable,
                nonempty(value, "executable"),
                key,
                line_number,
            )?,
            "arguments" => set_once(
                &mut command.arguments,
                parse_arguments(value),
                key,
                line_number,
            )?,
            _ => return Err(line_error(line_number, format!("unknown setting {key}"))),
        }
    }
    if let Some(command) = current {
        commands.push(command.finish()?);
    }
    if commands.is_empty() {
        return Err("configuration has no commands".to_owned());
    }
    Ok(Config { commands })
}

fn set_once<T>(
    slot: &mut Option<T>,
    value: Result<T, String>,
    key: &str,
    line: usize,
) -> Result<(), String> {
    if slot.is_some() {
        return Err(line_error(line, format!("duplicate setting {key}")));
    }
    *slot = Some(value.map_err(|error| line_error(line, error))?);
    Ok(())
}

fn line_error(line: usize, message: String) -> String {
    format!("configuration line {}: {message}", line + 1)
}

fn parse_section(line: &str) -> Result<String, String> {
    let Some(value) = line.strip_prefix("[command ") else {
        return Err("expected [command <id>]".to_owned());
    };
    let Some(id) = value.strip_suffix(']') else {
        return Err("expected [command <id>]".to_owned());
    };
    if !is_identifier(id) {
        return Err("invalid command id".to_owned());
    }
    Ok(id.to_owned())
}

fn parse_name(value: &str, field: &str) -> Result<String, String> {
    if !is_identifier(value) {
        return Err(format!("invalid {field}"));
    }
    Ok(value.to_owned())
}

fn nonempty(value: &str, field: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    Ok(value.to_owned())
}

pub(crate) fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub(crate) fn parse_api_key(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 {
        return Err("api_key must be 64 hexadecimal characters".to_owned());
    }
    let mut key = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_digit(pair[0]).ok_or_else(|| "api_key must be hexadecimal".to_owned())?;
        let low = hex_digit(pair[1]).ok_or_else(|| "api_key must be hexadecimal".to_owned())?;
        key[index] = high << 4 | low;
    }
    Ok(key)
}

pub(crate) fn keys_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0_u8;
    for index in 0..left.len() {
        difference |= left[index] ^ right[index];
    }
    difference == 0
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_arguments(value: &str) -> Result<Vec<String>, String> {
    let values = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| "arguments must be a list".to_owned())?
        .trim();
    if values.is_empty() {
        return Ok(Vec::new());
    }
    values
        .split(',')
        .map(|value| {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .ok_or_else(|| "arguments must be quoted".to_owned())?;
            if value.contains('"') || value.as_bytes().contains(&0) {
                return Err("invalid argument".to_owned());
            }
            Ok(value.to_owned())
        })
        .collect()
}

fn lookup_user(name: &str) -> Result<u32, String> {
    let contents = std::fs::read_to_string("/etc/passwd")
        .map_err(|error| format!("cannot read /etc/passwd: {error}"))?;
    for line in contents.lines() {
        let mut fields = line.split(':');
        if fields.next() == Some(name) {
            fields.next();
            return fields
                .next()
                .ok_or_else(|| format!("invalid passwd entry for {name}"))?
                .parse()
                .map_err(|_| format!("invalid uid for {name}"));
        }
    }
    Err(format!("unknown user {name}"))
}

fn lookup_group(name: &str) -> Result<u32, String> {
    let contents = std::fs::read_to_string("/etc/group")
        .map_err(|error| format!("cannot read /etc/group: {error}"))?;
    for line in contents.lines() {
        let mut fields = line.split(':');
        if fields.next() == Some(name) {
            fields.next();
            return fields
                .next()
                .ok_or_else(|| format!("invalid group entry for {name}"))?
                .parse()
                .map_err(|_| format!("invalid gid for {name}"));
        }
    }
    Err(format!("unknown group {name}"))
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
