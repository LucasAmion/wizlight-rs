//! The rate-limited, fire-and-forget `Bulb::stream` path.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::mock_bulb::MockBulb;
use serde_json::Value;
use wizlight::{
    Bulb, DEFAULT_STREAM_INTERVAL, Dimming, PilotBuilder, StreamConfig, StreamCounters,
};

const INTERVAL: Duration = Duration::from_millis(100);

fn config() -> StreamConfig {
    StreamConfig {
        min_interval: INTERVAL,
    }
}

fn dimming(value: u8) -> PilotBuilder {
    PilotBuilder::new().dimming(Dimming::new(value).expect("valid dimming"))
}

fn set_pilots(bulb: &MockBulb) -> Vec<Value> {
    bulb.requests()
        .iter()
        .filter_map(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter(|request| request["method"] == "setPilot")
        .collect()
}

async fn wait_for_set_pilots(bulb: &MockBulb, count: usize) {
    for _ in 0..100 {
        if set_pilots(bulb).len() >= count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "expected {count} setPilot datagrams, got {}",
        set_pilots(bulb).len()
    );
}

#[test]
fn default_rate_is_the_unmeasured_twenty_hertz_placeholder() {
    assert_eq!(DEFAULT_STREAM_INTERVAL, Duration::from_millis(50));
    assert_eq!(
        StreamConfig::default().min_interval,
        DEFAULT_STREAM_INTERVAL
    );
}

#[tokio::test(start_paused = true)]
async fn first_frame_is_sent_without_waiting_for_an_acknowledgement() {
    let bulb = MockBulb::start().await;
    bulb.set_latency(Some(Duration::from_secs(60)));
    let client = Bulb::connect_to(bulb.addr()).await.expect("bind client");
    let stream = client.stream();

    stream.send(&dimming(25)).expect("submit frame");
    wait_for_set_pilots(&bulb, 1).await;

    assert_eq!(set_pilots(&bulb)[0]["params"]["dimming"], 25);
    assert_eq!(
        stream.counters(),
        StreamCounters {
            sent: 1,
            coalesced: 0,
            dropped: 0,
        }
    );
    stream.shutdown().await;
}

#[tokio::test]
async fn an_address_family_mismatch_drops_the_frame_without_retrying() {
    let ipv6_loopback = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 38_899));
    let client = Bulb::connect_to(ipv6_loopback)
        .await
        .expect("bind IPv4 client");
    let stream = client.stream();

    stream.send(&dimming(25)).expect("submit valid frame");

    assert_eq!(
        stream.shutdown().await,
        StreamCounters {
            sent: 0,
            coalesced: 0,
            dropped: 1,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn token_bucket_honours_the_rate_and_coalesces_a_flood_to_one_frame() {
    let bulb = MockBulb::start().await;
    let client = Bulb::connect_to(bulb.addr()).await.expect("bind client");
    let stream = client.stream_with_config(config());

    stream.send(&dimming(1)).expect("first frame");
    wait_for_set_pilots(&bulb, 1).await;

    for index in 1..=1_000 {
        let value = ((index - 1) % 100 + 1) as u8;
        stream.send(&dimming(value)).expect("flood frame");
    }
    assert!(stream.send(&PilotBuilder::new()).is_err());

    tokio::time::advance(INTERVAL - Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(set_pilots(&bulb).len(), 1, "a token refilled early");

    tokio::time::advance(Duration::from_millis(1)).await;
    wait_for_set_pilots(&bulb, 2).await;

    let requests = set_pilots(&bulb);
    assert_eq!(requests.len(), 2, "the flood grew into a queue");
    assert_eq!(requests[1]["params"]["dimming"], 100, "newest frame lost");
    assert_eq!(
        stream.shutdown().await,
        StreamCounters {
            sent: 2,
            coalesced: 999,
            dropped: 0,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn one_kilohertz_input_stays_at_the_configured_packet_rate() {
    let bulb = MockBulb::start().await;
    let client = Bulb::connect_to(bulb.addr()).await.expect("bind client");
    let stream = client.stream_with_config(config());

    stream.send(&dimming(1)).expect("initial frame");
    wait_for_set_pilots(&bulb, 1).await;
    for tick in 1..=1_000 {
        let value = ((tick - 1) % 100 + 1) as u8;
        stream.send(&dimming(value)).expect("tick frame");
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
    }
    wait_for_set_pilots(&bulb, 11).await;

    assert_eq!(set_pilots(&bulb).len(), 11);
    assert_eq!(
        stream.shutdown().await,
        StreamCounters {
            sent: 11,
            coalesced: 990,
            dropped: 0,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn graceful_shutdown_waits_for_and_flushes_the_final_frame() {
    let bulb = MockBulb::start().await;
    let client = Bulb::connect_to(bulb.addr()).await.expect("bind client");
    let stream = client.stream_with_config(config());

    stream.send(&dimming(10)).expect("first frame");
    wait_for_set_pilots(&bulb, 1).await;
    stream.send(&dimming(90)).expect("final frame");

    let shutdown = tokio::spawn(stream.shutdown());
    tokio::task::yield_now().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown discarded the final frame"
    );
    assert_eq!(set_pilots(&bulb).len(), 1);

    tokio::time::advance(INTERVAL).await;
    let counters = shutdown.await.expect("shutdown task");
    wait_for_set_pilots(&bulb, 2).await;

    assert_eq!(set_pilots(&bulb)[1]["params"]["dimming"], 90);
    assert_eq!(
        counters,
        StreamCounters {
            sent: 2,
            coalesced: 0,
            dropped: 0,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_the_handle_also_flushes_the_final_frame() {
    let bulb = MockBulb::start().await;
    let client = Bulb::connect_to(bulb.addr()).await.expect("bind client");
    let stream = client.stream_with_config(config());

    stream.send(&dimming(10)).expect("first frame");
    wait_for_set_pilots(&bulb, 1).await;
    stream.send(&dimming(75)).expect("final frame");
    drop(stream);

    tokio::time::advance(INTERVAL).await;
    wait_for_set_pilots(&bulb, 2).await;
    assert_eq!(set_pilots(&bulb)[1]["params"]["dimming"], 75);
}

#[tokio::test]
async fn an_active_stream_does_not_break_a_request_on_the_shared_socket() {
    let bulb = MockBulb::start().await;
    bulb.set_latency(Some(Duration::from_millis(30)));
    let client = Bulb::connect_to(bulb.addr()).await.expect("bind client");
    let stream = client.stream_with_config(StreamConfig {
        min_interval: Duration::from_millis(1),
    });

    let (config, ()) = tokio::join!(client.get_system_config(), async {
        while !bulb
            .requests()
            .iter()
            .any(|raw| raw.contains("getSystemConfig"))
        {
            tokio::task::yield_now().await;
        }
        for value in 1..=20 {
            stream.send(&dimming(value)).expect("stream frame");
        }
        assert!(
            set_pilots(&bulb).is_empty(),
            "stream interrupted the request"
        );
    },);

    assert_eq!(
        config.expect("config reply").module_name.as_deref(),
        Some("ESP25_SHRGB_01")
    );
    wait_for_set_pilots(&bulb, 1).await;
    assert_eq!(set_pilots(&bulb)[0]["params"]["dimming"], 20);
    assert_eq!(
        stream.shutdown().await,
        StreamCounters {
            sent: 1,
            coalesced: 19,
            dropped: 0,
        }
    );
}
