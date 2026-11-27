# Changelog

## 1.0.0 (unreleased)

- Send app events with an async Tokio client. Client clones share a session and a connection pool.
- Resend a saved event after a timeout without counting it twice on the collector.
