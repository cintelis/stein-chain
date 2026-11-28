//! Per-client rate limits for the RPC (SPEC §9, requirement 8).
//!
//! Every submitted transaction costs this node a signature verification
//! before any fee is paid, and every `/raw/blocks` request a read of the
//! block log, so a public node must bound what one client can ask of it.
//! Each client has three budgets (submissions, block-log reads, everything
//! else) and a cap on the event streams it holds open. A request over
//! budget is answered `429 Too Many Requests` with `Retry-After`.
//!
//! A client is an IPv4 address or an IPv6 /64. Its address is the peer's,
//! or, behind a proxy, the one in the header named by
//! [`Limits::client_ip_header`]. Requests from this machine itself (a
//! loopback peer, no such header) are never limited.
//!
//! This bounds one client. A flood from many addresses is the proxy's job
//! (SPEC §10.5).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;
use serde_json::json;

/// What each client may ask of the RPC. A rate of 0 means no limit.
#[derive(Clone, Debug)]
pub struct Limits {
    /// `POST /tx` requests per minute.
    pub submits_per_minute: u32,
    /// `GET /raw/blocks` requests per minute.
    pub syncs_per_minute: u32,
    /// All other requests per minute.
    pub reads_per_minute: u32,
    /// `GET /events` streams open at once. 0 means no limit.
    pub streams: u32,
    /// The header a proxy in front of this node puts the client's address
    /// in, e.g. `cf-connecting-ip`. Set it only when the proxy is the only
    /// way to reach the node: anyone who can reach the node directly can
    /// write the header themselves.
    pub client_ip_header: Option<HeaderName>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            submits_per_minute: 60,
            syncs_per_minute: 120,
            reads_per_minute: 1200,
            streams: 16,
            client_ip_header: None,
        }
    }
}

/// The kinds of request, each with a budget of its own.
#[derive(Clone, Copy)]
enum Class {
    Submit = 0,
    Sync = 1,
    Read = 2,
}

/// Whom a budget belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Client {
    V4([u8; 4]),
    /// An IPv6 /64: one subscriber holds at least that many addresses.
    V6([u8; 8]),
    /// The peer's address is not known (the server was started without
    /// connection info).
    Unknown,
    /// Every client that arrives while the table is full shares one budget.
    Crowd,
}

impl Client {
    fn of(ip: IpAddr) -> Self {
        match ip.to_canonical() {
            IpAddr::V4(v4) => Self::V4(v4.octets()),
            IpAddr::V6(v6) => {
                let o = v6.octets();
                Self::V6([o[0], o[1], o[2], o[3], o[4], o[5], o[6], o[7]])
            }
        }
    }
}

/// A budget is a bucket holding at most a minute's worth of requests, which
/// refills steadily. Counted in units of 1/60,000 of a request, so that a
/// millisecond's refill is a whole number of units at any rate.
const UNITS_PER_REQUEST: u64 = 60_000;

/// How long a bucket takes to refill completely.
const REFILL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
struct Bucket {
    units: u64,
    at: Instant,
}

impl Bucket {
    fn full(per_minute: u32, now: Instant) -> Self {
        Self {
            units: u64::from(per_minute).saturating_mul(UNITS_PER_REQUEST),
            at: now,
        }
    }

    /// Takes one request from the bucket, or says how long until there is one.
    fn take(&mut self, per_minute: u32, now: Instant) -> Result<(), Duration> {
        let rate = u64::from(per_minute);
        let elapsed =
            u64::try_from(now.saturating_duration_since(self.at).as_millis()).unwrap_or(u64::MAX);
        self.units = self
            .units
            .saturating_add(elapsed.saturating_mul(rate))
            .min(rate.saturating_mul(UNITS_PER_REQUEST));
        self.at = now;
        match self.units.checked_sub(UNITS_PER_REQUEST) {
            Some(left) => {
                self.units = left;
                Ok(())
            }
            None => {
                let short = UNITS_PER_REQUEST.saturating_sub(self.units);
                Err(Duration::from_millis(short.div_ceil(rate.max(1))))
            }
        }
    }
}

struct Entry {
    buckets: [Bucket; 3],
    streams: u32,
    seen: Instant,
}

/// Most clients remembered. One that arrives when the table is full, and no
/// entry in it has gone idle, shares the `Crowd` budget, so the table cannot
/// be grown without bound from many addresses.
const MAX_CLIENTS: usize = 50_000;

/// The limits and what each client has used of them.
pub struct Limiter {
    limits: Limits,
    clients: Mutex<HashMap<Client, Entry>>,
    max_clients: usize,
}

/// Why a request was refused.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// Over the rate; there is room again after this long.
    Rate(Duration),
    /// Too many event streams open.
    Streams,
}

