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
    command_index: usize,
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
}

struct DeploymentState {
    next_id: u64,
    deployments: BTreeMap<u64, Deployment>,
    commands: Vec<CommandRuntime>,
}

pub(crate) struct Deployments {
    commands: Arc<Vec<ConfiguredCommand>>,
    state: Mutex<DeploymentState>,
}

impl Deployments {
    pub(crate) fn new(commands: Vec<ConfiguredCommand>) -> Self {
        let next_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX);
        let command_count = commands.len();
        Self {
            commands: Arc::new(commands),
            state: Mutex::new(DeploymentState {
                next_id,
                deployments: BTreeMap::new(),
                commands: (0..command_count)
                    .map(|_| CommandRuntime {
                        phase: CommandPhase::Idle,
                        queued: Vec::new(),
                        running: Vec::new(),
                    })
                    .collect(),
            }),
        }
    }

    pub(crate) fn start(self: &Arc<Self>, command_index: usize) -> Result<u64, ()> {
        let (id, start_batch) = {
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
                    command_index,
                    status: DeploymentStatus::Queued,
                },
            );
            let command = &mut state.commands[command_index];
            command.queued.push(id);
            let start_batch = command.phase == CommandPhase::Idle;
            if start_batch {
                command.phase = CommandPhase::Starting;
            }
            (id, start_batch)
        };
        if start_batch {
            self.spawn_batch(command_index);
        }
        Ok(id)
    }

    pub(crate) fn status(&self, id: u64) -> Option<(usize, DeploymentStatus)> {
        let state = self.state.lock().expect("deployment state poisoned");
        state
            .deployments
            .get(&id)
            .map(|deployment| (deployment.command_index, deployment.status))
    }

    fn spawn_batch(self: &Arc<Self>, command_index: usize) {
        let deployments = Arc::clone(self);
        if thread::Builder::new()
            .spawn(move || deployments.run_batch(command_index))
            .is_err()
        {
            self.fail_starting_batch(command_index);
        }
    }

    fn fail_starting_batch(&self, command_index: usize) {
        let mut state = self.state.lock().expect("deployment state poisoned");
        let queued = std::mem::take(&mut state.commands[command_index].queued);
        for id in queued {
            if let Some(deployment) = state.deployments.get_mut(&id) {
                deployment.status = DeploymentStatus::Failure;
            }
        }
        state.commands[command_index].phase = CommandPhase::Idle;
    }

    fn run_batch(self: Arc<Self>, command_index: usize) {
        let batch = {
            let mut state = self.state.lock().expect("deployment state poisoned");
            let batch = std::mem::take(&mut state.commands[command_index].queued);
            if batch.is_empty() {
                state.commands[command_index].phase = CommandPhase::Idle;
                return;
            }
            for id in &batch {
                if let Some(deployment) = state.deployments.get_mut(id) {
                    deployment.status = DeploymentStatus::Running;
                }
            }
            let command = &mut state.commands[command_index];
            command.running = batch.clone();
            command.phase = CommandPhase::Running;
            batch
        };

        let success = execute(&self.commands[command_index]);
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
            let command = &mut state.commands[command_index];
            command.running.clear();
            let start_next_batch = !command.queued.is_empty();
            command.phase = if start_next_batch {
                CommandPhase::Starting
            } else {
                CommandPhase::Idle
            };
            start_next_batch
        };
        if start_next_batch {
            self.spawn_batch(command_index);
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
