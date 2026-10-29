//! Multi-step bot verification over a retained artifact.
//!
//! Order per crawler comes from its preset: published IP ranges, then reverse DNS inside
//! the vendor domains with a mandatory forward confirm, then (for a few vendors) the
//! prefixes announced by an autonomous system. The first confirming step wins and its name
//! is kept as the `method`.
//!
//! Rules beyond the reference behaviour:
//! * A network failure never poisons the run. A lookup is retried with exponential backoff;
//!   after `trip` consecutive failures of one vendor only that vendor's remaining rows are
//!   reported as `network_error` without further lookups. Other vendors continue.
//! * An address outside a vendor list is `spoofed` only when no later step could still
//!   confirm it (a stale list must not condemn a real crawler); otherwise it stays
//!   `unverified` with the reason.
//! * A PTR outside the vendor domain, or a forward lookup that does not contain the
//!   address, is `spoofed`; a missing PTR is `ptr_failed`; unreachable DNS is
//!   `network_error`. Only bot groups are ever verified.
//! * Fresh DNS evidence is written to the artifact's `dns_cache`; cached evidence is
//!   reused for one hour and a cached network failure is never trusted.
//! * Lookups run on `workers` threads under the verifier's own worker gate and per-lookup
//!   timeout; `max_dns` bounds fresh lookups per call.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::bot_evidence::{cached_evidence, persist};
use crate::bots::suffixes_for;
use crate::dns::{Clock, Verifier};
use crate::error::{LogsError, Result};
use crate::net::Network;
use crate::presets::{Preset, PresetSet, Step};
use crate::ranges::{RangeBook, VerificationStatus};
use crate::store::{open_log_source, paging};
use crate::verdictcache::{VerdictCache, VERDICT_TTL};

type Evidence = Map<String, Value>;

/// How far verification may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Loaded lists and cached DNS evidence only; no network.
    Cached,
    /// Also download the range lists.
    Ranges,
    /// Live reverse+forward DNS (lists are used only if already loaded).
    Dns,
    /// Lists, DNS and ASN lookups.
    Full,
}

impl Mode {
    /// Parse `cached`, `ranges`, `dns` or `full`.
    pub fn from_name(name: &str) -> Result<Self> {
        match name {
            "cached" | "" => Ok(Self::Cached),
            "ranges" => Ok(Self::Ranges),
            "dns" => Ok(Self::Dns),
            "full" => Ok(Self::Full),
            _ => Err(LogsError::value(
                "verify_mode must be cached, ranges, dns or full",
            )),
        }
    }

    /// Whether range lists may be downloaded.
    pub fn fetch_ranges(self) -> bool {
        matches!(self, Self::Ranges | Self::Full)
    }

    fn live_dns(self) -> bool {
        matches!(self, Self::Dns | Self::Full)
    }

    fn live_asn(self) -> bool {
        self == Self::Full
    }
}

/// Prefixes announced by an autonomous system.
pub trait AsnResolver: Send + Sync {
    /// CIDR prefixes of `asn` (`AS32934`) or a description of the failure.
    fn prefixes(&self, asn: &str) -> std::result::Result<Vec<String>, String>;
}

/// Extract `route:` and `route6:` values from a whois reply.
pub fn parse_whois_routes(reply: &str) -> Vec<String> {
    reply
        .lines()
        .filter_map(|l| {
            let (key, value) = l.split_once(':')?;
            matches!(key.trim(), "route" | "route6").then(|| value.trim().to_string())
        })
        .collect()
}

/// Whois client for a RADB-style server (`-i origin AS<N>`).
pub struct Whois {
    pub server: String,
    pub timeout: Duration,
}

impl Default for Whois {
    fn default() -> Self {
        Self {
            server: "whois.radb.net:43".into(),
            timeout: Duration::from_secs(20),
        }
    }
}

impl AsnResolver for Whois {
    fn prefixes(&self, asn: &str) -> std::result::Result<Vec<String>, String> {
        let digits = asn.strip_prefix("AS").unwrap_or("");
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("{asn} is not an AS number"));
        }
        let address = self
            .server
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or("whois server has no address")?;
        let mut stream =
            TcpStream::connect_timeout(&address, self.timeout).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(format!("-i origin {asn}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        let mut reply = String::new();
        stream
            .take(4 * 1024 * 1024)
            .read_to_string(&mut reply)
            .map_err(|e| e.to_string())?;
        let routes = parse_whois_routes(&reply);
        if routes.is_empty() {
            Err(format!("whois returned no routes for {asn}"))
        } else {
            Ok(routes)
        }
    }
}