impl Limiter {
    /// A limiter enforcing `limits`.
    pub fn new(limits: Limits) -> Arc<Self> {
        Self::with_capacity(limits, MAX_CLIENTS)
    }

    fn with_capacity(limits: Limits, max_clients: usize) -> Arc<Self> {
        Arc::new(Self {
            limits,
            clients: Mutex::new(HashMap::new()),
            max_clients,
        })
    }

    fn rate(&self, class: Class) -> u32 {
        match class {
            Class::Submit => self.limits.submits_per_minute,
            Class::Sync => self.limits.syncs_per_minute,
            Class::Read => self.limits.reads_per_minute,
        }
    }

    /// Counts one request of `class` from `client`, and one more open stream
    /// if `stream`.
    fn admit(
        &self,
        mut client: Client,
        class: Class,
        stream: bool,
        now: Instant,
    ) -> Result<Client, Refusal> {
        let mut clients = self.clients.lock();
        if !clients.contains_key(&client) && clients.len() >= self.max_clients {
            // An entry unused for a whole refill has full buckets again:
            // forgetting it loses nothing.
            clients.retain(|_, e| e.streams > 0 || now.saturating_duration_since(e.seen) < REFILL);
            if clients.len() >= self.max_clients {
                client = Client::Crowd;
            }
        }
        let entry = clients.entry(client).or_insert_with(|| Entry {
            buckets: [
                Bucket::full(self.limits.submits_per_minute, now),
                Bucket::full(self.limits.syncs_per_minute, now),
                Bucket::full(self.limits.reads_per_minute, now),
            ],
            streams: 0,
            seen: now,
        });
        entry.seen = now;
        if stream && self.limits.streams != 0 && entry.streams >= self.limits.streams {
            return Err(Refusal::Streams);
        }
        let per_minute = self.rate(class);
        if per_minute != 0
            && let Some(bucket) = entry.buckets.get_mut(class as usize)
        {
            bucket.take(per_minute, now).map_err(Refusal::Rate)?;
        }
        if stream {
            entry.streams = entry.streams.saturating_add(1);
        }
        Ok(client)
    }

    fn close_stream(&self, client: Client) {
        if let Some(entry) = self.clients.lock().get_mut(&client) {
            entry.streams = entry.streams.saturating_sub(1);
        }
    }

    /// Whose budget `request` counts against; `None` if it is not limited.
    fn client(&self, request: &Request) -> Option<Client> {
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0.ip());
        let behind_proxy = self
            .limits
            .client_ip_header
            .as_ref()
            .and_then(|name| request.headers().get(name))
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<IpAddr>().ok());
        match (behind_proxy, peer) {
            (Some(ip), _) => Some(Client::of(ip)),
            (None, Some(ip)) if ip.to_canonical().is_loopback() => None,
            (None, Some(ip)) => Some(Client::of(ip)),
            (None, None) => Some(Client::Unknown),
        }
    }
}

/// Held by an open `GET /events` stream; dropping it gives the slot back.
pub struct StreamSlot {
    limiter: Arc<Limiter>,
    client: Client,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        self.limiter.close_stream(self.client);
    }
}

