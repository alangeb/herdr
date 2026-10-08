use crate::noninteractive_process::curl_command;
use crate::protocol::ClientShellTabStatusSegment;
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(10);
const WINDOW: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
struct Endpoint {
    label: String,
    metrics: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Pending,
    Up,
    Down,
}

#[derive(Clone)]
struct Sample {
    when: Instant,
    gen: Option<u64>,
    gen_now: Option<f64>,
    pre: Option<u64>,
    cached: Option<u64>,
    queries: Option<u64>,
    hits: Option<u64>,
    cache_rate: Option<f64>,
    kv: Option<f64>,
}

#[derive(Clone)]
struct Snapshot {
    streams: Option<u64>,
    queue: Option<u64>,
    gen: Option<u64>,
    gen_now: Option<f64>,
    pre: Option<u64>,
    cached: Option<u64>,
    queries: Option<u64>,
    hits: Option<u64>,
    cache_rate: Option<f64>,
    kv: Option<f64>,
}

struct EpState {
    label: String,
    status: Status,
    last: Option<Sample>,
    samples: VecDeque<Sample>,
    streams: Option<u64>,
    queue: Option<u64>,
    gen: Option<f64>,
    pre: Option<f64>,
    cache: Option<u8>,
    kv: Option<u8>,
}

impl EpState {
    fn new(label: String) -> Self {
        Self {
            label,
            status: Status::Pending,
            last: None,
            samples: VecDeque::new(),
            streams: None,
            queue: None,
            gen: None,
            pre: None,
            cache: None,
            kv: None,
        }
    }

    fn update(&mut self, now: Instant, snap: Option<Snapshot>) {
        self.status = if snap.is_some() {
            Status::Up
        } else {
            Status::Down
        };
        if let Some(s) = snap {
            let cur = Sample {
                when: now,
                gen: s.gen,
                gen_now: s.gen_now,
                pre: s.pre,
                cached: s.cached,
                queries: s.queries,
                hits: s.hits,
                cache_rate: s.cache_rate,
                kv: s.kv,
            };
            if let Some(old) = self.last.clone() {
                let secs = now.duration_since(old.when).as_secs_f64().max(0.001);
                self.gen = rate(old.gen, cur.gen, secs).or(cur.gen_now);
                self.pre = rate(old.pre, cur.pre, secs);
                if let Some(x) = cur.kv {
                    self.kv = Some(usage_pct(x));
                }
            }
            self.streams = s.streams;
            self.queue = s.queue;
            self.samples.push_back(cur.clone());
            while self
                .samples
                .front()
                .is_some_and(|x| now.duration_since(x.when) > WINDOW)
            {
                self.samples.pop_front();
            }
            self.cache = cache_percent(&self.samples);
            self.last = Some(cur);
        } else {
            self.samples.clear();
            self.last = None;
            self.streams = None;
            self.queue = None;
            self.gen = None;
            self.pre = None;
            self.cache = None;
            self.kv = None;
        }
    }

    fn text(&self) -> String {
        if self.status == Status::Down {
            return format!("{}:--", self.label);
        }
        if self.status == Status::Pending {
            return format!("{}:..", self.label);
        }

        let streams = self.streams.unwrap_or(0);
        let queue = self.queue.unwrap_or(0);
        let gen = self.gen.unwrap_or(0.0);
        let pre = self.pre.unwrap_or(0.0);
        let active = streams > 0 || queue > 0 || gen >= 0.5 || pre >= 0.5;
        if !active && self.kv.unwrap_or(0) == 0 && self.cache.is_none() {
            return format!("{}:·", self.label);
        }

        let streams_text = if queue > 0 {
            format!("{streams}+{queue}")
        } else {
            format!("{streams}")
        };
        let mut parts = vec![if active {
            streams_text
        } else {
            "·".to_string()
        }];
        if active {
            if gen >= 0.5 {
                parts.push(format!("{}g", fmt_rate(Some(gen))));
            }
            if pre >= 0.5 {
                parts.push(format!("{}p", fmt_rate(Some(pre))));
            }
        }

        let kv_text = self.kv.map(|x| format!("{x}%"));
        let cache_text = self.cache.map(|x| format!("({x}%)"));
        match (kv_text, cache_text) {
            (Some(kv), Some(cache)) => parts.push(format!("{kv}{cache}")),
            (Some(kv), None) => parts.push(kv),
            (None, Some(cache)) => parts.push(cache),
            (None, None) if active => parts.push("·".to_string()),
            (None, None) => {}
        }
        format!("{}:{}", self.label, parts.join("/"))
    }
}