/// Outcome for one `(bot, ip)`.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub status: VerificationStatus,
    /// Step that decided: `ip_list`, `dns_reverse_forward` or `asn`; `None` when undecided.
    pub method: Option<&'static str>,
    pub hostname: Option<String>,
    pub detail: String,
    /// Evidence that should be stored in the artifact's DNS cache.
    pub fresh: Option<Evidence>,
}

fn verdict(status: VerificationStatus, method: Option<Step>, detail: impl Into<String>) -> Verdict {
    Verdict {
        status,
        method: method.map(Step::name),
        hostname: None,
        detail: detail.into(),
        fresh: None,
    }
}

enum Out {
    Pos(Verdict),
    Neg(Verdict),
    Err(Verdict),
    Skip(String),
}

/// Settings and shared state of the live (networked) part of a run.
pub struct Live {
    pub dns: bool,
    pub asn: bool,
    /// Fresh DNS lookups still allowed.
    pub budget: AtomicUsize,
    /// Extra attempts after a network failure.
    pub retries: u32,
    /// First backoff; doubles per attempt.
    pub backoff: Duration,
    /// Consecutive failed lookups after which a vendor is given up on.
    pub trip: u32,
    pub workers: usize,
    /// Smallest gap between two fresh DNS lookups across all workers; zero means
    /// unlimited. The whole-run rate is `workers / min_interval` at most.
    pub min_interval: Duration,
    failures: Mutex<HashMap<String, u32>>,
    asn_cache: Mutex<HashMap<String, std::result::Result<Vec<Network>, String>>>,
    /// Whether the resolver answers the control lookup; `None` until the first DNS
    /// step needs a live lookup.
    resolver_ok: Mutex<Option<bool>>,
    /// Monotonic time of the next allowed fresh lookup (pacing slot).
    next_slot: Mutex<f64>,
}

impl Live {
    /// Live settings for `mode` with `max_dns` fresh lookups.
    pub fn new(mode: Mode, max_dns: usize) -> Self {
        Self {
            dns: mode.live_dns(),
            asn: mode.live_asn(),
            budget: AtomicUsize::new(max_dns),
            retries: 2,
            backoff: Duration::from_millis(200),
            trip: 3,
            workers: 4,
            min_interval: Duration::ZERO,
            failures: Mutex::new(HashMap::new()),
            asn_cache: Mutex::new(HashMap::new()),
            resolver_ok: Mutex::new(None),
            next_slot: Mutex::new(0.0),
        }
    }

    /// Whether reverse DNS works from this environment at all, judged once per run by
    /// the resolver's own control lookup. `None` means no lookup was needed yet.
    pub fn resolver_state(&self) -> Option<bool> {
        self.resolver_ok.lock().ok().and_then(|g| *g)
    }

    /// The control lookup, cached for the whole run: a dead resolver must not make
    /// every row wait for its own timeout. The lookup runs under the lock so it fires
    /// exactly once even when several workers ask at the same moment.
    fn resolver_available(&self, engine: &Engine<'_>) -> bool {
        let mut state = match self.resolver_ok.lock() {
            Ok(g) => g,
            Err(_) => return true,
        };
        if let Some(ok) = *state {
            return ok;
        }
        let ok = engine.verifier.dns_available();
        *state = Some(ok);
        ok
    }

    /// Hold until the next pacing slot; keeps the global lookup rate under
    /// `1/min_interval` regardless of worker count.
    fn pace(&self, clock: &dyn Clock) {
        if self.min_interval.is_zero() {
            return;
        }
        let interval = self.min_interval.as_secs_f64();
        let wait = self
            .next_slot
            .lock()
            .map(|mut slot| {
                let now = clock.monotonic();
                let at = slot.max(now);
                *slot = at + interval;
                at - now
            })
            .unwrap_or_default();
        if wait > 0.0 {
            clock.sleep(Duration::from_secs_f64(wait));
        }
    }

