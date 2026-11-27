# HalfLime Rust SDK

[![Apache 2.0 license](https://img.shields.io/badge/license-Apache%202.0-2563EB)](https://github.com/halflime/halflime-rust/blob/main/LICENSE)

Count events from a Rust app in [your own HalfLime instance](https://github.com/halflime/halflime). For example, send `export_finished` after a successful export to see how many PDFs your app produced. The client runs on Tokio and works in a Tauri backend without depending on Tauri.

## Send your first event

Create an `app` project on your collector with `format` in its `properties` list. Set `HALFLIME_ENDPOINT` to the Worker's origin and `HALFLIME_APP_KEY` to the project's public `appKey`. Keep the instance admin token out of the app.

From this checkout, run `cargo run --example track`. The example in `examples/track.rs` sends one event and prints `accepted: 1` when the collector stores it. To see the result, query `get_breakdown` on your instance with the project ID, a time range covering the send, and `dimension: "prop:format"`. Look for the `pdf` row.

## Use it in your app

With this checkout next to your app, add the following to your `Cargo.toml` (Rust 1.89 or newer):

```toml
[dependencies]
halflime = { path = "../halflime-rust" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Create one client at startup. Clones share its HTTP connection pool and session. In a Tokio task that already has the client:

```rust
use halflime::{Client, Error, EventOptions, Props};

async fn record_export(client: &Client, format: &str) -> Result<(), Error> {
    let mut props = Props::new();
    props.insert("format".into(), format.into());
    client.track("export_finished", props, EventOptions::default()).await?;
    Ok(())
}
```

Use HTTPS outside local development. Pass a known format such as `pdf`, not a filename or a customer's document title. Handle analytics errors separately from the export itself.

## If a send times out

The collector may have stored the event before the connection closed. When you need to retry, create the event with `client.event(...)` and keep it. Call `client.send(&event).await` on each attempt. An `accepted` count of `0` means the collector already has that event; calling `track` again would give it a new ID.

The SDK neither queues events on disk nor retries for you. The default timeout is 10 seconds; set `Config::timeout` to a positive duration up to 30 seconds to change it. Await the send before your app exits.

## Sessions and privacy

Sessions live in memory. A new client, an app restart, or 30 minutes without a new event starts a new session. Count sessions as sessions, not people. The SDK sends only when you call it; apply your app's consent settings before you send.

Use short event names and flat string, number or boolean properties. The collector stores only custom keys you allow on the project. The SDK strips URL queries and fragments, but a name or account number in a path can still get through. Pass a route template such as `/accounts/:id` instead. See the [collector API](https://github.com/halflime/halflime/blob/main/docs/api.md) for fields and limits.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release --locked
cargo package --allow-dirty --locked
```

## License

Copyright 2026 [Alex Zappa](https://alex.zappa.dev/) ([FreshJuice](https://freshjuice.dev/)). Licensed under [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0), included in `LICENSE`.
