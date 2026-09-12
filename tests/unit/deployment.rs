use super::*;

unsafe extern "C" {
    fn getuid() -> u32;
    fn getgid() -> u32;
}

fn command(arguments: Vec<&str>, timeout: Duration) -> ConfiguredCommand {
    ConfiguredCommand {
        id: "test".to_owned(),
        api_key: [7; 32],
        uid: unsafe { getuid() },
        gid: unsafe { getgid() },
        executable: "/bin/sleep".to_owned(),
        arguments: arguments.into_iter().map(str::to_owned).collect(),
        timeout,
    }
}

fn wait_for(manager: &Deployments, id: u64, expected: DeploymentStatus) {
    for _ in 0..100 {
        if manager.status(id).map(|(_, status)| status) == Some(expected) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("deployment {id} did not reach {expected:?}");
}

#[test]
fn queues_follow_up_batch() {
    let manager = Arc::new(Deployments::new(vec![command(
        vec!["0.20"],
        Duration::from_secs(1),
    )]));
    let first = manager.start(0).unwrap();
    wait_for(&manager, first, DeploymentStatus::Running);
    let second = manager.start(0).unwrap();
    let third = manager.start(0).unwrap();
    assert_eq!(manager.status(second).unwrap().1, DeploymentStatus::Queued);
    assert_eq!(manager.status(third).unwrap().1, DeploymentStatus::Queued);
    wait_for(&manager, first, DeploymentStatus::Success);
    wait_for(&manager, second, DeploymentStatus::Success);
    assert_eq!(manager.status(third).unwrap().1, DeploymentStatus::Success);
}

#[test]
fn fails_a_command_that_exceeds_its_timeout() {
    let manager = Arc::new(Deployments::new(vec![command(
        vec!["1"],
        Duration::from_millis(50),
    )]));
    let id = manager.start(0).unwrap();
    wait_for(&manager, id, DeploymentStatus::Failure);
}
