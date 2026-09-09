use std::time::Duration;

use wizlight::{Discovery, SystemConfig};

#[tokio::main]
async fn main() -> Result<(), wizlight::Error> {
    let bulbs = Discovery::new()
        .system_config(true)
        .collect(Duration::from_secs(5))
        .await?;

    for bulb in bulbs {
        let config = bulb
            .system_config
            .as_ref()
            .and_then(|response| response.parse_result::<SystemConfig>().ok());
        println!(
            "{}  {}  {}  {}",
            bulb.mac,
            bulb.addr.ip(),
            config
                .as_ref()
                .and_then(|config| config.module_name.as_deref())
                .unwrap_or("unknown model"),
            config
                .as_ref()
                .and_then(|config| config.fw_version.as_deref())
                .unwrap_or("unknown firmware"),
        );
    }
    Ok(())
}
