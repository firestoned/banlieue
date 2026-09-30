// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `api.rs` (`wait_task`), driven through the fake.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::*;
    use crate::fake::FakeProxmox;
    use crate::{Error, Params};

    const TICK: Duration = Duration::from_millis(1);
    const DEADLINE: Duration = Duration::from_millis(200);

    async fn started(fake: &FakeProxmox) -> Upid {
        fake.add_vm("pve1", 100, "a");
        fake.start_vm("pve1", 100).await.unwrap()
    }

    #[tokio::test]
    async fn wait_returns_ok_when_the_task_succeeds() {
        let fake = FakeProxmox::single_node("pve1");
        let upid = started(&fake).await;
        fake.wait_task_with(&upid, DEADLINE, TICK).await.unwrap();
    }

    #[tokio::test]
    async fn wait_polls_until_a_slow_task_finishes() {
        let fake = FakeProxmox::single_node("pve1");
        fake.set_task_polls_before_done(3);
        let upid = started(&fake).await;
        fake.wait_task_with(&upid, DEADLINE, TICK).await.unwrap();
        assert!(fake.task_polls(&upid) >= 4);
    }

    #[tokio::test]
    async fn a_failed_task_surfaces_its_exit_status() {
        let fake = FakeProxmox::single_node("pve1");
        fake.fail_next_task("storage is full");
        let upid = started(&fake).await;
        let e = fake
            .wait_task_with(&upid, DEADLINE, TICK)
            .await
            .unwrap_err();
        match e {
            Error::TaskFailed { exitstatus, .. } => assert_eq!(exitstatus, "storage is full"),
            other => panic!("{other}"),
        }
    }

    #[tokio::test]
    async fn a_task_that_never_finishes_times_out_instead_of_returning() {
        let fake = FakeProxmox::single_node("pve1");
        fake.set_task_polls_before_done(u32::MAX);
        let upid = started(&fake).await;
        let e = fake
            .wait_task_with(&upid, Duration::from_millis(20), TICK)
            .await
            .unwrap_err();
        assert!(matches!(e, Error::TaskTimeout { .. }), "{e}");
    }

    #[tokio::test]
    async fn a_task_error_from_the_api_propagates() {
        let fake = FakeProxmox::single_node("pve1");
        let upid = Upid::parse("UPID:pve1:1:2:3:qmstart:999:x@pve!t:").unwrap();
        let e = fake
            .wait_task_with(&upid, DEADLINE, TICK)
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Api { .. }), "{e}");
    }

    #[tokio::test]
    async fn params_are_usable_as_a_config_update() {
        let fake = FakeProxmox::single_node("pve1");
        fake.add_vm("pve1", 100, "a");
        fake.set_vm_config("pve1", 100, &Params::new().set("cores", 2))
            .await
            .unwrap();
        assert_eq!(
            fake.vm_config("pve1", 100).await.unwrap().get("cores"),
            Some("2")
        );
    }
}
