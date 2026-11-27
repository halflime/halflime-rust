#![doc = include_str!("../README.md")]

use chrono::{SecondsFormat, Utc};
use regex::Regex;
use reqwest::{Url, header::HeaderValue};
use serde_json::{Map, Value, json};
use std::{
    fmt,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub type Props = Map<String, Value>;
pub type SystemProps = Map<String, Value>;

pub struct Config {
    pub endpoint: String,
    pub app_key: String,
    pub system_props: SystemProps,
    pub timeout: Duration,
}

impl Config {
    pub fn new(endpoint: impl Into<String>, app_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            app_key: app_key.into(),
            system_props: SystemProps::new(),
            timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct EventOptions {
    pub path: Option<String>,
    pub referrer: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Event {
    value: Value,
    body: Arc<str>,
}

impl Event {
    pub fn as_json(&self) -> &Value {
        &self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acknowledgment {
    pub accepted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidEvent,
    Transport,
    Timeout,
    Http(u16),
    InvalidAcknowledgment,
    ResponseTooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig => f.write_str("Invalid HalfLime configuration"),
            Self::InvalidEvent => f.write_str("Invalid HalfLime event"),
            Self::Transport => f.write_str("HalfLime delivery failed"),
            Self::Timeout => f.write_str("HalfLime delivery timed out"),
            Self::Http(status) => write!(f, "HalfLime rejected the event ({status})"),
            Self::InvalidAcknowledgment => f.write_str("Invalid HalfLime acknowledgment"),
            Self::ResponseTooLarge => f.write_str("HalfLime acknowledgment exceeds 1 KiB"),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Url,
    app_key: HeaderValue,
    system_props: Arc<SystemProps>,
    session: Arc<Mutex<(Uuid, Instant)>>,
    timeout: Duration,
}

impl Client {
    pub fn new(config: Config) -> Result<Self, Error> {
        if !Regex::new(r"(?i)^https?://[^/?#\\\s]+(?:/|/api/v1/events)?$")
            .unwrap()
            .is_match(&config.endpoint)
        {
            return Err(Error::InvalidConfig);
        }
        let mut endpoint = Url::parse(&config.endpoint).map_err(|_| Error::InvalidConfig)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(endpoint.path(), "/" | "/api/v1/events")
            || config.timeout.is_zero()
            || config.timeout > Duration::from_secs(30)
            || config.app_key.is_empty()
            || config.app_key.len() > 200
            || !config.app_key.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(Error::InvalidConfig);
        }
        endpoint.set_path("/api/v1/events");
        let app_key = HeaderValue::from_str(&config.app_key).map_err(|_| Error::InvalidConfig)?;
        let system_props = config
            .system_props
            .into_iter()
            .filter(|(key, value)| {
                if key == "isDebug" {
                    value.is_boolean()
                } else {
                    matches!(
                        key.as_str(),
                        "appVersion"
                            | "appBuildNumber"
                            | "osName"
                            | "osVersion"
                            | "locale"
                            | "deviceModel"
                    ) && value
                        .as_str()
                        .is_some_and(|v| v.encode_utf16().count() <= 100 && safe_string(v))
                }
            })
            .chain(std::iter::once((
                "sdkVersion".into(),
                json!(concat!("halflime-rust@", env!("CARGO_PKG_VERSION"))),
            )))
            .collect();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(config.timeout)
            .build()
            .map_err(|_| Error::InvalidConfig)?;
        Ok(Self {
            http,
            endpoint,
            app_key,
            system_props: Arc::new(system_props),
            session: Arc::new(Mutex::new((Uuid::new_v4(), Instant::now()))),
            timeout: config.timeout,
        })
    }

    pub fn event(&self, name: &str, props: Props, options: EventOptions) -> Result<Event, Error> {
        if name.trim().is_empty() || name.encode_utf16().count() > 100 || !safe_string(name) {
            return Err(Error::InvalidEvent);
        }
        let session_id = {
            let mut session = self.session.lock().map_err(|_| Error::InvalidEvent)?;
            if session.1.elapsed() >= Duration::from_secs(1800) {
                session.0 = Uuid::new_v4();
            }
            session.1 = Instant::now();
            session.0
        };
        let props: Props = props
            .into_iter()
            .filter(|(key, value)| {
                property_name().is_match(key)
                    && !sensitive_name().is_match(key)
                    && (value.is_boolean()
                        || value.as_f64().is_some_and(f64::is_finite)
                        || value
                            .as_str()
                            .is_some_and(|v| v.encode_utf16().count() <= 200 && safe_string(v)))
            })
            .take(20)
            .collect();
        let mut value = json!({ "eventId": Uuid::new_v4().to_string(), "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true), "sessionId": session_id.to_string(), "eventName": name, "systemProps": *self.system_props, "props": props });
        if let Some(path) = options.path.and_then(|v| sanitize_path(&v)) {
            value["path"] = json!(path);
        }
        if let Some(referrer) = options.referrer.and_then(|v| sanitize_referrer(&v)) {
            value["referrer"] = json!(referrer);
        }
        let body = serde_json::to_string(&[&value]).map_err(|_| Error::InvalidEvent)?;
        if body.len() > 65536 {
            return Err(Error::InvalidEvent);
        }
        Ok(Event {
            value,
            body: body.into(),
        })
    }

    pub async fn send(&self, event: &Event) -> Result<Acknowledgment, Error> {
        tokio::time::timeout(self.timeout, async {
            let mut response = self
                .http
                .post(self.endpoint.clone())
                .header("Content-Type", "application/json")
                .header("App-Key", self.app_key.clone())
                .body(event.body.to_string())
                .send()
                .await
                .map_err(transport_error)?;
            if response.status().as_u16() != 200 {
                return Err(Error::Http(response.status().as_u16()));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
                if body.len() + chunk.len() > 1024 {
                    return Err(Error::ResponseTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            let ack: Value =
                serde_json::from_slice(&body).map_err(|_| Error::InvalidAcknowledgment)?;
            let accepted = ack
                .get("accepted")
                .and_then(Value::as_u64)
                .filter(|n| *n <= 1)
                .ok_or(Error::InvalidAcknowledgment)?;
            Ok(Acknowledgment {
                accepted: accepted as usize,
            })
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    pub async fn track(
        &self,
        name: &str,
        props: Props,
        options: EventOptions,
    ) -> Result<Acknowledgment, Error> {
        self.send(&self.event(name, props, options)?).await
    }
}

fn transport_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Transport
    }
}
fn property_name() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-zA-Z][a-zA-Z0-9_]{0,39}$").unwrap())
}
fn sensitive_name() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)email|phone|password|secret|token|credential|authorization|cookie|address|user.?id|session.?id|ip.?address|^ip$|^name$").unwrap())
}
fn safe_string(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    !value.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}') && !RE.get_or_init(|| Regex::new(r"(?i)[\w.+-]+@[\w.-]+\.[a-z]{2,}|\b(?:\d{1,3}\.){3}\d{1,3}\b|(?:[0-9a-f]{0,4}:){2,}[0-9a-f:]*|bearer\s|(?:sk|pk|ghp|github_pat|AKIA)[_-][a-z0-9]{8,}|eyJ[a-z0-9_-]+\.[a-z0-9_-]+|(?:password|token|secret|api.?key)\s*[:=]").unwrap()).is_match(value)
}
fn sanitize_path(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let url = Url::parse("https://halflime.invalid")
        .ok()?
        .join(value)
        .ok()?;
    let decoded = percent_encoding::percent_decode_str(url.path())
        .decode_utf8()
        .ok()?;
    if !matches!(url.scheme(), "http" | "https") || !safe_string(&decoded) {
        return None;
    }
    Some(url.path().chars().take(512).collect())
}
fn sanitize_referrer(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !safe_string(url.host_str()?) {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_clients_share_session_and_rotate_after_idle_boundary() {
        let client = Client::new(Config::new("http://localhost", "key")).unwrap();
        let first = client
            .event("first", Props::new(), EventOptions::default())
            .unwrap();
        let clone = client.clone();
        let next = clone
            .event("next", Props::new(), EventOptions::default())
            .unwrap();
        assert_eq!(first.as_json()["sessionId"], next.as_json()["sessionId"]);
        client.session.lock().unwrap().1 = Instant::now() - Duration::from_secs(1800);
        let rotated = clone
            .event("idle", Props::new(), EventOptions::default())
            .unwrap();
        assert_ne!(first.as_json()["sessionId"], rotated.as_json()["sessionId"]);
        assert_eq!(first.as_json()["sessionId"], next.as_json()["sessionId"]);
    }
}