struct State {
    endpoints: Vec<EpState>,
    line: Option<ClientShellTabStatusSegment>,
    started: bool,
}

impl State {
    fn recompute(&mut self, alert: bool) {
        if self.endpoints.is_empty() {
            self.line = None;
            return;
        }
        let text = self
            .endpoints
            .iter()
            .map(|e| e.text())
            .collect::<Vec<_>>()
            .join(" ");
        self.line = Some(ClientShellTabStatusSegment {
            text,
            accent: alert,
        });
    }
}

static ENDPOINTS: OnceLock<Vec<Endpoint>> = OnceLock::new();
static STATE: OnceLock<RwLock<State>> = OnceLock::new();
static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub(crate) fn parse_and_strip(args: &mut Vec<String>) -> Result<(), String> {
    let mut parsed: Vec<Endpoint> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--" {
            break;
        }
        if args[i] == "--monitor" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| "missing value for --monitor".to_string())?
                .clone();
            parsed.push(endpoint_from_url(&value)?);
            args.remove(i + 1);
            args.remove(i);
            i = 0;
            continue;
        }
        if let Some(value) = args[i].strip_prefix("--monitor=") {
            parsed.push(endpoint_from_url(value)?);
            args.remove(i);
            i = 0;
            continue;
        }
        i += 1;
    }
    if parsed.is_empty() {
        return Ok(());
    }
    let mut seen = std::collections::HashSet::new();
    parsed.retain(|e| seen.insert(e.metrics.clone()));
    let mut seen_labels: HashMap<String, usize> = HashMap::new();
    for endpoint in &mut parsed {
        if let Some(n) = seen_labels.get_mut(&endpoint.label) {
            *n += 1;
            endpoint.label = format!("{}-{}", endpoint.label, *n);
        } else {
            seen_labels.insert(endpoint.label.clone(), 1);
        }
    }
    let _ = ENDPOINTS.set(parsed.clone());
    let _ = STOP.set(Arc::new(AtomicBool::new(false)));
    let endpoints = parsed
        .into_iter()
        .map(|e| EpState::new(e.label))
        .collect::<Vec<_>>();
    let mut state = State {
        endpoints,
        line: None,
        started: false,
    };
    state.recompute(false);
    let _ = STATE.set(RwLock::new(state));
    Ok(())
}

fn endpoint_from_url(url: &str) -> Result<Endpoint, String> {
    let raw = url.trim();
    let scheme = raw
        .split("://")
        .next()
        .filter(|s| *s == "http" || *s == "https")
        .ok_or_else(|| "invalid monitor URL: expected http:// or https://".to_string())?
        .to_string();
    let after = raw
        .split_once("://")
        .map(|x| x.1)
        .unwrap_or_default()
        .trim_end_matches('/');
    let (authority, path) = match after.find('/') {
        Some(i) => (after[..i].to_string(), after[i..].to_string()),
        None => (after.to_string(), String::new()),
    };
    if authority.is_empty() {
        return Err("invalid monitor URL: missing host".to_string());
    }
    let (host, port) = split_host_port(&authority, &scheme)?;
    let metrics = if path.ends_with("/metrics") {
        format!("{scheme}://{authority}{path}")
    } else {
        format!("{scheme}://{host}:{port}/metrics")
    };
    Ok(Endpoint {
        label: alias(&host, port),
        metrics,
    })
}

fn split_host_port(authority: &str, scheme: &str) -> Result<(String, u16), String> {
    if authority.starts_with('[') {
        if let Some(end) = authority.find(']') {
            let host = authority[1..end].to_string();
            let rest = authority[end + 1..].trim_start_matches(':');
            let port = if rest.is_empty() {
                default_port(scheme)
            } else {
                rest.parse()
                    .map_err(|_| "invalid monitor URL port".to_string())?
            };
            return Ok((host, port));
        }
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            return Ok((host.to_string(), port.parse().unwrap_or(0)));
        }
    }
    Ok((authority.to_string(), default_port(scheme)))
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" {
        443
    } else {
        80
    }
}

