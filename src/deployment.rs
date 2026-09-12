use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::ConfiguredCommand;

const MAX_DEPLOYMENTS: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeploymentStatus {
    Queued,
    Running,
    Success,
    Failure,
}

impl DeploymentStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }

    fn is_finished(self) -> bool {
        matches!(self, Self::Success | Self::Failure)
    }
}

struct Deployment {
    api_key: [u8; 32],
    status: DeploymentStatus,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CommandPhase {
    Idle,
    Starting,
    Running,
}

struct CommandRuntime {
    phase: CommandPhase,
    queued: Vec<u64>,
    running: Vec<u64>,
    command: Option<ConfiguredCommand>,
}

struct DeploymentState {
    next_id: u64,
    deployments: BTreeMap<u64, Deployment>,
    commands: BTreeMap<String, CommandRuntime>,
}

pub(crate) struct Deployments {
    state: Mutex<DeploymentState>,
}

impl Deployments {
    pub(crate) fn new() -> Self {
        let next_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX);
        Self {
            state: Mutex::new(DeploymentState {
                next_id,
                deployments: BTreeMap::new(),
                commands: BTreeMap::new(),
            }),
        }
    }

    pub(crate) fn start(self: &Arc<Self>, command: ConfiguredCommand) -> Result<u64, ()> {
        let (id, command_id, start_batch) = {
            let mut state = self.state.lock().expect("deployment state poisoned");
            reclaim_finished(&mut state.deployments);
            if state.deployments.len() >= MAX_DEPLOYMENTS {
                return Err(());
            }
            let id = state.next_id;
            state.next_id = state.next_id.checked_add(1).ok_or(())?;
            state.deployments.insert(
                id,
                Deployment {
                    api_key: command.api_key,
                    status: DeploymentStatus::Queued,
                },
            );
            let command_id = command.id.clone();
            let runtime =
                state
                    .commands
                    .entry(command_id.clone())
                    .or_insert_with(|| CommandRuntime {
                        phase: CommandPhase::Idle,
                        queued: Vec::new(),
                        running: Vec::new(),
                        command: None,
                    });
            runtime.queued.push(id);
            runtime.command = Some(command);
            let start_batch = runtime.phase == CommandPhase::Idle;
            if start_batch {
                runtime.phase = CommandPhase::Starting;
            }
            (id, command_id, start_batch)
        };
        if start_batch {
            self.spawn_batch(command_id);
        }
        Ok(id)
    }

    pub(crate) fn status(&self, id: u64) -> Option<([u8; 32], DeploymentStatus)> {
        let state = self.state.lock().expect("deployment state poisoned");
        state
            .deployments
            .get(&id)
            .map(|deployment| (deployment.api_key, deployment.status))
    }

    fn spawn_batch(self: &Arc<Self>, command_id: String) {
        let deployments = Arc::clone(self);
        let worker_command_id = command_id.clone();
        if thread::Builder::new()
            .spawn(move || deployments.run_batch(worker_command_id))
            .is_err()
        {
            self.fail_starting_batch(&command_id);
        }
    }

    fn fail_starting_batch(&self, command_id: &str) {
        let mut state = self.state.lock().expect("deployment state poisoned");
        let queued = std::mem::take(
            &mut state
                .commands
                .get_mut(command_id)
                .expect("command runtime missing")
                .queued,
        );
        for id in queued {
            if let Some(deployment) = state.deployments.get_mut(&id) {
                deployment.status = DeploymentStatus::Failure;
            }
        }
        let runtime = state
            .commands
            .get_mut(command_id)
            .expect("command runtime missing");
        runtime.command = None;
        runtime.phase = CommandPhase::Idle;
    }

    fn run_batch(self: Arc<Self>, command_id: String) {
        let (batch, command) = {
            let mut state = self.state.lock().expect("deployment state poisoned");
            let batch = std::mem::take(
                &mut state
                    .commands
                    .get_mut(&command_id)
                    .expect("command runtime missing")
                    .queued,
            );
            if batch.is_empty() {
                let runtime = state
                    .commands
                    .get_mut(&command_id)
                    .expect("command runtime missing");
                runtime.command = None;
                runtime.phase = CommandPhase::Idle;
                return;
            }
            for id in &batch {
                if let Some(deployment) = state.deployments.get_mut(id) {
                    deployment.status = DeploymentStatus::Running;
                }
            }
            let runtime = state
                .commands
                .get_mut(&command_id)
                .expect("command runtime missing");
            let command = runtime.command.take().expect("queued command missing");
            runtime.running = batch.clone();
            runtime.phase = CommandPhase::Running;
            (batch, command)
        };

        let success = execute(&command);
        let start_next_batch = {
            let mut state = self.state.lock().expect("deployment state poisoned");
            for id in &batch {
                if let Some(deployment) = state.deployments.get_mut(id) {
                    deployment.status = if success {
                        DeploymentStatus::Success
                    } else {
                        DeploymentStatus::Failure
                    };
                }
            }
            let runtime = state
                .commands
                .get_mut(&command_id)
                .expect("command runtime missing");
            runtime.running.clear();
            let start_next_batch = !runtime.queued.is_empty();
            runtime.phase = if start_next_batch {
                CommandPhase::Starting
            } else {
                CommandPhase::Idle
            };
            start_next_batch
        };
        if start_next_batch {
            self.spawn_batch(command_id);
        }
    }
}

fn reclaim_finished(deployments: &mut BTreeMap<u64, Deployment>) {
    while deployments.len() >= MAX_DEPLOYMENTS {
        let id = deployments
            .iter()
            .find_map(|(id, deployment)| deployment.status.is_finished().then_some(*id));
        match id {
            Some(id) => {
                deployments.remove(&id);
            }
            None => return,
        }
    }
}

fn execute(command: &ConfiguredCommand) -> bool {
    let mut child = match Command::new(&command.executable)
        .args(&command.arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .gid(command.gid)
        .uid(command.uid)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() >= command.timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/deployment.rs"]
mod tests;
