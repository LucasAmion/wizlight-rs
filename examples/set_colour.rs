use std::env;
use std::net::IpAddr;

use wizlight::{Bulb, Channel, Dimming, PilotBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ip: IpAddr = env::args()
        .nth(1)
        .ok_or("usage: cargo run --example set_colour -- <bulb-ip>")?
        .parse()?;
    let bulb = Bulb::connect(ip).await?;
    let colour = PilotBuilder::new()
        .rgb(Channel::new(255), Channel::new(80), Channel::new(0))
        .warm_white(Channel::new(32))
        .dimming(Dimming::new(60)?);

    bulb.set_pilot(&colour).await?;
    Ok(())
}
