//! Read-only client for The Forge API (https://sp-mod.com/docs). Public, no key.
//! Rate limits (Cloudflare, per IP): 40 req / 10 s burst, 200 req / 60 s. We stay under 30 / 10 s and 150 / 60 s,
//! honour Retry-After on 429, and cache answers for 5 minutes.

use crate::AppState;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::State;

pub const BASE: &str = "https://sp-mod.com";
pub const UA: &str = concat!("OutpostLauncher/", env!("CARGO_PKG_VERSION"));
const CACHE_FOR: Duration = Duration::from_secs(300);

pub struct Forge {
    http: reqwest::Client,
    recent: Mutex<VecDeque<Instant>>,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Default for Forge {
    fn default() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(UA)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client");
        Forge { http, recent: Mutex::new(VecDeque::new()), cache: Mutex::new(HashMap::new()) }
    }
}

impl Forge {
    async fn throttle(&self) {
        loop {
            let wait = {
                let mut q = self.recent.lock().unwrap();
                let now = Instant::now();
                while q.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
                    q.pop_front();
                }
                let last10 = q.iter().filter(|t| now.duration_since(**t) <= Duration::from_secs(10)).count();
                if last10 < 30 && q.len() < 150 {
                    q.push_back(now);
                    None
                } else {
                    Some(Duration::from_millis(500))
                }
            };
            match wait {
                None => return,
                Some(d) => tokio::time::sleep(d).await,
            }
        }
    }

    pub async fn get(&self, path: &str) -> Result<Value, String> {
        if !path.starts_with("/api/v0/") || path.contains("..") || path.contains('#') {
            return Err("Not a Forge API path.".into());
        }
        if let Some((at, v)) = self.cache.lock().unwrap().get(path) {
            if at.elapsed() < CACHE_FOR {
                return Ok(v.clone());
            }
        }
        for _ in 0..3 {
            self.throttle().await;
            let r = self
                .http
                .get(format!("{BASE}{path}"))
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(|e| format!("Could not reach The Forge: {e}"))?;
            let status = r.status();
            if status.as_u16() == 429 {
                let secs = r.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(30);
                tokio::time::sleep(Duration::from_secs(secs.clamp(1, 120))).await;
                continue;
            }
            let body = r.bytes().await.map_err(|e| format!("The Forge answer broke off: {e}"))?;
            let v: Value = serde_json::from_slice(&body).map_err(|_| format!("The Forge answered {status} without JSON."))?;
            if !status.is_success() {
                let msg = v.get("message").and_then(Value::as_str).unwrap_or("request failed");
                return Err(format!("The Forge answered {status}: {msg}"));
            }
            let mut c = self.cache.lock().unwrap();
            if c.len() > 400 {
                c.clear();
            }
            c.insert(path.to_string(), (Instant::now(), v.clone()));
            return Ok(v);
        }
        Err("The Forge is rate limiting requests. Try again in a minute.".into())
    }
}

/// Generic read-only GET for the UI; only /api/v0/ paths on sp-mod.com.
#[tauri::command]
pub async fn forge_get(state: State<'_, AppState>, path: String) -> Result<Value, String> {
    state.forge.get(&path).await
}