fn alias(host: &str, port: u16) -> String {
    let first = host
        .bytes()
        .find(|b| b.is_ascii_alphanumeric())
        .unwrap_or(b'h');
    format!("{}{}", first as char, port % 10)
}

pub(crate) fn ensure_started() {
    let Some(state_lock) = STATE.get() else {
        return;
    };
    {
        let mut state = state_lock.write().unwrap();
        if state.started {
            return;
        }
        state.started = true;
    }
    let Some(endpoints) = ENDPOINTS.get() else {
        return;
    };
    let Some(stop) = STOP.get() else {
        return;
    };
    for (index, endpoint) in endpoints.clone().into_iter().enumerate() {
        let stop = stop.clone();
        let _ = thread::spawn(move || poll(index, endpoint, stop));
    }
}

pub(crate) fn segment() -> Option<ClientShellTabStatusSegment> {
    STATE
        .get()
        .and_then(|lock| lock.read().ok().and_then(|s| s.line.clone()))
}

fn poll(index: usize, endpoint: Endpoint, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let snap = fetch_metrics(&endpoint.metrics)
            .ok()
            .and_then(|text| snapshot_from_text(&text));
        if let Some(lock) = STATE.get() {
            let mut state = lock.write().unwrap();
            if let Some(ep) = state.endpoints.get_mut(index) {
                ep.update(now, snap);
            }
            let alert = state
                .endpoints
                .iter()
                .any(|e| e.status == Status::Down || e.queue.unwrap_or(0) > 0);
            state.recompute(alert);
        }
        thread::sleep(POLL);
    }
}

fn fetch_metrics(url: &str) -> Result<String, ()> {
    let mut cmd = curl_command();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .args(["--fail", "--silent", "--show-error", "--max-time", "3", url]);
    let out = cmd.output().map_err(|_| ())?;
    if !out.status.success() {
        return Err(());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn fmt_rate(rate: Option<f64>) -> String {
    let x = rate.unwrap_or(0.0).max(0.0);
    if x >= 10000.0 {
        format!("{}k", (x / 1000.0).round() as u32)
    } else {
        format!("{}", x.round() as u32)
    }
}

fn rate(old: Option<u64>, cur: Option<u64>, secs: f64) -> Option<f64> {
    let (old, cur) = (old?, cur?);
    let delta = if cur >= old { cur - old } else { cur };
    Some(delta as f64 / secs.max(0.001))
}

fn usage_pct(x: f64) -> u8 {
    let p = if x > 1.5 { x } else { x * 100.0 };
    p.clamp(0.0, 100.0).round() as u8
}

fn cache_percent(samples: &VecDeque<Sample>) -> Option<u8> {
    if samples.len() >= 2 {
        let mut cached = 0.0f64;
        let mut computed = 0.0f64;
        let mut queries = 0.0f64;
        let mut hits = 0.0f64;
        let mut have_cached = false;
        let mut have_computed = false;
        let mut have_queries = false;
        let mut have_hits = false;
        for pair in samples.iter().zip(samples.iter().skip(1)) {
            if let (Some(old), Some(cur)) = (pair.0.cached, pair.1.cached) {
                cached += counter_delta(Some(old), Some(cur));
                have_cached = true;
            }
            if let (Some(old), Some(cur)) = (pair.0.pre, pair.1.pre) {
                computed += counter_delta(Some(old), Some(cur));
                have_computed = true;
            }
            if let (Some(old), Some(cur)) = (pair.0.queries, pair.1.queries) {
                queries += counter_delta(Some(old), Some(cur));
                have_queries = true;
            }
            if let (Some(old), Some(cur)) = (pair.0.hits, pair.1.hits) {
                hits += counter_delta(Some(old), Some(cur));
                have_hits = true;
            }
        }
        if have_cached && have_computed && cached + computed > 0.0 {
            return Some(usage_pct(cached / (cached + computed) * 100.0));
        }
        if have_queries && have_hits && queries > 0.0 {
            return Some(usage_pct(hits / queries * 100.0));
        }
    }
    if let Some(last) = samples.back() {
        if let (Some(cached), Some(computed)) = (last.cached, last.pre) {
            if cached + computed > 0 {
                let cached = cached as f64;
                let computed = computed as f64;
                return Some(usage_pct(cached / (cached + computed) * 100.0));
            }
        }
        if let (Some(queries), Some(hits)) = (last.queries, last.hits) {
            if queries > 0 {
                let queries = queries as f64;
                let hits = hits as f64;
                return Some(usage_pct(hits / queries * 100.0));
            }
        }
    }
    samples.back().and_then(|s| s.cache_rate).map(usage_pct)
}

fn counter_delta(old: Option<u64>, cur: Option<u64>) -> f64 {
    match (old, cur) {
        (Some(old), Some(cur)) if cur >= old => (cur - old) as f64,
        (Some(_), Some(cur)) => cur as f64,
        (None, Some(cur)) => cur as f64,
        _ => 0.0,
    }
}

fn snapshot_from_text(text: &str) -> Option<Snapshot> {
    let metrics = parse_metrics(text);
    if metrics.is_empty() {
        return None;
    }
    if metrics.iter().any(|m| m.name.starts_with("vllm:")) {
        Some(vllm_snapshot(&metrics))
    } else if metrics.iter().any(|m| m.name.starts_with("ds4_")) {
        Some(ds4_snapshot(&metrics))
    } else if metrics
        .iter()
        .any(|m| m.name.starts_with("sglang:") || m.name.starts_with("sglang_"))
    {
        Some(sglang_snapshot(&metrics))
    } else if metrics
        .iter()
        .any(|m| m.name.starts_with("llamacpp:") || m.name.starts_with("llamacpp_"))
    {
        Some(llamacpp_snapshot(&metrics))
    } else {
        None
    }
}

#[derive(Debug, Clone)]
struct MetricSample {
    name: String,
    labels: HashMap<String, String>,
    value: f64,
}

fn parse_metrics(text: &str) -> Vec<MetricSample> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(metric) = parts.next() else {
            continue;
        };
        let Some(value) = parts.next().and_then(|v| v.parse::<f64>().ok()) else {
            continue;
        };
        if !value.is_finite() {
            continue;
        }
        let (name, labels) = split_metric(metric);
        out.push(MetricSample {
            name: name.to_string(),
            labels,
            value,
        });
    }
    out
}