    fn tripped(&self, vendor: &str) -> bool {
        self.failures
            .lock()
            .is_ok_and(|f| f.get(vendor).copied().unwrap_or(0) >= self.trip)
    }

    fn note(&self, vendor: &str, failed: bool) {
        if let Ok(mut f) = self.failures.lock() {
            if failed {
                *f.entry(vendor.to_string()).or_insert(0) += 1;
            } else {
                f.remove(vendor);
            }
        }
    }

    fn take_budget(&self) -> bool {
        self.budget
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}

/// Everything verification reads.
pub struct Engine<'a> {
    pub presets: &'a PresetSet,
    pub book: &'a RangeBook,
    pub verifier: &'a Verifier,
    pub asn: Option<&'a dyn AsnResolver>,
    pub clock: &'a dyn Clock,
}

/// Status and detail of a DNS evidence record.
pub fn classify_dns(ev: &Evidence) -> (VerificationStatus, String) {
    let reason = ev
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match ev.get("verified") {
        Some(Value::Bool(true)) => (VerificationStatus::Verified, reason),
        Some(Value::Bool(false)) => {
            let named = ev.get("hostname").is_some_and(|h| !h.is_null());
            (
                if named {
                    VerificationStatus::Spoofed
                } else {
                    VerificationStatus::PtrFailed
                },
                reason,
            )
        }
        _ => (VerificationStatus::NetworkError, reason),
    }
}

