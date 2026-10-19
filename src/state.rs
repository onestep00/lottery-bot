use anyhow::{bail, Context, Result};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub last_request_ms: i64,
    pub cooldown_until_ms: i64,
    pub throttle_strikes: u32,
    // pending is deliberately permanent until a human reconciles an uncertain purchase.
    pub purchases: BTreeMap<String, String>,
    pub check_sequence: u64,
    pub notifications: BTreeMap<String, crate::notification::Notice>,
    pub last_telegram_ms: i64,
    pub telegram_cooldown_until_ms: i64,
}
pub struct Store {
    pub data: Data,
    path: PathBuf,
    _lock: File,
}
impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("run.lock"))?;
        lock.try_lock_exclusive()
            .context("다른 작업이 실행 중입니다")?;
        let path = dir.join("state.json");
        let data = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)
                .context("상태 파일 손상: 자동 초기화하지 않습니다")?
        } else {
            Data::default()
        };
        Ok(Self {
            data,
            path,
            _lock: lock,
        })
    }
    pub fn save(&self) -> Result<()> {
        let tmp = self.path.with_extension("tmp");
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(&self.data)?)?;
        f.sync_all()?;
        fs::rename(tmp, &self.path)?;
        File::open(self.path.parent().unwrap())?.sync_all()?;
        Ok(())
    }
    pub fn available(&self) -> Result<()> {
        if self.data.cooldown_until_ms > now_ms() {
            bail!(
                "서버 요청 중단 상태: {}초 뒤 재개 가능",
                (self.data.cooldown_until_ms - now_ms()) / 1000 + 1
            );
        }
        Ok(())
    }
    pub fn begin_check(&mut self) -> Result<String> {
        self.data.check_sequence = self
            .data
            .check_sequence
            .checked_add(1)
            .context("확인 실행 번호 초과")?;
        // A fresh explicit check supersedes stale, unsent results. Purchases are untouched.
        self.data
            .notifications
            .retain(|id, notice| !(id.starts_with("result:") && notice.status == "pending"));
        self.save()?;
        Ok(format!(
            "{}-{}",
            crate::lottery::today(),
            self.data.check_sequence
        ))
    }
    pub fn reserve(&mut self, key: &str) -> Result<()> {
        if let Some(status) = self.data.purchases.get(key) {
            bail!("{key}: 기존 구매 상태({status})가 있어 재전송하지 않습니다. 사이트 구매내역을 확인하세요");
        }
        self.data.purchases.insert(key.into(), "pending".into());
        self.save()
    }
    pub fn purchased(&mut self, key: &str) -> Result<()> {
        self.data.purchases.insert(key.into(), "success".into());
        self.save()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_survives_restart_and_blocks_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut s = Store::open(dir.path()).unwrap();
            s.reserve("lotto:1234").unwrap();
        }
        let mut s = Store::open(dir.path()).unwrap();
        assert!(s.reserve("lotto:1234").is_err());
    }
    #[test]
    fn exclusive_lock_and_corrupt_state_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        assert!(Store::open(dir.path()).is_err());
        drop(s);
        fs::write(dir.path().join("state.json"), b"broken").unwrap();
        assert!(Store::open(dir.path()).is_err());
    }
    #[test]
    fn repeated_checks_have_independent_delivery_ids_without_touching_purchases() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path()).unwrap();
        s.reserve("lotto645:round:1247").unwrap();
        let first = s.begin_check().unwrap();
        crate::notification::enqueue(&mut s, "result:old", "stale".into()).unwrap();
        crate::notification::enqueue(&mut s, "purchase:lotto", "receipt".into()).unwrap();
        let second = s.begin_check().unwrap();
        assert_ne!(first, second);
        assert!(!s.data.notifications.contains_key("result:old"));
        assert!(s.data.notifications.contains_key("purchase:lotto"));
        assert_eq!(s.data.purchases["lotto645:round:1247"], "pending");
    }
}
