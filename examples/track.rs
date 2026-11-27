use halflime::{Client, Config, EventOptions, Props};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::new(
        std::env::var("HALFLIME_ENDPOINT")?,
        std::env::var("HALFLIME_APP_KEY")?,
    ))?;
    let mut props = Props::new();
    props.insert("format".into(), "pdf".into());
    let ack = client
        .track("export_finished", props, EventOptions::default())
        .await?;
    println!("accepted: {}", ack.accepted);
    Ok(())
}