impl Engine<'_> {
    /// Verdict of `bot` seen at `ip`. `cached` is stored DNS evidence; `live` enables lookups.
    pub fn resolve(
        &self,
        bot: &str,
        ip: &str,
        cached: Option<&Evidence>,
        live: Option<&Live>,
    ) -> Verdict {
        let Some(preset) = self.presets.find(bot) else {
            return verdict(
                VerificationStatus::Unsupported,
                None,
                "no preset for this bot",
            );
        };
        if preset.steps.is_empty() || !preset.group.is_bot() {
            return verdict(
                VerificationStatus::Unsupported,
                None,
                "no verification method is published for this bot",
            );
        }
        let mut outs: Vec<(Step, Out)> = Vec::new();
        for &step in &preset.steps {
            let out = match step {
                Step::IpList => self.range_step(preset, ip),
                Step::Dns => self.dns_step(preset, bot, ip, cached, live),
                Step::Asn => self.asn_step(preset, ip, live),
            };
            let done = matches!(out, Out::Pos(_));
            outs.push((step, out));
            if done {
                break;
            }
        }
        let mut notes: Vec<String> = Vec::new();
        let (mut pos, mut err, mut neg) = (None, None, None);
        let (mut skipped, mut list_neg) = (false, false);
        for (step, out) in outs {
            match out {
                Out::Pos(v) => pos = Some(v),
                Out::Err(v) => err = Some(v),
                Out::Neg(v) => {
                    list_neg |= step == Step::IpList;
                    neg = Some(v);
                }
                Out::Skip(n) => {
                    skipped = true;
                    notes.push(n);
                }
            }
        }
        if let Some(v) = pos {
            return v;
        }
        if let Some(v) = err {
            return v;
        }
        if let Some(v) = neg {
            if list_neg && skipped {
                return verdict(
                    VerificationStatus::Unverified,
                    None,
                    format!(
                        "outside published ranges; not confirmed: {}",
                        notes.join("; ")
                    ),
                );
            }
            return v;
        }
        verdict(VerificationStatus::Unverified, None, notes.join("; "))
    }

    fn range_step(&self, p: &Preset, ip: &str) -> Out {
        let mut loaded = false;
        for list in &p.lists {
            match self.book.contains(list, ip) {
                Some(true) => {
                    return Out::Pos(verdict(
                        VerificationStatus::Verified,
                        Some(Step::IpList),
                        format!("inside the {list} range list"),
                    ))
                }
                Some(false) => loaded = true,
                None => {}
            }
        }
        if loaded {
            Out::Neg(verdict(
                VerificationStatus::Spoofed,
                Some(Step::IpList),
                "outside the published range list",
            ))
        } else {
            Out::Skip("no range list is loaded".into())
        }
    }

    fn dns_step(
        &self,
        p: &Preset,
        bot: &str,
        ip: &str,
        cached: Option<&Evidence>,
        live: Option<&Live>,
    ) -> Out {
        let owned: Vec<String> = if p.suffixes.is_empty() {
            suffixes_for(bot).iter().map(|s| s.to_string()).collect()
        } else {
            p.suffixes.clone()
        };
        if owned.is_empty() {
            return Out::Skip("no PTR domain is published".into());
        }
        let suffixes: Vec<&str> = owned.iter().map(String::as_str).collect();
        let usable = cached.filter(|e| e.get("verified").is_some_and(Value::is_boolean));
        let (ev, fresh) = match usable {
            Some(e) => (e.clone(), false),
            None => match live.filter(|l| l.dns) {
                None => return Out::Skip("no DNS evidence (live DNS is off)".into()),
                Some(l) => {
                    if !l.resolver_available(self) {
                        return Out::Err(verdict(
                            VerificationStatus::ResolverUnavailable,
                            None,
                            "the resolver does not answer the control lookup; no DNS verdict is possible in this environment",
                        ));
                    }
                    match self.lookup(l, &p.vendor, ip, &suffixes) {
                        Some(e) => (e, true),
                        None => return Out::Skip("DNS lookup budget exhausted".into()),
                    }
                }
            },
        };
        let (status, detail) = classify_dns(&ev);
        let mut v = verdict(status, Some(Step::Dns), detail);
        v.hostname = ev
            .get("hostname")
            .and_then(Value::as_str)
            .map(str::to_string);
        if fresh && status != VerificationStatus::NetworkError {
            v.fresh = Some(ev);
        }
        match status {
            VerificationStatus::Verified => Out::Pos(v),
            VerificationStatus::NetworkError => {
                v.method = None;
                Out::Err(v)
            }
            _ => Out::Neg(v),
        }
    }

    /// One lookup with retries; `None` when the budget is spent.
    fn lookup(&self, live: &Live, vendor: &str, ip: &str, suffixes: &[&str]) -> Option<Evidence> {
        if live.tripped(vendor) {
            return Some(net_error(
                "DNS is unreachable for this vendor; lookups stopped",
            ));
        }
        if !live.take_budget() {
            return None;
        }
        live.pace(self.clock);
        let mut ev = Evidence::new();
        for attempt in 0..=live.retries {
            ev = self.verifier.verify_bot_rdns(ip, suffixes);
            if ev.get("verified").is_some_and(Value::is_boolean) {
                live.note(vendor, false);
                ev.insert("checked_at".into(), json!(self.clock.wall()));
                return Some(ev);
            }
            if attempt < live.retries {
                self.clock.sleep(live.backoff * 2u32.pow(attempt));
            }
        }
        live.note(vendor, true);
        Some(ev)
    }

    fn asn_step(&self, p: &Preset, ip: &str, live: Option<&Live>) -> Out {
        let (Some(asn), Some(resolver)) = (p.asn.as_deref(), self.asn) else {
            return Out::Skip("no ASN lookup is configured".into());
        };
        let Some(live) = live.filter(|l| l.asn) else {
            return Out::Skip("ASN lookup needs verify mode full".into());
        };
        let Some(address) = crate::net::parse_ip(ip) else {
            return Out::Skip("invalid IP address".into());
        };
        let cached = live.asn_cache.lock().ok().and_then(|c| c.get(asn).cloned());
        let nets = match cached {
            Some(r) => r,
            None => {
                let mut result = Err(String::new());
                for attempt in 0..=live.retries {
                    result = resolver
                        .prefixes(asn)
                        .map(|list| list.iter().filter_map(|c| Network::parse(c).ok()).collect());
                    if result.is_ok() {
                        break;
                    }
                    if attempt < live.retries {
                        self.clock.sleep(live.backoff * 2u32.pow(attempt));
                    }
                }
                if let Ok(mut c) = live.asn_cache.lock() {
                    c.insert(asn.to_string(), result.clone());
                }
                result
            }
        };
        match nets {
            Err(e) => Out::Err(verdict(VerificationStatus::NetworkError, None, e)),
            Ok(list) if list.iter().any(|n| n.contains(address)) => Out::Pos(verdict(
                VerificationStatus::Verified,
                Some(Step::Asn),
                format!("announced by {asn}"),
            )),
            Ok(_) => Out::Neg(verdict(
                VerificationStatus::Spoofed,
                Some(Step::Asn),
                format!("not announced by {asn}"),
            )),
        }
    }

    /// Resolve many rows on `live.workers` threads; results keep input order.
    pub fn resolve_live(
        &self,
        rows: &[(String, String, Option<Evidence>)],
        live: &Live,
    ) -> Vec<Verdict> {
        let next = AtomicUsize::new(0);
        let done: Mutex<Vec<(usize, Verdict)>> = Mutex::new(Vec::new());
        // More workers than the verifier's gate admits would bounce lookups back
        // as fake failures; the gate is the real concurrency limit.
        let workers = live
            .workers
            .clamp(1, 32)
            .min(self.verifier.capacity().max(1));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some((bot, ip, cached)) = rows.get(i) else {
                        break;
                    };
                    let v = self.resolve(bot, ip, cached.as_ref(), Some(live));
                    if let Ok(mut d) = done.lock() {
                        d.push((i, v));
                    }
                });
            }
        });
        let mut all = done.into_inner().unwrap_or_default();
        all.sort_by_key(|(i, _)| *i);
        all.into_iter().map(|(_, v)| v).collect()
    }
}

