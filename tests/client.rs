use halflime::{Client, Config, EventOptions, Props};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn sends_over_http_and_reuses_bytes_after_lost_ack() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let received = bodies.clone();
    let server = tokio::spawn(async move {
        for attempt in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut socket).await;
            assert!(headers.starts_with("POST /api/v1/events HTTP/1.1"));
            assert!(headers.to_lowercase().contains("app-key: public-key"));
            assert!(!headers.to_lowercase().contains("cookie:"));
            received.lock().unwrap().push(body);
            if attempt == 0 {
                continue;
            }
            let ack = if attempt == 1 {
                "{\"accepted\":1}"
            } else {
                "{\"accepted\":0}"
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", ack.len(), ack).as_bytes()).await.unwrap();
        }
    });
    let client = Client::new(Config::new(endpoint, "public-key")).unwrap();
    let mut props = Props::new();
    props.insert("plan".into(), json!("free"));
    props.insert("email".into(), json!("a@example.org"));
    props.insert("other".into(), json!("192.168.1.1"));
    props.insert("nested".into(), json!({"a": 1}));
    let event = client
        .event(
            "opened",
            props,
            EventOptions {
                path: Some("/pricing?secret=x#hash".into()),
                referrer: Some("https://example.org/private?secret=x".into()),
            },
        )
        .unwrap();
    assert!(client.send(&event).await.is_err());
    assert_eq!(client.clone().send(&event).await.unwrap().accepted, 1);
    assert_eq!(client.send(&event).await.unwrap().accepted, 0);
    server.await.unwrap();
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(bodies[1], bodies[2]);
    let wire: serde_json::Value = serde_json::from_slice(&bodies[0]).unwrap();
    assert_eq!(wire[0]["props"], json!({"plan": "free"}));
    assert_eq!(wire[0]["path"], "/pricing");
    assert_eq!(wire[0]["referrer"], "https://example.org");
    assert_eq!(
        wire[0]["systemProps"]["sdkVersion"],
        concat!("halflime-rust@", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        event.as_json()["sessionId"],
        client
            .event("next", Props::new(), EventOptions::default())
            .unwrap()
            .as_json()["sessionId"]
    );
}

#[test]
fn validates_input_and_filters_untrusted_properties() {
    use std::time::Duration;
    for endpoint in [
        "file:///tmp/a",
        "https://user:pass@example.org",
        "https://example.org/no",
        "https://example.org/a/../",
        "https://example.org?secret=x",
    ] {
        assert!(Client::new(Config::new(endpoint, "key")).is_err());
    }
    assert!(Client::new(Config::new("https://example.org", "bad\nkey")).is_err());
    let mut config = Config::new("https://example.org/api/v1/events", "key");
    config.timeout = Duration::ZERO;
    assert!(Client::new(config).is_err());
    let mut config = Config::new("https://example.org", "key");
    config.system_props.insert("isDebug".into(), json!(true));
    config
        .system_props
        .insert("unknown".into(), json!("discard"));
    let client = Client::new(config).unwrap();
    assert!(
        client
            .event("", Props::new(), EventOptions::default())
            .is_err()
    );
    assert!(
        client
            .event("a@example.org", Props::new(), EventOptions::default())
            .is_err()
    );
    let props = (0..30).map(|i| (format!("prop{i}"), json!(i))).collect();
    let event = client
        .event(
            "bounded",
            props,
            EventOptions {
                path: Some("/user/a%40example.org".into()),
                referrer: Some("https://192.168.1.1/private".into()),
            },
        )
        .unwrap();
    assert_eq!(event.as_json()["props"].as_object().unwrap().len(), 20);
    assert!(event.as_json().get("path").is_none());
    assert!(event.as_json().get("referrer").is_none());
    assert!(event.as_json()["systemProps"].get("unknown").is_none());
    assert_eq!(event.as_json()["systemProps"]["isDebug"], true);
}

#[tokio::test]
async fn rejects_redirects_and_bounds_ack_body_and_deadline() {
    use halflime::Error;
    use std::time::Duration;
    for (reply, expected) in [
        (
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: /redirected\r\nContent-Length: 0\r\n\r\n"
                .to_owned(),
            Error::Http(307),
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{\"accepted\":2}".to_owned(),
            Error::InvalidAcknowledgment,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 17\r\n\r\n{\"accepted\":true}".to_owned(),
            Error::InvalidAcknowledgment,
        ),
        (
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: 1025\r\n\r\n{}",
                " ".repeat(1025)
            ),
            Error::ResponseTooLarge,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 200\r\n\r\n{\"accepted\":1}".to_owned(),
            Error::Transport,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = Config::new(format!("http://{}", listener.local_addr().unwrap()), "key");
        config.timeout = Duration::from_millis(200);
        let client = Client::new(config).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket.write_all(reply.as_bytes()).await.unwrap();
        });
        let event = client
            .event("test", Props::new(), EventOptions::default())
            .unwrap();
        assert_eq!(client.send(&event).await.unwrap_err(), expected);
        server.await.unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::new(format!("http://{}", listener.local_addr().unwrap()), "key");
    config.timeout = Duration::from_millis(50);
    let client = Client::new(config).unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let start = std::time::Instant::now();
    assert_eq!(
        client
            .track("test", Props::new(), EventOptions::default())
            .await
            .unwrap_err(),
        Error::Timeout
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    server.abort();
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        request.push(byte[0]);
        assert!(request.len() <= 8192);
    }
    let headers = String::from_utf8(request).unwrap();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length: ")
                .map(str::to_owned)
        })
        .unwrap()
        .parse()
        .unwrap();
    assert!(length <= 65536);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    (headers, body)
}
