use anyhow::{ensure, Context, Result};
use std::{env, path::PathBuf};

pub struct Config {
    pub username: String,
    pub password: String,
    pub count: u32,
    pub webhook: Option<String>,
    pub state_dir: PathBuf,
    pub interval_ms: u64,
}
impl Config {
    pub fn load(env_file: &str) -> Result<Self> {
        // Parse as data, never source a secrets file into a shell.
        if std::path::Path::new(env_file).exists() {
            dotenvy::from_path(env_file)
                .map_err(|_| anyhow::anyhow!(".env 구문을 확인하세요 (비밀값 비공개)"))?;
        }
        let username = env::var("USERNAME").context("USERNAME 필요")?;
        let password = env::var("PASSWORD").context("PASSWORD 필요")?;
        ensure!(
            !username.is_empty() && !password.is_empty(),
            "계정 정보가 비어 있습니다"
        );
        let count = env::var("COUNT").unwrap_or_else(|_| "5".into()).parse()?;
        ensure!((1..=5).contains(&count), "COUNT는 1~5여야 합니다");
        let interval: u64 = env::var("REQUEST_INTERVAL_SECONDS")
            .unwrap_or_else(|_| "5".into())
            .parse()?;
        ensure!(
            (5..=300).contains(&interval),
            "요청 간격은 5~300초여야 합니다"
        );
        let webhook = env::var("TELEGRAM_WEBHOOK_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        if let Some(ref value) = webhook {
            let url = url::Url::parse(value).context("알림 URL 형식 오류")?;
            ensure!(url.scheme() == "https", "알림 URL은 HTTPS여야 합니다");
        }
        Ok(Self {
            username,
            password,
            count,
            webhook,
            state_dir: env::var_os("STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| ".state".into()),
            interval_ms: interval * 1000,
        })
    }
}