fn net_error(reason: &str) -> Evidence {
    let mut m = Evidence::new();
    m.insert("verified".into(), Value::Null);
    m.insert("hostname".into(), Value::Null);
    m.insert("reason".into(), json!(reason));
    m
}

/// Request for [`bot_verification_page`].
#[derive(Debug, Clone)]
pub struct PageQuery {
    pub mode: Mode,
    /// Keep rows with this status; empty means all.
    pub status: String,
    /// Keep rows of this group or virtual group (`all_ai_bots`); empty means all.
    pub group: String,
    pub limit: i64,
    pub offset: i64,
    /// Fresh DNS lookups allowed for this call (0..=100_000).
    pub max_dns: i64,
}

impl Default for PageQuery {
    fn default() -> Self {
        Self {
            mode: Mode::Cached,
            status: String::new(),
            group: String::new(),
            limit: 100,
            offset: 0,
            max_dns: 20,
        }
    }
}

struct Row {
    bot: String,
    family: String,
    ip: String,
    visits: i64,
    last_visit: Option<String>,
    cached: Option<Evidence>,
}

/// Bot IPs of an artifact with status, group and confirmation method.
///
/// Status filtering and counts use the no-network pass; live verification then runs only
/// on the returned page, so one call never does unbounded network work.
pub fn bot_verification_page(
    source: &str,
    engine: &Engine<'_>,
    q: &PageQuery,
    now: f64,
) -> Result<Value> {
    paging(q.limit, q.offset)?;
    crate::analyze::check_range("max_dns", q.max_dns, 0, 100_000)?;
    let wanted = if q.status.is_empty() {
        None
    } else {
        Some(VerificationStatus::from_name(&q.status).ok_or_else(|| {
            LogsError::value(
                "status must be verified, spoofed, unverified, unsupported, ptr_failed, network_error or resolver_unavailable",
            )
        })?)
    };
    let mut rows = bot_rows(source, now)?;
    let in_group = |bot: &str| {
        let g = engine.presets.group_of(bot);
        q.group.is_empty() || g.name() == q.group || g.in_virtual(&q.group)
    };
    rows.retain(|r| in_group(&r.bot));
    let mut counts: Map<String, Value> = VerificationStatus::ALL
        .iter()
        .map(|s| (s.name().to_string(), json!(0)))
        .collect();
    let mut cheap: Vec<Verdict> = Vec::with_capacity(rows.len());
    for r in &rows {
        let v = engine.resolve(&r.bot, &r.ip, r.cached.as_ref(), None);
        if let Some(n) = counts.get_mut(v.status.name()) {
            *n = json!(n.as_i64().unwrap_or(0) + 1);
        }
        cheap.push(v);
    }
    let picked: Vec<usize> = (0..rows.len())
        .filter(|&i| wanted.is_none_or(|w| w == cheap[i].status))
        .collect();
    let total = picked.len() as i64;
    let page: Vec<usize> = picked
        .into_iter()
        .skip(q.offset as usize)
        .take(q.limit as usize)
        .collect();
    let mut shown: Vec<Verdict> = page.iter().map(|&i| cheap[i].clone()).collect();
    let mut live_used = false;
    if q.mode.live_dns() || q.mode.live_asn() {
        let todo: Vec<usize> = (0..page.len())
            .filter(|&k| {
                matches!(
                    shown[k].status,
                    VerificationStatus::Unverified | VerificationStatus::NetworkError
                )
            })
            .collect();
        let input: Vec<(String, String, Option<Evidence>)> = todo
            .iter()
            .map(|&k| {
                let r = &rows[page[k]];
                (r.bot.clone(), r.ip.clone(), r.cached.clone())
            })
            .collect();
        let live = Live::new(q.mode, q.max_dns as usize);
        let fresh = engine.resolve_live(&input, &live);
        let mut writes: Vec<Evidence> = Vec::new();
        for (k, v) in todo.into_iter().zip(fresh) {
            if let Some(ev) = &v.fresh {
                let mut w = ev.clone();
                w.insert("bot".into(), json!(rows[page[k]].bot));
                w.insert("ip".into(), json!(rows[page[k]].ip));
                writes.push(w);
            }
            shown[k] = v;
        }
        live_used = true;
        let refs: Vec<&Evidence> = writes.iter().collect();
        if !refs.is_empty() {
            persist(source, &refs)?;
        }
    }
    let out_rows: Vec<Value> = page
        .iter()
        .zip(&shown)
        .map(|(&i, v)| {
            let r = &rows[i];
            json!({
                "bot": r.bot, "family": r.family, "ip": r.ip, "visits": r.visits,
                "last_visit": r.last_visit,
                "group": engine.presets.group_of(&r.bot).name(),
                "status": v.status.name(), "status_code": v.status.code(),
                "method": v.method, "hostname": v.hostname, "detail": v.detail,
            })
        })
        .collect();
    let more = q.offset + (out_rows.len() as i64) < total;
    Ok(json!({
        "ok": true, "source": source, "mode": format!("{:?}", q.mode).to_lowercase(),
        "rows": out_rows, "total": total, "limit": q.limit, "offset": q.offset,
        "has_more": more, "next_offset": if more { json!(q.offset + q.limit) } else { Value::Null },
        "status_counts": counts, "live": live_used,
        "identity_note": "Bot names are user-agent claims; only verified rows are authenticated",
    }))
}

