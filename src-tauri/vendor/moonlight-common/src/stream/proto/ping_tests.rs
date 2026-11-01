use sans_io_time::Instant;

use crate::stream::proto::{
    packet::{SunshinePing, SunshinePingPacket},
    ping::{LEGACY_PING, PING_RETRY_TIMEOUT, PingSender, PingSenderConfig},
};

#[test]
fn ping_legacy() {
    let mut time = Instant::from_nanos(0);

    let mut sender = PingSender::new(
        time,
        PingSenderConfig {
            sunshine_ping: None,
        },
    );

    // check for first ping
    assert_eq!(sender.poll_timeout(), Some(time + PING_RETRY_TIMEOUT));
    assert_eq!(sender.poll_transmit().as_deref(), Some(LEGACY_PING));
    assert_eq!(sender.poll_transmit(), None);

    // advance time only by half
    time += PING_RETRY_TIMEOUT / 2;
    sender.handle_timeout(time);

    // check for no ping
    assert_eq!(sender.poll_timeout(), Some(time + (PING_RETRY_TIMEOUT / 2)));
    assert_eq!(sender.poll_transmit(), None);

    // advance time by another half
    time += PING_RETRY_TIMEOUT / 2;
    sender.handle_timeout(time);

    // check for second ping
    assert_eq!(sender.poll_transmit().as_deref(), Some(LEGACY_PING));
    assert_eq!(sender.poll_transmit(), None);
    assert_eq!(sender.poll_timeout(), Some(time + PING_RETRY_TIMEOUT));
}

fn sunshine_ping(ping: SunshinePing, sequence_number: u32) -> [u8; SunshinePingPacket::SIZE] {
    let packet = SunshinePingPacket {
        payload: ping,
        sequence_number,
    };

    let mut bytes = [0; _];
    packet.serialize(&mut bytes);

    bytes
}

#[test]
fn ping_sunshine() {
    let mut time = Instant::from_nanos(0);
    let ping = SunshinePing([
        54, 53, 48, 69, 57, 67, 66, 52, 54, 51, 57, 65, 53, 54, 70, 70,
    ]);

    let mut sender = PingSender::new(
        time,
        PingSenderConfig {
            sunshine_ping: Some(ping.clone()),
        },
    );

    // check for first ping
    assert_eq!(sender.poll_timeout(), Some(time + PING_RETRY_TIMEOUT));
    assert_eq!(
        sender.poll_transmit().as_deref(),
        Some(sunshine_ping(ping.clone(), 0).as_slice())
    );
    assert_eq!(sender.poll_transmit(), None);

    // advance time only by half
    time += PING_RETRY_TIMEOUT / 2;
    sender.handle_timeout(time);

    // check for no ping
    assert_eq!(sender.poll_timeout(), Some(time + (PING_RETRY_TIMEOUT / 2)));
    assert_eq!(sender.poll_transmit(), None);

    // advance time by another half
    time += PING_RETRY_TIMEOUT / 2;
    sender.handle_timeout(time);

    // check for second ping
    assert_eq!(
        sender.poll_transmit().as_deref(),
        Some(sunshine_ping(ping, 1).as_slice())
    );
    assert_eq!(sender.poll_transmit(), None);
    assert_eq!(sender.poll_timeout(), Some(time + PING_RETRY_TIMEOUT));
}