fn refuse(status: StatusCode, message: &str, retry_after: Option<Duration>) -> Response {
    let mut response = (status, Json(json!({ "error": message }))).into_response();
    if let Some(wait) = retry_after {
        // Whole seconds, rounded up: a client that waits this long is served.
        let seconds = wait.as_millis().div_ceil(1000).max(1);
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

/// The middleware: counts the request against its client's budget, and
/// answers 429 instead of running it when the budget is spent.
pub async fn enforce(
    State(limiter): State<Arc<Limiter>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(client) = limiter.client(&request) else {
        return next.run(request).await;
    };
    let path = request.uri().path();
    let class = if request.method() == Method::POST {
        Class::Submit
    } else if path == "/raw/blocks" {
        Class::Sync
    } else {
        Class::Read
    };
    let stream = path == "/events";
    match limiter.admit(client, class, stream, Instant::now()) {
        Ok(client) => {
            if stream {
                // The handler moves this into the stream it returns.
                request.extensions_mut().insert(Arc::new(StreamSlot {
                    limiter: Arc::clone(&limiter),
                    client,
                }));
            }
            next.run(request).await
        }
        Err(Refusal::Rate(wait)) => refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "too many requests from this address; slow down",
            Some(wait),
        ),
        Err(Refusal::Streams) => refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "too many event streams open from this address",
            None,
        ),
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn limits(submits: u32) -> Limits {
        Limits {
            submits_per_minute: submits,
            streams: 2,
            ..Limits::default()
        }
    }

    fn client(n: u8) -> Client {
        Client::V4([10, 0, 0, n])
    }

    #[test]
    fn a_client_gets_a_minutes_worth_at_once_and_then_the_steady_rate() {
        let limiter = Limiter::new(limits(60));
        let t0 = Instant::now();
        for _ in 0..60 {
            assert!(limiter.admit(client(1), Class::Submit, false, t0).is_ok());
        }
        assert_eq!(
            limiter.admit(client(1), Class::Submit, false, t0),
            Err(Refusal::Rate(Duration::from_secs(1)))
        );
        // One a second from then on: not before the second is up.
        let almost = t0 + Duration::from_millis(999);
        assert!(
            limiter
                .admit(client(1), Class::Submit, false, almost)
                .is_err()
        );
        let later = t0 + Duration::from_millis(1000);
        assert!(
            limiter
                .admit(client(1), Class::Submit, false, later)
                .is_ok()
        );
        assert!(
            limiter
                .admit(client(1), Class::Submit, false, later)
                .is_err()
        );
    }

    #[test]
    fn refused_requests_do_not_push_the_next_one_back() {
        let limiter = Limiter::new(limits(1));
        let t0 = Instant::now();
        assert!(limiter.admit(client(1), Class::Submit, false, t0).is_ok());
        // Asking every 100 ms while waiting costs nothing.
        for i in 1..600 {
            let t = t0 + Duration::from_millis(i * 100);
            assert!(limiter.admit(client(1), Class::Submit, false, t).is_err());
        }
        let t = t0 + Duration::from_secs(60);
        assert!(limiter.admit(client(1), Class::Submit, false, t).is_ok());
    }

    #[test]
    fn budgets_are_per_client_and_per_kind_of_request() {
        let limiter = Limiter::new(limits(1));
        let t0 = Instant::now();
        assert!(limiter.admit(client(1), Class::Submit, false, t0).is_ok());
        assert!(limiter.admit(client(1), Class::Submit, false, t0).is_err());
        assert!(limiter.admit(client(2), Class::Submit, false, t0).is_ok());
        assert!(limiter.admit(client(1), Class::Read, false, t0).is_ok());
        assert!(limiter.admit(client(1), Class::Sync, false, t0).is_ok());
    }

    #[test]
    fn a_rate_of_zero_is_no_limit() {
        let limiter = Limiter::new(limits(0));
        let t0 = Instant::now();
        for _ in 0..10_000 {
            assert!(limiter.admit(client(1), Class::Submit, false, t0).is_ok());
        }
    }

    #[test]
    fn open_streams_are_capped_and_closing_one_frees_a_slot() {
        let limiter = Limiter::new(limits(60));
        let t0 = Instant::now();
        assert!(limiter.admit(client(1), Class::Read, true, t0).is_ok());
        assert!(limiter.admit(client(1), Class::Read, true, t0).is_ok());
        assert_eq!(
            limiter.admit(client(1), Class::Read, true, t0),
            Err(Refusal::Streams)
        );
        // Other requests still go through.
        assert!(limiter.admit(client(1), Class::Read, false, t0).is_ok());
        drop(StreamSlot {
            limiter: Arc::clone(&limiter),
            client: client(1),
        });
        assert!(limiter.admit(client(1), Class::Read, true, t0).is_ok());
    }

    #[test]
    fn a_full_table_forgets_idle_clients_and_then_shares_one_budget() {
        let limiter = Limiter::with_capacity(limits(1), 2);
        let t0 = Instant::now();
        assert_eq!(
            limiter.admit(client(1), Class::Submit, false, t0),
            Ok(client(1))
        );
        assert_eq!(
            limiter.admit(client(2), Class::Submit, false, t0),
            Ok(client(2))
        );
        // No room, nobody idle: newcomers share the crowd's budget.
        assert_eq!(
            limiter.admit(client(3), Class::Submit, false, t0),
            Ok(Client::Crowd)
        );
        assert!(limiter.admit(client(4), Class::Submit, false, t0).is_err());
        // Known clients keep their own.
        assert!(limiter.admit(client(1), Class::Read, false, t0).is_ok());
        // A minute on, the idle entries go and a newcomer has its own again.
        let later = t0 + REFILL;
        assert_eq!(
            limiter.admit(client(5), Class::Submit, false, later),
            Ok(client(5))
        );
        assert!(limiter.clients.lock().len() <= 2);
    }

    #[test]
    fn an_ipv6_subscriber_is_one_client_and_mapped_ipv4_is_ipv4() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(Client::of(a), Client::of(b));
        assert_ne!(Client::of(a), Client::of(c));
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert_eq!(Client::of(mapped), Client::V4([192, 0, 2, 7]));
    }
}