/// Request for [`bot_verification_all`].
#[derive(Debug, Clone)]
pub struct AllQuery {
    pub mode: Mode,
    /// Keep rows of this group or virtual group; empty means all.
    pub group: String,
    /// Resolver worker threads (1..=32).
    pub workers: i64,
    /// Total fresh DNS lookups allowed for the run (0..=1_000_000).
    pub max_dns: i64,
    /// Fresh DNS lookups per second across all workers; 0 means unlimited (0..=10_000).
    pub dns_per_second: i64,
}

impl Default for AllQuery {
    fn default() -> Self {
        Self {
            mode: Mode::Full,
            group: String::new(),
            workers: 8,
            max_dns: 100_000,
            dns_per_second: 0,
        }
    }
}

/// Rows of `bot_ips` with their usable cached DNS evidence.
fn bot_rows(source: &str, now: f64) -> Result<Vec<Row>> {
    let con = open_log_source(source)?;
    let mut rows: Vec<Row> = Vec::new();
    let mut stmt = con.prepare(
        "SELECT b.bot,b.family,b.ip,b.visits,b.last_visit,c.payload,c.checked_at
         FROM bot_ips b LEFT JOIN dns_cache c ON b.ip=c.ip AND b.bot=c.bot
         ORDER BY b.visits DESC,b.bot,b.ip",
    )?;
    let mut records = stmt.query([])?;
    while let Some(r) = records.next()? {
        let payload: Option<String> = r.get(5)?;
        let stamp: Option<f64> = r.get(6)?;
        let ev = cached_evidence(payload.as_deref(), stamp, now);
        let usable = ev.get("cached") == Some(&json!(true));
        rows.push(Row {
            bot: r.get(0)?,
            family: r.get(1)?,
            ip: r.get(2)?,
            visits: r.get(3)?,
            last_visit: r.get(4)?,
            cached: usable.then_some(ev),
        });
    }
    Ok(rows)
}