fn split_metric(s: &str) -> (&str, HashMap<String, String>) {
    let Some(open) = s.find('{') else {
        return (s, HashMap::new());
    };
    let body = &s[open + 1..].trim_end_matches('}');
    let mut labels = HashMap::new();
    for pair in split_top_level(body) {
        if let Some((k, v)) = pair.split_once('=') {
            labels.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    (&s[..open], labels)
}

fn split_top_level(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in body.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                cur.push(ch);
            }
            ',' if !quoted => {
                out.push(cur.clone());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn source_is(s: &HashMap<String, String>, want: &[&str]) -> bool {
    s.get("source").is_some_and(|v| want.contains(&v.as_str()))
}

fn mode_is(s: &HashMap<String, String>, want: &[&str]) -> bool {
    s.get("mode").is_some_and(|v| want.contains(&v.as_str()))
}

fn kind_is(s: &HashMap<String, String>, want: &str) -> bool {
    s.get("kind").is_some_and(|v| v == want)
}

fn sum(
    m: &[MetricSample],
    name: &str,
    filter: impl Fn(&HashMap<String, String>) -> bool,
) -> Option<u64> {
    let mut sum = 0.0f64;
    let mut any = false;
    for x in m {
        if x.name == name && filter(&x.labels) {
            any = true;
            sum += x.value;
        }
    }
    any.then_some(sum.max(0.0) as u64)
}

fn first_sum<F>(m: &[MetricSample], names: &[&str], filter: F) -> Option<u64>
where
    F: Fn(&HashMap<String, String>) -> bool + Copy,
{
    names.iter().find_map(|name| sum(m, name, filter))
}

fn max(m: &[MetricSample], name: &str) -> Option<f64> {
    let mut out = None;
    for x in m {
        if x.name == name && x.value.is_finite() {
            out = Some(out.map(|old: f64| old.max(x.value)).unwrap_or(x.value));
        }
    }
    out
}

fn first_max<F>(m: &[MetricSample], names: &[&str], filter: F) -> Option<f64>
where
    F: Fn(&HashMap<String, String>) -> bool + Copy,
{
    names.iter().find_map(|name| {
        let mut best = None;
        for x in m {
            if x.name == *name && filter(&x.labels) && x.value.is_finite() {
                best = Some(best.map(|old: f64| old.max(x.value)).unwrap_or(x.value));
            }
        }
        best
    })
}

fn vllm_snapshot(m: &[MetricSample]) -> Snapshot {
    Snapshot {
        streams: sum(m, "vllm:num_requests_running", |_| true),
        queue: sum(m, "vllm:num_requests_waiting", |_| true),
        gen: sum(m, "vllm:generation_tokens_total", |_| true),
        gen_now: None,
        pre: sum(m, "vllm:prompt_tokens_by_source_total", |l| {
            source_is(l, &["local_compute"])
        }),
        cached: sum(m, "vllm:prompt_tokens_by_source_total", |l| {
            source_is(l, &["local_cache_hit", "external_kv_transfer"])
        }),
        queries: sum(m, "vllm:prefix_cache_queries_total", |_| true),
        hits: sum(m, "vllm:prefix_cache_hits_total", |_| true),
        cache_rate: None,
        kv: max(m, "vllm:kv_cache_usage_perc"),
    }
}

fn ds4_snapshot(m: &[MetricSample]) -> Snapshot {
    Snapshot {
        streams: sum(m, "ds4_requests_inflight", |_| true),
        queue: sum(m, "ds4_queue_depth", |_| true),
        gen: sum(m, "ds4_tokens_decoded_total", |_| true),
        gen_now: max(m, "ds4_decode_tok_s"),
        pre: sum(m, "ds4_tokens_prefilled_total", |l| kind_is(l, "computed")),
        cached: sum(m, "ds4_tokens_prefilled_total", |l| kind_is(l, "cached")),
        queries: None,
        hits: None,
        cache_rate: None,
        kv: None,
    }
}

fn llamacpp_snapshot(m: &[MetricSample]) -> Snapshot {
    // llama.cpp's prompt_tokens_total excludes cached tokens: cached activity is
    // reported separately in prompt_tokens_cached_total.
    let pre = sum(m, "llamacpp:prompt_tokens_total", |_| true);
    let cached = sum(m, "llamacpp:prompt_tokens_cached_total", |_| true);
    let cache_rate = match (pre, cached) {
        (Some(computed), Some(cached)) if computed + cached > 0 => {
            Some(cached as f64 / (computed + cached) as f64 * 100.0)
        }
        _ => None,
    };

    Snapshot {
        streams: sum(m, "llamacpp:requests_processing", |_| true),
        queue: sum(m, "llamacpp:requests_deferred", |_| true),
        gen: sum(m, "llamacpp:tokens_predicted_total", |_| true),
        gen_now: max(m, "llamacpp:predicted_tokens_seconds"),
        pre,
        cached,
        queries: None,
        hits: None,
        cache_rate,
        kv: None,
    }
}

fn sglang_snapshot(m: &[MetricSample]) -> Snapshot {
    Snapshot {
        streams: first_sum(
            m,
            &[
                "sglang:num_running_reqs",
                "sglang:num_running_requests",
                "sglang_num_running_reqs",
                "sglang_num_running_requests",
            ],
            |_| true,
        ),
        queue: first_sum(
            m,
            &[
                "sglang:num_queue_reqs",
                "sglang:num_waiting_reqs",
                "sglang:num_waiting_requests",
                "sglang_num_queue_reqs",
                "sglang_num_waiting_reqs",
                "sglang_num_waiting_requests",
            ],
            |_| true,
        ),
        gen: first_sum(
            m,
            &[
                "sglang:generation_tokens_total",
                "sglang_gen_tokens_total",
                "sglang_gen_throughput_total",
            ],
            |_| true,
        ),
        gen_now: first_max(
            m,
            &[
                "sglang:gen_throughput",
                "sglang:generation_tok_s",
                "sglang_gen_throughput",
                "sglang_generation_tok_s",
            ],
            |_| true,
        ),
        pre: first_sum(
            m,
            &[
                "sglang:prefill_effective_tokens_total",
                "sglang_prefill_effective_tokens_total",
                "sglang:prefill_tokens_total",
                "sglang_prefill_tokens_total",
                "sglang:prompt_compute_tokens_total",
                "sglang_prompt_compute_tokens_total",
            ],
            |labels| {
                mode_is(labels, &["input"])
                    || source_is(labels, &["local_compute"])
                    || (!labels.contains_key("mode") && !labels.contains_key("source"))
            },
        ),
        cached: first_sum(
            m,
            &[
                "sglang:cached_tokens_total",
                "sglang_cached_tokens_total",
                "sglang:cache_hit_tokens_total",
                "sglang_cache_hit_tokens_total",
                "sglang:prompt_cache_hit_tokens_total",
                "sglang_prompt_cache_hit_tokens_total",
                "sglang:prefill_effective_tokens_total",
                "sglang_prefill_effective_tokens_total",
            ],
            |labels| !mode_is(labels, &["input"]) && !source_is(labels, &["local_compute"]),
        ),
        queries: None,
        hits: None,
        cache_rate: first_max(
            m,
            &["sglang:cache_hit_rate", "sglang_cache_hit_rate"],
            |_| true,
        ),
        kv: first_max(
            m,
            &[
                "sglang:token_usage",
                "sglang:kv_cache_usage",
                "sglang_token_usage",
                "sglang_kv_cache_usage",
            ],
            |_| true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_and_strips_monitor_args() {
        let mut argv = args(&[
            "herdr",
            "--monitor",
            "http://spark:8000/v1",
            "client",
            "--monitor=http://cuda:8002/v1",
        ]);
        parse_and_strip(&mut argv).unwrap();
        assert_eq!(argv, vec!["herdr", "client"]);
        let eps = ENDPOINTS.get().unwrap();
        assert_eq!(eps[0].label, "s0");
        assert_eq!(eps[0].metrics, "http://spark:8000/metrics");
        assert_eq!(eps[1].label, "c2");
        assert_eq!(eps[1].metrics, "http://cuda:8002/metrics");
    }

    #[test]
    fn aliases_urls() {
        assert_eq!(
            endpoint_from_url("http://spark:8000/v1").unwrap().label,
            "s0"
        );
        assert_eq!(
            endpoint_from_url("http://cuda:8001/v1").unwrap().label,
            "c1"
        );
        assert_eq!(
            endpoint_from_url("http://localhost:8080").unwrap().label,
            "l0"
        );
        assert_eq!(
            endpoint_from_url("http://10.0.0.2:8000/v1").unwrap().label,
            "10"
        );
    }

    #[test]
    fn parses_vllm_metrics() {
        let text = "vllm:num_requests_running{engine=\"0\",model_name=\"model\"} 3.0\nvllm:num_requests_waiting{engine=\"0\"} 1.0\nvllm:kv_cache_usage_perc 0.175\nvllm:generation_tokens_total 100\nvllm:prompt_tokens_by_source_total{source=\"local_compute\"} 10\nvllm:prompt_tokens_by_source_total{source=\"local_cache_hit\"} 40\nvllm:prefix_cache_queries_total 50\nvllm:prefix_cache_hits_total 40\n";
        let s = snapshot_from_text(text).unwrap();
        assert_eq!(s.streams, Some(3));
        assert_eq!(s.queue, Some(1));
        assert_eq!(s.kv, Some(0.175));
        assert_eq!(s.pre, Some(10));
        assert_eq!(s.cached, Some(40));
    }

    #[test]
    fn parses_sglang_underscore_metrics() {
        let text = "sglang_num_running_requests 4\nsglang_num_waiting_requests 2\nsglang_gen_throughput 42\nsglang_prefill_effective_tokens_total 100\nsglang_cached_tokens_total 400\nsglang_kv_cache_usage 0.32\n";
        let s = snapshot_from_text(text).unwrap();
        assert_eq!(s.streams, Some(4));
        assert_eq!(s.queue, Some(2));
        assert_eq!(s.gen_now, Some(42.0));
        assert_eq!(s.pre, Some(100));
        assert_eq!(s.cached, Some(400));
        assert_eq!(s.kv, Some(0.32));
    }

    #[test]
    fn parses_sglang_colon_metrics_with_labels() {
        let text = "sglang:cached_tokens_total{cache_source=\"device\"} 300\nsglang:cached_tokens_total{cache_source=\"host\"} 100\nsglang:prefill_effective_tokens_total{mode=\"input\"} 100\nsglang:prefill_effective_tokens_total{mode=\"device_hit\"} 300\nsglang:prefill_effective_tokens_total{mode=\"host_hit\"} 100\n";
        let s = snapshot_from_text(text).unwrap();
        assert_eq!(s.pre, Some(100));
        assert_eq!(s.cached, Some(400));
    }

    #[test]
    fn cache_percent_uses_sglang_lifetime_counters_when_window_has_one_sample() {
        let mut samples = VecDeque::new();
        samples.push_back(Sample {
            when: Instant::now(),
            gen: None,
            gen_now: None,
            pre: Some(100),
            cached: Some(1900),
            queries: None,
            hits: None,
            cache_rate: Some(0.0),
            kv: None,
        });
        assert_eq!(cache_percent(&samples), Some(95));
    }

    #[test]
    fn parses_llamacpp_metrics() {
        let text = "llamacpp:prompt_tokens_total 435259\nllamacpp:prompt_tokens_cached_total 218575\nllamacpp:tokens_predicted_total 7091\nllamacpp:predicted_tokens_seconds 0\nllamacpp:requests_processing 1\nllamacpp:requests_deferred 0\n";
        let s = snapshot_from_text(text).unwrap();
        assert_eq!(s.streams, Some(1));
        assert_eq!(s.queue, Some(0));
        assert_eq!(s.gen, Some(7091));
        assert_eq!(s.gen_now, Some(0.0));
        assert_eq!(s.pre, Some(435259));
        assert_eq!(s.cached, Some(218575));
    }

    #[test]
    fn cache_percent_uses_llamacpp_cached_and_computed_counter_window() {
        let mut samples = VecDeque::new();
        samples.push_back(Sample {
            when: Instant::now(),
            gen: None,
            gen_now: None,
            pre: Some(1000),
            cached: Some(1000),
            queries: None,
            hits: None,
            cache_rate: None,
            kv: None,
        });
        samples.push_back(Sample {
            when: Instant::now(),
            gen: None,
            gen_now: None,
            pre: Some(1500),
            cached: Some(4000),
            queries: None,
            hits: None,
            cache_rate: None,
            kv: None,
        });
        assert_eq!(cache_percent(&samples), Some(86));
    }

    #[test]
    fn formats_endpoint_blocks() {
        let mut idle = EpState::new("x0".to_string());
        idle.status = Status::Up;
        assert_eq!(idle.text(), "x0:·");

        let pending = EpState::new("x0".to_string());
        assert_eq!(pending.text(), "x0:..");

        let mut down = EpState::new("x0".to_string());
        down.status = Status::Down;
        assert_eq!(down.text(), "x0:--");

        let mut busy = EpState::new("c1".to_string());
        busy.status = Status::Up;
        busy.streams = Some(3);
        busy.queue = Some(1);
        busy.gen = Some(14.0);
        busy.pre = Some(200.0);
        busy.kv = Some(18);
        busy.cache = Some(75);
        assert_eq!(busy.text(), "c1:3+1/14g/200p/18%(75%)");

        let mut spark = EpState::new("s0".to_string());
        spark.status = Status::Up;
        spark.streams = Some(1);
        spark.gen = Some(25.0);
        spark.cache = Some(90);
        assert_eq!(spark.text(), "s0:1/25g/(90%)");

        let mut unknown_active = EpState::new("l0".to_string());
        unknown_active.status = Status::Up;
        unknown_active.streams = Some(2);
        unknown_active.gen = Some(100.0);
        assert_eq!(unknown_active.text(), "l0:2/100g/·");
    }

    #[test]
    fn parses_ds4_metrics() {
        let text = "ds4_requests_inflight 1\nds4_queue_depth 0\nds4_decode_tok_s 33.28\nds4_tokens_prefilled_total{kind=\"computed\"} 10\nds4_tokens_prefilled_total{kind=\"cached\"} 90\n";
        let s = snapshot_from_text(text).unwrap();
        assert_eq!(s.streams, Some(1));
        assert_eq!(s.gen_now, Some(33.28));
        assert_eq!(s.pre, Some(10));
        assert_eq!(s.cached, Some(90));
        assert_eq!(s.kv, None);
    }
}
