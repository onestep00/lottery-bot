use crate::state::{now_ms, Store};
use anyhow::{bail, ensure, Context, Result};
use rand::Rng;
use reqwest::{
    blocking::Client,
    cookie::Jar,
    header::{HeaderMap, HeaderValue, LOCATION, RETRY_AFTER},
    redirect::Policy,
    Method, Url,
};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

pub struct Http {
    client: Client,
    pub jar: Arc<Jar>,
    interval_ms: u64,
}
pub struct Reply {
    pub text: String,
    pub url: Url,
}
impl Reply {
    pub fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_str(&self.text)
            .context("서버 JSON 응답 형식 오류 (로그인/점검/사이트 변경 확인)")
    }
}
impl Http {
    pub fn new(interval_ms: u64) -> Result<Self> {
        let jar = Arc::new(Jar::default());
        let client = Client::builder().cookie_provider(jar.clone())
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(15)).timeout(Duration::from_secs(45))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .build()?;
        Ok(Self {
            client,
            jar,
            interval_ms,
        })
    }
    pub fn get(&self, store: &mut Store, url: &str, referer: &str) -> Result<Reply> {
        self.request(store, Method::GET, url, None, referer)
    }
    pub fn post(
        &self,
        store: &mut Store,
        url: &str,
        form: &[(String, String)],
        referer: &str,
    ) -> Result<Reply> {
        self.request(store, Method::POST, url, Some(form), referer)
    }
    pub fn post_json_empty(&self, store: &mut Store, url: &str, referer: &str) -> Result<Reply> {
        self.request(store, Method::POST, url, None, referer)
    }
    fn request(
        &self,
        store: &mut Store,
        mut method: Method,
        url: &str,
        form: Option<&[(String, String)]>,
        referer: &str,
    ) -> Result<Reply> {
        let mut current = Url::parse(url)?;
        let mut form = form;
        let original_origin = current.origin();
        for _ in 0..6 {
            store.available()?;
            // One limiter covers every host, redirect, and process restart.
            let wait =
                (store.data.last_request_ms + self.interval_ms as i64 - now_ms()).max(0) as u64;
            std::thread::sleep(Duration::from_millis(
                wait + rand::thread_rng().gen_range(100..=1000),
            ));
            store.data.last_request_ms = now_ms();
            store.save()?;
            let mut headers = HeaderMap::new();
            headers.insert("Referer", HeaderValue::from_str(referer)?);
            headers.insert(
                "Accept-Language",
                HeaderValue::from_static("ko-KR,ko;q=0.9"),
            );
            headers.insert(
                "X-Requested-With",
                HeaderValue::from_static("XMLHttpRequest"),
            );
            headers.insert(
                "Accept",
                HeaderValue::from_static("application/json, text/html;q=0.9, */*;q=0.8"),
            );
            if method == Method::POST {
                headers.insert(
                    "Origin",
                    HeaderValue::from_str(&current.origin().ascii_serialization())?,
                );
            }
            let mut req = self
                .client
                .request(method.clone(), current.clone())
                .headers(headers);
            if let Some(values) = form {
                req = req.form(values);
            } else if method == Method::POST {
                req = req.json(&serde_json::json!({}));
            }
            // No automatic request retry, especially for purchase POSTs.
            let response = req.send().map_err(|_| {
                anyhow::anyhow!("네트워크 요청 실패: {} (자동 재시도 없음)", current.path())
            })?;
            let status = response.status();
            eprintln!(
                "{} {} {}",
                chrono::Utc::now().to_rfc3339(),
                status.as_u16(),
                current.path()
            );
            if status.as_u16() == 429 || status.as_u16() == 403 {
                store.data.throttle_strikes = store.data.throttle_strikes.saturating_add(1);
                let wait = cooldown_seconds(
                    response
                        .headers()
                        .get(RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                    store.data.throttle_strikes,
                    SystemTime::now(),
                );
                store.data.cooldown_until_ms =
                    now_ms().saturating_add((wait as i64).saturating_mul(1000));
                store.save()?;
                bail!(
                    "HTTP {}: 요청 중단, 최소 {wait}초 대기. 자동 재시도 없음",
                    status.as_u16()
                );
            }
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .context("리다이렉트 주소 없음")?
                    .to_str()?;
                let next = current.join(location)?;
                ensure!(
                    next.scheme() == "https" || cfg!(test),
                    "HTTPS가 아닌 리다이렉트 거절"
                );
                ensure!(
                    next.host_str() == current.host_str()
                        || next
                            .host_str()
                            .is_some_and(|h| h.ends_with(".dhlottery.co.kr")),
                    "다른 서비스로의 리다이렉트 거절"
                );
                if status.as_u16() == 303
                    || ((status.as_u16() == 301 || status.as_u16() == 302)
                        && method == Method::POST)
                {
                    method = Method::GET;
                    form = None;
                } else if method == Method::POST {
                    bail!("POST 리다이렉트 재전송 거절: 사이트 구매내역을 확인하세요");
                }
                ensure!(
                    method != Method::POST || next.origin() == original_origin,
                    "다른 도메인에 POST 전송 거절"
                );
                current = next;
                continue;
            }
            ensure!(
                status.is_success(),
                "HTTP {}: {} (자동 재시도 없음)",
                status.as_u16(),
                current.path()
            );
            let text = response
                .text()
                .context("응답 읽기 실패 (자동 재시도 없음)")?;
            return Ok(Reply { text, url: current });
        }
        bail!("리다이렉트 횟수 초과")
    }
}
pub fn cooldown_seconds(retry: Option<&str>, strikes: u32, now: SystemTime) -> u64 {
    let backoff = 3600u64.saturating_mul(1u64 << strikes.saturating_sub(1).min(5));
    let requested = retry
        .and_then(|s| {
            s.parse::<u64>().ok().or_else(|| {
                httpdate::parse_http_date(s)
                    .ok()
                    .map(|t| t.duration_since(now).unwrap_or_default().as_secs())
            })
        })
        .unwrap_or(0);
    requested.max(backoff).min(i64::MAX as u64 / 1000)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn respects_retry_after_and_exponential_cooldown() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert_eq!(cooldown_seconds(Some("7200"), 1, now), 7200);
        assert_eq!(cooldown_seconds(Some("invalid"), 2, now), 7200);
        let date = httpdate::fmt_http_date(now + Duration::from_secs(9000));
        assert_eq!(cooldown_seconds(Some(&date), 1, now), 9000);
        assert_eq!(cooldown_seconds(Some("10"), 1, now), 3600);
    }
    fn one_response(response: String) -> (String, std::thread::JoinHandle<()>) {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0; 8192];
            let received = stream.read(&mut buf).unwrap();
            assert!(received > 0);
            stream.write_all(response.as_bytes()).unwrap();
        });
        (address, handle)
    }
    #[test]
    fn throttle_persists_and_blocks_the_next_process_before_network() {
        let (url, thread) = one_response("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7200\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
        let dir = tempfile::tempdir().unwrap();
        let http = Http::new(0).unwrap();
        {
            let mut store = Store::open(dir.path()).unwrap();
            assert!(http.get(&mut store, &url, &url).is_err());
            assert!(store.data.cooldown_until_ms >= now_ms() + 7_199_000);
        }
        thread.join().unwrap();
        let mut reopened = Store::open(dir.path()).unwrap();
        let error = http
            .get(&mut reopened, &url, &url)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("요청 중단 상태"));
    }
    #[test]
    fn purchase_post_is_not_replayed_on_307() {
        let (url, thread) = one_response("HTTP/1.1 307 Temporary Redirect\r\nLocation: /another-purchase\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.reserve("lotto645:round:1234").unwrap();
        let error = Http::new(0)
            .unwrap()
            .post(&mut store, &url, &[("amount".into(), "5000".into())], &url)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("POST 리다이렉트 재전송 거절"));
        assert_eq!(store.data.purchases["lotto645:round:1234"], "pending");
        thread.join().unwrap();
    }
    #[test]
    fn http_503_has_no_retry() {
        let (url, thread) = one_response(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into(),
        );
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let error = Http::new(0)
            .unwrap()
            .get(&mut store, &url, &url)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("HTTP 503"));
        thread.join().unwrap();
    }
}
