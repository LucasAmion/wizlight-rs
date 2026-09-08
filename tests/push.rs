//! The shared `syncPilot` and `firstBeat` listener.

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use common::mock_bulb::MockBulb;
use serde_json::json;
use tokio::net::UdpSocket;
use tokio::time::timeout;
use wizlight::{
    Discovery, Error, PUSH_KEEPALIVE_INTERVAL, PushEvent, PushManager, PushUnavailable,
};

const WAIT: Duration = Duration::from_secs(2);

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

async fn wait_for_registrations(bulb: &MockBulb, count: usize) {
    for _ in 0..100 {
        let registrations = bulb
            .requests()
            .iter()
            .filter(|request| request.contains("\"method\":\"registration\""))
            .count();
        if registrations >= count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("expected {count} registrations, got {:?}", bulb.requests());
}

async fn next(subscription: &mut wizlight::PushSubscription) -> PushEvent {
    timeout(WAIT, subscription.recv())
        .await
        .expect("push timeout")
        .expect("listener stopped")
}

#[tokio::test]
async fn mock_bulb_sync_pilot_is_parsed_and_registered_with_the_route_source_ip() {
    let manager = PushManager::bind_to(loopback(0)).await.expect("bind push");
    let bulb = MockBulb::builder()
        .push_port(manager.local_addr().port())
        .start()
        .await;
    let mut subscription = manager
        .subscribe(bulb.mac().to_ascii_uppercase(), bulb.addr())
        .await
        .expect("subscribe");

    let PushEvent::SyncPilot { addr, pilot } = next(&mut subscription).await else {
        panic!("expected syncPilot");
    };
    assert_eq!(addr, bulb.addr());
    assert_eq!(pilot.mac.as_deref(), Some(bulb.mac()));
    assert_eq!(pilot.temp, Some(2700));
    assert_eq!(subscription.mac(), bulb.mac());

    wait_for_registrations(&bulb, 1).await;
    let request = bulb.last_request().expect("registration");
    assert_eq!(request["method"], "registration");
    assert_eq!(request["params"]["phoneIp"], "127.0.0.1");
    assert_eq!(request["params"]["phoneMac"], "AAAAAAAAAAAA");
    assert_eq!(request["params"]["register"], true);
    assert!(request["params"].get("id").is_none());
}

#[tokio::test(start_paused = true)]
async fn registration_is_refreshed_every_twenty_seconds() {
    let manager = PushManager::bind_to(loopback(0)).await.expect("bind push");
    let bulb = MockBulb::builder()
        .push_port(manager.local_addr().port())
        .start()
        .await;
    let _subscription = manager
        .subscribe(bulb.mac(), bulb.addr())
        .await
        .expect("subscribe");
    wait_for_registrations(&bulb, 1).await;

    tokio::time::advance(PUSH_KEEPALIVE_INTERVAL - Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        bulb.requests()
            .iter()
            .filter(|request| request.contains("\"method\":\"registration\""))
            .count(),
        1
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    wait_for_registrations(&bulb, 2).await;
}

#[tokio::test]
async fn refresh_reregisters_after_discovery_clears_the_push_target() {
    let manager = PushManager::bind_to(loopback(0)).await.expect("bind push");
    let bulb = MockBulb::builder()
        .push_port(manager.local_addr().port())
        .start()
        .await;
    let mut subscription = manager
        .subscribe(bulb.mac(), bulb.addr())
        .await
        .expect("subscribe");
    let _ = next(&mut subscription).await;
    assert_eq!(bulb.push_target(), Some(manager.local_addr()));

    Discovery::new()
        .target(bulb.addr())
        .collect(Duration::from_millis(50))
        .await
        .expect("discover");
    assert_eq!(bulb.push_target(), None);

    manager.refresh().await.expect("refresh registration");
    let PushEvent::SyncPilot { pilot, .. } = next(&mut subscription).await else {
        panic!("expected syncPilot after refresh");
    };
    assert_eq!(pilot.mac.as_deref(), Some(bulb.mac()));
    assert_eq!(bulb.push_target(), Some(manager.local_addr()));
}

#[tokio::test]
async fn first_beat_is_delivered_while_test_garbage_and_other_macs_are_ignored() {
    let manager = PushManager::bind_to(loopback(0)).await.expect("bind push");
    let bulb = MockBulb::builder()
        .push_port(manager.local_addr().port())
        .start()
        .await;
    let mut subscription = manager
        .subscribe(bulb.mac(), bulb.addr())
        .await
        .expect("subscribe");
    let _ = next(&mut subscription).await;

    let sender = UdpSocket::bind(loopback(0)).await.expect("bind sender");
    let target = manager.local_addr();
    sender.send_to(b"test", target).await.expect("send test");
    sender
        .send_to(b"not json", target)
        .await
        .expect("send garbage");
    sender
        .send_to(
            json!({
                "method": "syncPilot",
                "params": {"mac": "000000000000", "state": false}
            })
            .to_string()
            .as_bytes(),
            target,
        )
        .await
        .expect("send other bulb");
    assert!(
        timeout(Duration::from_millis(50), subscription.recv())
            .await
            .is_err()
    );

    sender
        .send_to(
            json!({"method": "firstBeat", "params": {"mac": bulb.mac()}})
                .to_string()
                .as_bytes(),
            target,
        )
        .await
        .expect("send firstBeat");

    assert_eq!(
        next(&mut subscription).await,
        PushEvent::FirstBeat {
            addr: sender.local_addr().expect("sender addr"),
            mac: bulb.mac().to_owned(),
        }
    );
}

#[tokio::test]
async fn port_in_use_has_a_typed_polling_fallback_reason() {
    let occupied = UdpSocket::bind(loopback(0)).await.expect("occupy port");
    let addr = occupied.local_addr().expect("occupied addr");

    match PushManager::bind_to(addr)
        .await
        .expect_err("must refuse port")
    {
        Error::PushUnavailable(PushUnavailable::PortInUse { addr: refused }) => {
            assert_eq!(refused, addr);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn registry_routes_by_mac_and_the_last_cancel_releases_the_port() {
    let probe = UdpSocket::bind(loopback(0)).await.expect("allocate port");
    let addr = probe.local_addr().expect("allocated addr");
    drop(probe);

    let manager = PushManager::bind_to(addr).await.expect("bind push");
    let first = MockBulb::builder()
        .mac("9877d5230f0a")
        .push_port(addr.port())
        .start()
        .await;
    let second = MockBulb::builder()
        .mac("9877d523a4da")
        .push_port(addr.port())
        .start()
        .await;
    let mut first_sub = manager
        .subscribe(first.mac(), first.addr())
        .await
        .expect("first subscription");
    let mut second_sub = manager
        .subscribe(second.mac(), second.addr())
        .await
        .expect("second subscription");

    let PushEvent::SyncPilot { pilot, .. } = next(&mut first_sub).await else {
        panic!("first syncPilot");
    };
    assert_eq!(pilot.mac.as_deref(), Some(first.mac()));
    let PushEvent::SyncPilot { pilot, .. } = next(&mut second_sub).await else {
        panic!("second syncPilot");
    };
    assert_eq!(pilot.mac.as_deref(), Some(second.mac()));

    drop(first_sub);
    assert!(matches!(
        PushManager::bind_to(addr).await,
        Err(Error::PushUnavailable(PushUnavailable::PortInUse { .. }))
    ));

    second_sub.cancel().await.expect("cancel last subscription");
    wait_for_registrations(&second, 2).await;
    assert_eq!(
        second.last_request().expect("unregistration")["params"]["register"],
        false
    );
    let rebound = UdpSocket::bind(addr)
        .await
        .expect("listener port released after cancellation");
    drop(rebound);
}

#[tokio::test]
async fn dropping_the_last_subscription_stops_the_listener() {
    let probe = UdpSocket::bind(loopback(0)).await.expect("allocate port");
    let addr = probe.local_addr().expect("allocated addr");
    drop(probe);

    let manager = PushManager::bind_to(addr).await.expect("bind push");
    let bulb = MockBulb::builder().push_port(addr.port()).start().await;
    let mut subscription = manager
        .subscribe(bulb.mac(), bulb.addr())
        .await
        .expect("subscribe");
    let _ = next(&mut subscription).await;
    drop(subscription);

    for _ in 0..100 {
        if let Ok(rebound) = UdpSocket::bind(addr).await {
            drop(rebound);
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("listener port was not released after dropping its last subscription");
}