/// Statuses worth writing back: decided verdicts only — environment and budget
/// outcomes are state, not evidence.
fn decisive(v: &Verdict) -> bool {
    matches!(
        v.status,
        VerificationStatus::Verified | VerificationStatus::Spoofed | VerificationStatus::PtrFailed
    ) && v.method.is_some()
}

/// Evidence origin label stored beside a verdict.
fn origin_of(method: &str) -> &'static str {
    match method {
        "ip_list" => "ip-list",
        "dns_reverse_forward" => "rdns+forward",
        _ => "asn",
    }
}

/// The verdict-cache row of a decisive verdict.
fn verdict_row(bot: &str, ip: &str, v: &Verdict, now: f64) -> crate::verdictcache::Row {
    let method = v.method.unwrap_or_default();
    crate::verdictcache::Row {
        bot: bot.to_string(),
        ip: ip.to_string(),
        method: method.to_string(),
        status: v.status.name().to_string(),
        hostname: v.hostname.clone(),
        detail: v.detail.clone(),
        source: origin_of(method).to_string(),
        checked_at: now,
        ttl: VERDICT_TTL,
    }
}

/// Verify every bot–IP pair of an artifact in one run.
///
/// Fresh rows of the artifact's verdict cache are served without any work. What
/// remains is judged once: cheap steps (loaded lists, cached DNS evidence) first,
/// then the live steps on `workers` threads under a global lookup budget and rate.
/// Decided verdicts persist after every chunk of 256 rows, so an interrupted run
/// resumes where it stopped. The verdict cache never holds non-verdicts.
pub fn bot_verification_all(
    source: &str,
    engine: &Engine<'_>,
    q: &AllQuery,
    now: f64,
    progress: &dyn Fn(usize, usize),
) -> Result<Value> {
    crate::analyze::check_range("workers", q.workers, 1, 32)?;
    crate::analyze::check_range("max_dns", q.max_dns, 0, 1_000_000)?;
    crate::analyze::check_range("dns_per_second", q.dns_per_second, 0, 10_000)?;
    let rows = bot_rows(source, now)?;
    let in_group = |bot: &str| {
        let g = engine.presets.group_of(bot);
        q.group.is_empty() || g.name() == q.group || g.in_virtual(&q.group)
    };
    let rows: Vec<Row> = rows.into_iter().filter(|r| in_group(&r.bot)).collect();
    let cache = VerdictCache::for_source(source);
    let mut cache_error: Option<String> = None;
    let mut hits: HashMap<(String, String), crate::verdictcache::Row> = match cache.fresh(now) {
        Ok(found) => {
            let mut by_pair: HashMap<(String, String), crate::verdictcache::Row> = HashMap::new();
            for row in found {
                by_pair
                    .entry((row.bot.clone(), row.ip.clone()))
                    .and_modify(|old| {
                        if row.checked_at > old.checked_at {
                            *old = row.clone();
                        }
                    })
                    .or_insert(row);
            }
            by_pair
        }
        Err(e) => {
            cache_error = Some(format!("verdict cache unreadable: {e}"));
            HashMap::new()
        }
    };
    let mut verdicts: Vec<Verdict> = Vec::with_capacity(rows.len());
    let mut pending: Vec<(usize, String, String, Option<Evidence>)> = Vec::new();
    let mut cheap_writes: Vec<crate::verdictcache::Row> = Vec::new();
    let mut cache_hits = 0usize;
    let live_steps = q.mode.live_dns() || q.mode.live_asn();
    for (i, r) in rows.iter().enumerate() {
        if let Some(hit) = hits.remove(&(r.bot.clone(), r.ip.clone())) {
            cache_hits += 1;
            verdicts.push(Verdict {
                status: VerificationStatus::from_name(&hit.status)
                    .unwrap_or(VerificationStatus::Unverified),
                method: match hit.method.as_str() {
                    "ip_list" => Some("ip_list"),
                    "dns_reverse_forward" => Some("dns_reverse_forward"),
                    "asn" => Some("asn"),
                    _ => None,
                },
                hostname: hit.hostname,
                detail: hit.detail,
                fresh: None,
            });
            continue;
        }
        let v = engine.resolve(&r.bot, &r.ip, r.cached.as_ref(), None);
        if live_steps
            && matches!(
                v.status,
                VerificationStatus::Unverified | VerificationStatus::NetworkError
            )
        {
            pending.push((i, r.bot.clone(), r.ip.clone(), r.cached.clone()));
        } else if decisive(&v) {
            cheap_writes.push(verdict_row(&r.bot, &r.ip, &v, now));
        }
        verdicts.push(v);
    }
    let total = rows.len();
    let mut done = total - pending.len();
    progress(done, total);
    let mut live = Live::new(q.mode, q.max_dns as usize);
    live.workers = q.workers as usize;
    if q.dns_per_second > 0 {
        live.min_interval = Duration::from_secs_f64(1.0 / q.dns_per_second as f64);
    }
    let mut stored = 0usize;
    let mut dns_written = 0usize;
    for chunk in pending.chunks(256) {
        let input: Vec<(String, String, Option<Evidence>)> = chunk
            .iter()
            .map(|(_, bot, ip, cached)| (bot.clone(), ip.clone(), cached.clone()))
            .collect();
        let outs = engine.resolve_live(&input, &live);
        let mut dns_writes: Vec<Evidence> = Vec::new();
        let mut verdict_writes: Vec<crate::verdictcache::Row> = Vec::new();
        for ((i, bot, ip, _), v) in chunk.iter().zip(outs) {
            if let Some(ev) = &v.fresh {
                let mut w = ev.clone();
                w.insert("bot".into(), json!(bot));
                w.insert("ip".into(), json!(ip));
                dns_writes.push(w);
            }
            if decisive(&v) {
                verdict_writes.push(verdict_row(bot, ip, &v, now));
            }
            verdicts[*i] = v;
        }
        verdict_writes.append(&mut cheap_writes);
        if !dns_writes.is_empty() {
            let refs: Vec<&Evidence> = dns_writes.iter().collect();
            persist(source, &refs)?;
            dns_written += refs.len();
        }
        match cache.store(&verdict_writes, now) {
            Ok(n) => stored += n,
            Err(e) if cache_error.is_none() => {
                cache_error = Some(format!("verdict cache not written: {e}"));
            }
            Err(_) => {}
        }
        done += chunk.len();
        progress(done, total);
    }
    if !cheap_writes.is_empty() {
        match cache.store(&cheap_writes, now) {
            Ok(n) => stored += n,
            Err(e) if cache_error.is_none() => {
                cache_error = Some(format!("verdict cache not written: {e}"));
            }
            Err(_) => {}
        }
    }
    let mut counts: Map<String, Value> = VerificationStatus::ALL
        .iter()
        .map(|s| (s.name().to_string(), json!(0)))
        .collect();
    let mut methods: Map<String, Value> = Map::new();
    for v in &verdicts {
        if let Some(slot) = counts.get_mut(v.status.name()) {
            *slot = json!(slot.as_i64().unwrap_or(0) + 1);
        }
        let key = v.method.unwrap_or("none");
        *methods.entry(key.to_string()).or_insert(json!(0)) =
            json!(methods.get(key).and_then(Value::as_i64).unwrap_or(0) + 1);
    }
    let lookups = q.max_dns as usize - live.budget.load(Ordering::SeqCst);
    Ok(json!({
        "ok": true, "source": source,
        "mode": format!("{:?}", q.mode).to_lowercase(),
        "total": total, "done": done,
        "status_counts": counts,
        "method_counts": methods,
        "live": {
            "enabled": live_steps,
            "dns_lookups": lookups,
            "max_dns": q.max_dns,
            "workers": q.workers,
            "dns_per_second": q.dns_per_second,
            "resolver_available": live.resolver_state(),
        },
        "verdict_cache": {
            "path": cache.path.to_string_lossy(),
            "hits": cache_hits,
            "stored": stored,
            "dns_evidence_written": dns_written,
            "ttl": VERDICT_TTL,
            "error": cache_error,
        },
        "identity_note": "Bot names are user-agent claims; only verified rows are authenticated",
    }))
}
