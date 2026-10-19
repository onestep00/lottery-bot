use crate::{
    config::Config,
    http::cooldown_seconds,
    state::{now_ms, Store},
};
use anyhow::{bail, ensure, Context, Result};
use chrono::{FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

#[derive(Clone, Serialize, Deserialize)]
pub struct Notice {
    pub html: String,
    pub status: String,
}
pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
pub fn money(value: &str) -> String {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return escape(value);
    }
    let mut grouped = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped
}
fn balance_text(value: &str) -> String {
    if value.chars().any(|c| c.is_ascii_digit()) {
        format!("{}원", money(value))
    } else {
        escape(value)
    }
}
pub fn card(title: &str, body: &str) -> String {
    let time = Utc::now()
        .with_timezone(&FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%m.%d %H:%M KST");
    format!(
        "<b>{}</b>\n\n{}\n\n────────────\n🕒 {}",
        escape(title),
        body,
        time
    )
}
pub fn lotto_numbers(raw: &[String]) -> Result<String> {
    ensure!(
        !raw.is_empty() && raw.len() <= 5,
        "구매 번호 목록 형식 변경"
    );
    let mut lines = Vec::new();
    for (index, game) in raw.iter().enumerate() {
        let mut fields: Vec<&str> = game.split('|').collect();
        ensure!(fields.len() == 7, "구매 번호 응답 형식 변경");
        let last = fields[6];
        ensure!(last.is_ascii() && last.len() > 1, "구매 번호 형식 오류");
        fields[6] = &last[..last.len() - 1]; // last character is the selection mode
        let mut nums = Vec::new();
        for field in &fields[1..] {
            let n: u32 = field.parse().context("구매 번호 숫자 형식 오류")?;
            ensure!((1..=45).contains(&n), "구매 번호 범위 오류");
            nums.push(format!("{n:02}"));
        }
        lines.push(format!(
            "{} │ {}",
            (b'A' + index as u8) as char,
            nums.join("  ")
        ));
    }
    Ok(lines.join("\n"))
}
pub fn purchase_lotto(round: &str, count: u32, raw: &[String], balance: &str) -> String {
    let nums =
        lotto_numbers(raw).unwrap_or_else(|_| "구매 완료 · 번호는 사이트에서 확인하세요".into());
    card(
        "🎟 로또 6/45 · 구매 완료",
        &format!(
            "<b>{}회</b> · 자동 {}게임 · {}원\n\n<pre>{}</pre>\n\n💳 남은 예치금 <b>{}</b>",
            escape(round),
            count,
            money(&(count * 1000).to_string()),
            escape(&nums),
            balance_text(balance)
        ),
    )
}
pub fn purchase_pension(round: &str, number: &str, balance: &str) -> String {
    let lines = (1..=5)
        .map(|group| {
            format!(
                "{group}조 │ {}",
                number
                    .chars()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    card(
        "🎫 연금복권 720+ · 구매 완료",
        &format!(
            "<b>{}회</b> · 1~5조 같은 번호 · 5,000원\n\n<pre>{}</pre>\n\n💳 남은 예치금 <b>{}</b>",
            escape(round),
            escape(&lines),
            balance_text(balance)
        ),
    )
}
pub fn section_cards(title: &str, sections: &[String]) -> Vec<String> {
    let mut messages = Vec::new();
    let mut current = String::new();
    for section in sections {
        if !current.is_empty()
            && current.encode_utf16().count() + section.encode_utf16().count() > 3500
        {
            messages.push(card(title, &current));
            current.clear();
        }
        if !current.is_empty() {
            current.push_str("\n\n────────────\n\n");
        }
        current.push_str(section);
    }
    if !current.is_empty() {
        messages.push(card(title, &current));
    }
    messages
}
pub fn error(command: &str, reason: &str) -> String {
    let reason: String = reason.chars().take(800).collect();
    card("⚠️ 복권 봇 · 작업 중단", &format!("작업 <b>{}</b>\n\n{}\n\n구매 요청을 자동으로 반복하지 않습니다.\n결과가 불확실하면 사이트 구매내역을 확인하세요.", escape(command), escape(&reason)))
}
pub fn enqueue(s: &mut Store, id: &str, html: String) -> Result<()> {
    // Stable IDs ensure a repeated cron invocation does not produce repeated alerts.
    s.data.notifications.entry(id.into()).or_insert(Notice {
        html,
        status: "pending".into(),
    });
    s.save()
}
pub fn flush(config: &Config, s: &mut Store) -> Result<()> {
    let Some(webhook) = &config.webhook else {
        return Ok(());
    };
    let ids: Vec<String> = s
        .data
        .notifications
        .iter()
        .filter(|(_, n)| n.status == "pending")
        .map(|(id, _)| id.clone())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    ensure!(
        s.data.telegram_cooldown_until_ms <= now_ms(),
        "텔레그램 429 대기 중: 알림을 보관했습니다"
    );
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;
    let endpoint = telegram_endpoint(webhook)?;
    for id in ids {
        let html = s.data.notifications[&id].html.clone();
        ensure!(
            html.encode_utf16().count() <= 4000,
            "텔레그램 메시지 길이 초과: {id}"
        );
        let delay = (s.data.last_telegram_ms + 3100 - now_ms()).max(0) as u64;
        std::thread::sleep(Duration::from_millis(delay));
        s.data.last_telegram_ms = now_ms();
        // A lost response may already have delivered. Do not automatically replay it.
        s.data.notifications.get_mut(&id).unwrap().status = "unknown".into();
        s.save()?;
        let response = client
            .post(endpoint.clone())
            .form(&[
                ("text", html.as_str()),
                ("parse_mode", "HTML"),
                ("disable_web_page_preview", "true"),
            ])
            .send()
            .map_err(|_| {
                anyhow::anyhow!("텔레그램 전송 결과 불확실: 자동 재전송 없음 (URL 비공개)")
            })?;
        let status = response.status();
        let retry_header = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned);
        let body: serde_json::Value = response.json().unwrap_or_default();
        if status.as_u16() == 429 || body["error_code"] == 429 {
            let retry_body = body["parameters"]["retry_after"]
                .as_u64()
                .map(|n| n.to_string());
            let wait = cooldown_seconds(
                retry_header.as_deref().or(retry_body.as_deref()),
                1,
                SystemTime::now(),
            );
            s.data.telegram_cooldown_until_ms =
                now_ms().saturating_add((wait as i64).saturating_mul(1000));
            s.data.notifications.get_mut(&id).unwrap().status = "pending".into();
            s.save()?;
            bail!("텔레그램 429: {wait}초 대기, 알림을 보관했습니다 (즉시 재시도 없음)");
        }
        ensure!(
            status.is_success() && body["ok"] == true,
            "텔레그램 알림 성공 미확인 (HTTP {}, URL 비공개)",
            status.as_u16()
        );
        s.data.notifications.get_mut(&id).unwrap().status = "sent".into();
        s.save()?;
        eprintln!("텔레그램 알림 전송 완료: {id}");
    }
    Ok(())
}
fn telegram_endpoint(webhook: &str) -> Result<url::Url> {
    let mut url =
        url::Url::parse(webhook).map_err(|_| anyhow::anyhow!("텔레그램 URL 형식 오류"))?;
    // Old URLs may contain parse_mode=markdown or a previous text. Form values must win.
    let query: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !["text", "parse_mode", "disable_web_page_preview"].contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(query);
    Ok(url)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html_escapes_untrusted_values_and_formats_numbers() {
        assert_eq!(escape("<b>&\""), "&lt;b&gt;&amp;&quot;");
        assert_eq!(money("1234567"), "1,234,567");
        let raw = vec!["A|1|2|3|4|5|63".into()];
        let html = purchase_lotto("1234", 1, &raw, "12000");
        assert!(html.contains("A │ 01  02  03  04  05  06"));
        assert!(html.contains("12,000원</b>"));
    }
    #[test]
    fn old_webhook_markdown_does_not_override_html() {
        let u = telegram_endpoint("https://api.telegram.org/botTOKEN/sendMessage?chat_id=123&parse_mode=markdown&text=old").unwrap();
        assert_eq!(
            u.query_pairs().collect::<Vec<_>>(),
            vec![("chat_id".into(), "123".into())]
        );
    }
    #[test]
    fn outbox_survives_restart_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut s = Store::open(dir.path()).unwrap();
            enqueue(&mut s, "buy:round", "hello".into()).unwrap();
        }
        let mut s = Store::open(dir.path()).unwrap();
        enqueue(&mut s, "buy:round", "duplicate".into()).unwrap();
        assert_eq!(s.data.notifications.len(), 1);
        assert_eq!(s.data.notifications["buy:round"].html, "hello");
    }
}
