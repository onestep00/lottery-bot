use crate::{config::Config, crypto, http::Http, notification as notify, state::Store};
use anyhow::{bail, ensure, Context, Result};
use chrono::{Datelike, Duration, FixedOffset, Utc};
use regex::Regex;
use reqwest::cookie::CookieStore;
use scraper::{Html, Selector};
use serde_json::{json, Value};

pub const BASE: &str = "https://www.dhlottery.co.kr";
const GAME: &str = "https://ol.dhlottery.co.kr/olotto/game/game645.do";
const PENSION: &str = "https://el.dhlottery.co.kr/game/pension720/game.jsp";
pub fn today() -> chrono::NaiveDate {
    Utc::now()
        .with_timezone(&FixedOffset::east_opt(9 * 3600).unwrap())
        .date_naive()
}
pub fn week() -> String {
    let date = today();
    (date - Duration::days(date.weekday().num_days_from_monday() as i64)).to_string()
}
pub fn text(v: &Value) -> String {
    v.as_str().map(str::to_owned).unwrap_or_else(|| {
        if v.is_null() {
            String::new()
        } else {
            v.to_string()
        }
    })
}
fn required(v: &Value, field: &str) -> Result<String> {
    let value = text(&v[field]);
    ensure!(!value.is_empty(), "서버 응답 필수 필드 없음: {field}");
    Ok(value)
}
fn form(values: &[(&str, String)]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}
pub fn html_field(html: &str, names: &[&str]) -> Result<String> {
    let doc = Html::parse_document(html);
    for name in names {
        let sel = Selector::parse(&format!("[id=\"{name}\"], input[name=\"{name}\"]")).unwrap();
        for el in doc.select(&sel) {
            let value = el
                .value()
                .attr("value")
                .map(str::to_owned)
                .unwrap_or_else(|| el.text().collect::<String>());
            let value = value.trim().to_owned();
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    bail!(
        "게임 페이지에서 {}을 읽지 못했습니다. 회차/날짜를 추측하지 않고 중단합니다",
        names.join("/")
    )
}
fn rsa_page_field(html: &str, name: &str) -> Option<String> {
    let pattern = format!(r#"(?:var\s+)?{}\s*=\s*["']([0-9a-fA-F]+)["']"#, name);
    Regex::new(&pattern)
        .ok()?
        .captures(html)
        .map(|c| c[1].into())
}
fn cookie_value<'a>(cookies: &'a str, name: &str) -> Option<&'a str> {
    cookies
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name && !value.is_empty()).then_some(value))
}
pub struct Lottery<'a> {
    pub http: Http,
    pub config: &'a Config,
}
impl<'a> Lottery<'a> {
    pub fn new(config: &'a Config) -> Result<Self> {
        Ok(Self {
            http: Http::new(config.interval_ms)?,
            config,
        })
    }
    pub fn login(&self, s: &mut Store) -> Result<()> {
        let page = self
            .http
            .get(s, &format!("{BASE}/login"), &format!("{BASE}/main"))?;
        let (modulus, exponent) = match (
            rsa_page_field(&page.text, "rsaModulus"),
            rsa_page_field(&page.text, "publicExponent"),
        ) {
            (Some(m), Some(e)) => (m, e),
            _ => {
                let key = self
                    .http
                    .get(
                        s,
                        &format!("{BASE}/login/selectRsaModulus.do"),
                        &format!("{BASE}/login"),
                    )?
                    .json()?;
                let node = key.get("data").unwrap_or(&key);
                (
                    required(node, "rsaModulus")?,
                    required(node, "publicExponent")?,
                )
            }
        };
        let mut response = self.http.post(
            s,
            &format!("{BASE}/login/securityLoginCheck.do"),
            &form(&[
                (
                    "userId",
                    crypto::rsa_hex(&self.config.username, &modulus, &exponent)?,
                ),
                (
                    "userPswdEncn",
                    crypto::rsa_hex(&self.config.password, &modulus, &exponent)?,
                ),
                ("inpUserId", self.config.username.clone()),
            ]),
            &format!("{BASE}/login"),
        )?;
        if response.url.path() == "/mbrsrvc/ExpryPswdNoti" {
            ensure!(
                response.text.contains("/nxtChngProc.do") && response.text.contains("btnCancel"),
                "비밀번호 변경 안내 형식 변경: 자동 유예 중단"
            );
            let postponed = self
                .http
                .post_json_empty(
                    s,
                    &format!("{BASE}/mbrsrvc/nxtChngProc.do"),
                    response.url.as_str(),
                )?
                .json()?;
            ensure!(
                text(&postponed["data"]["resultCnt"])
                    .parse::<u64>()
                    .unwrap_or(0)
                    > 0,
                "비밀번호 다음에 변경 요청 실패 (반복 요청 없음)"
            );
            eprintln!("비밀번호 변경 안내: 다음에 변경 처리 완료");
            response = self.http.get(
                s,
                &format!("{BASE}/login/loginSuccess.do?returnUrl=/main"),
                response.url.as_str(),
            )?;
        }

        ensure!(
            response.url.path().contains("loginSuccess"),
            "로그인 성공을 확인하지 못했습니다. 계정/사이트 변경을 확인하세요 (재로그인 없음)"
        );
        // The legacy game encryption still reads a JSESSIONID cookie. The main
        // site also uses DHJSESSIONID; preserve the original bot's compatibility alias.
        let game_url = url::Url::parse(PENSION)?;
        let game_cookies = self
            .http
            .jar
            .cookies(&game_url)
            .and_then(|h| h.to_str().ok().map(str::to_owned))
            .unwrap_or_default();
        if cookie_value(&game_cookies, "JSESSIONID").is_none() {
            let main_cookies = self
                .http
                .jar
                .cookies(&url::Url::parse(BASE)?)
                .and_then(|h| h.to_str().ok().map(str::to_owned))
                .unwrap_or_default();
            if let Some(value) = cookie_value(&main_cookies, "JSESSIONID")
                .or_else(|| cookie_value(&main_cookies, "DHJSESSIONID"))
            {
                self.http.jar.add_cookie_str(
                    &format!("JSESSIONID={value}; Domain=.dhlottery.co.kr; Path=/; Secure"),
                    &game_url,
                );
            }
        }
        Ok(())
    }
    pub fn preflight(&self, s: &mut Store) -> Result<()> {
        println!("로그인 정상, 예치금 {}원", self.balance(s)?);
        let lotto = self.http.get(s, GAME, "https://el.dhlottery.co.kr/")?;
        println!(
            "로또 게임 페이지 정상: {}회, 추첨일 {}",
            html_field(&lotto.text, &["curRound"])?,
            html_field(&lotto.text, &["ROUND_DRAW_DATE"])?
        );
        html_field(&lotto.text, &["WAMT_PAY_TLMT_END_DT"])?;
        let pension = self.http.get(s, PENSION, &format!("{BASE}/main"))?;
        println!(
            "연금복권 게임 페이지 정상: {}회",
            self.pension_round(s, &pension.text)?
        );
        self.pension_session_key()?;
        for (code, name) in [("LO40", "로또"), ("LP72", "연금복권")] {
            println!(
                "{name} 구매내역 조회 정상: {}건",
                self.ledger(s, code)?.len()
            );
        }
        println!("사전 점검 완료 (구매/주문/번호 생성 요청 없음)");
        Ok(())
    }
    pub fn balance(&self, s: &mut Store) -> Result<String> {
        let v = self
            .http
            .get(
                s,
                &format!("{BASE}/mypage/selectUserMndp.do"),
                &format!("{BASE}/mypage/home"),
            )?
            .json()?;
        let data = v.get("data").context("잔액 조회 응답에 data 없음")?;
        let node = data.get("userMndp").unwrap_or(data);
        required(node, "totalAmt")
    }
    pub fn ledger(&self, s: &mut Store, product: &str) -> Result<Vec<Value>> {
        let mut url = url::Url::parse(&format!("{BASE}/mypage/selectMyLotteryledger.do"))?;
        url.query_pairs_mut().extend_pairs([
            (
                "srchStrDt",
                (today() - Duration::days(14)).format("%Y%m%d").to_string(),
            ),
            ("srchEndDt", today().format("%Y%m%d").to_string()),
            ("ltGdsCd", product.into()),
            ("pageNum", "1".into()),
            ("recordCountPerPage", "100".into()),
            ("sort", "".into()),
            ("winResult", "".into()),
            ("lramSmam", "".into()),
        ]);
        let v = self
            .http
            .get(s, url.as_str(), &format!("{BASE}/mypage/mylotteryledger"))?
            .json()?;
        parse_ledger(&v)
    }
    pub fn buy_lotto(&self, s: &mut Store) -> Result<String> {
        let weekly = format!("lotto645:week:{}", week());
        if let Some(status) = s.data.purchases.get(&weekly) {
            ensure!(
                status == "success",
                "로또 이전 요청 결과가 불확실합니다. 사이트 구매내역을 확인하세요"
            );
            return Ok(notify::card(
                "ℹ️ 로또 · 중복 구매 생략",
                "이번 주 구매를 이미 처리했습니다.",
            ));
        }
        let page = self.http.get(s, GAME, "https://el.dhlottery.co.kr/")?;
        let round = html_field(&page.text, &["curRound"])?;
        ensure!(round.parse::<u32>().is_ok(), "로또 회차 형식 오류");
        let draw = html_field(&page.text, &["ROUND_DRAW_DATE"])?;
        let limit = html_field(&page.text, &["WAMT_PAY_TLMT_END_DT"])?;
        let key = format!("lotto645:round:{round}");
        ensure!(
            !s.data.purchases.contains_key(&key),
            "{key} 기존 구매 상태가 있습니다"
        );
        // Also protect against purchases made by the old bot or another device.
        let ledger = self.ledger(s, "LO40")?;
        if ledger.iter().any(|row| text(&row["ltEpsd"]) == round) {
            s.purchased(&key)?;
            s.purchased(&weekly)?;
            return Ok(format!(
                "로또 {round}회: 사이트에 기존 구매내역 있음 (추가 구매 생략)"
            ));
        }
        let ready = self
            .http
            .post(
                s,
                "https://ol.dhlottery.co.kr/olotto/game/egovUserReadySocket.json",
                &[],
                GAME,
            )?
            .json()?;
        let waiting = text(&ready["ready_cnt"]).parse::<u64>().unwrap_or(0);
        ensure!(
            waiting == 0,
            "접속 대기열이 있어 구매 중단 (반복 접속 없음)"
        );
        let direct = required(&ready, "ready_ip")?;
        let games: Vec<Value> = (0..self.config.count).map(|i| json!({"genType":"0", "arrGameChoiceNum":null, "alpabet": ((b'A' + i as u8) as char).to_string()})).collect();
        let data = form(&[
            ("round", round.clone()),
            ("direct", direct),
            ("nBuyAmount", (1000 * self.config.count).to_string()),
            ("param", serde_json::to_string(&games)?),
            ("ROUND_DRAW_DATE", draw),
            ("WAMT_PAY_TLMT_END_DT", limit),
            ("gameCnt", self.config.count.to_string()),
            ("saleMdaDcd", "10".into()),
        ]);
        s.reserve(&weekly)?;
        s.reserve(&key)?;
        let v = self
            .http
            .post(
                s,
                "https://ol.dhlottery.co.kr/olotto/game/execBuy.do",
                &data,
                GAME,
            )?
            .json()?;
        ensure!(
            v["loginYn"] != "N"
                && v["isAllowed"] != "N"
                && text(&v["result"]["resultCode"]) == "100",
            "로또 구매 성공 미확인. 사이트 구매내역 확인 필요; 자동 재전송 금지"
        );
        let numbers = v["result"]["arrGameChoiceNum"]
            .as_array()
            .map(|nums| nums.iter().map(text).collect::<Vec<_>>())
            .unwrap_or_default();
        let receipt = notify::purchase_lotto(&round, self.config.count, &numbers, "조회 전");
        self.record_receipt(s, "lotto645", &weekly, &key, receipt)?;
        let balance = self.balance(s).unwrap_or_else(|_| "조회 불가".into());
        let receipt = notify::purchase_lotto(&round, self.config.count, &numbers, &balance);
        self.update_receipt(s, "lotto645", &receipt)?;
        Ok(receipt)
    }
    fn record_receipt(
        &self,
        s: &mut Store,
        product: &str,
        weekly: &str,
        round_key: &str,
        receipt: String,
    ) -> Result<()> {
        s.data.purchases.insert(weekly.into(), "success".into());
        s.data.purchases.insert(round_key.into(), "success".into());
        s.data.notifications.insert(
            format!("purchase:{product}:{}", week()),
            notify::Notice {
                html: receipt,
                status: "pending".into(),
            },
        );
        s.save()
    }
    fn update_receipt(&self, s: &mut Store, product: &str, receipt: &str) -> Result<()> {
        if let Some(notice) = s
            .data
            .notifications
            .get_mut(&format!("purchase:{product}:{}", week()))
        {
            notice.html = receipt.into();
        }
        s.save()
    }

    fn pension_round(&self, s: &mut Store, html: &str) -> Result<String> {
        let round = if let Ok(value) =
            html_field(html, &["ROUND", "round", "LT_EPSD", "DROUND", "curRound"])
        {
            value
        } else {
            ensure!(
                html.contains("/selectLtEpsd.do"),
                "연금복권 회차 조회 경로 변경"
            );
            text(
                &self
                    .http
                    .get(s, "https://el.dhlottery.co.kr/selectLtEpsd.do", PENSION)?
                    .json()?,
            )
        };
        ensure!(
            round.parse::<u32>().is_ok_and(|n| n > 0),
            "연금복권 회차 응답 형식 오류"
        );
        Ok(round)
    }
    fn pension_session_key(&self) -> Result<String> {
        let cookies = self
            .http
            .jar
            .cookies(&url::Url::parse(PENSION)?)
            .context("연금복권 세션 쿠키 없음")?;
        let cookies = cookies.to_str()?;
        cookies
            .split(';')
            .find_map(|part| part.trim().strip_prefix("JSESSIONID="))
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .context("연금복권 JSESSIONID 형식 확인 필요")
    }
    fn pension_call(&self, s: &mut Store, path: &str, payload: &str, key: &str) -> Result<Value> {
        // Preserve the original protocol's inner percent encoding in q, then form encoding.
        let encrypted = crypto::encrypt(payload, key);
        let encoded: String = url::form_urlencoded::byte_serialize(encrypted.as_bytes()).collect();
        let v = self
            .http
            .post(
                s,
                &format!("https://el.dhlottery.co.kr/{path}"),
                &form(&[("q", encoded)]),
                PENSION,
            )?
            .json()?;
        let plain = crypto::decrypt(&required(&v, "q")?, key)?;
        serde_json::from_str(&plain).context("연금복권 응답 JSON 형식 변경")
    }
    pub fn buy_pension(&self, s: &mut Store) -> Result<String> {
        let weekly = format!("win720:week:{}", week());
        if let Some(status) = s.data.purchases.get(&weekly) {
            ensure!(
                status == "success",
                "연금복권 이전 요청 결과가 불확실합니다. 사이트 구매내역을 확인하세요"
            );
            return Ok(notify::card(
                "ℹ️ 연금복권 · 중복 구매 생략",
                "이번 주 구매를 이미 처리했습니다.",
            ));
        }
        let page = self.http.get(s, PENSION, &format!("{BASE}/main"))?;
        let round = self.pension_round(s, &page.text)?;
        let purchase_key = format!("win720:round:{round}");
        ensure!(
            !s.data.purchases.contains_key(&purchase_key),
            "{purchase_key} 기존 구매 상태가 있습니다"
        );
        let ledger = self.ledger(s, "LP72")?;
        if ledger.iter().any(|row| text(&row["ltEpsd"]) == round) {
            s.purchased(&purchase_key)?;
            s.purchased(&weekly)?;
            return Ok(format!(
                "연금복권 {round}회: 기존 구매내역 있음 (추가 구매 생략)"
            ));
        }
        let key = self.pension_session_key()?;
        let auto = self.pension_call(s, "makeAutoNo.do", &format!("ROUND={round}&round={round}&LT_EPSD={round}&SEL_NO=&BUY_CNT=&AUTO_SEL_SET=SA&SEL_CLASS=&BUY_TYPE=A&ACCS_TYPE=01"), &key)?;
        let number = required(&auto, "selLotNo")?;
        ensure!(
            number.len() == 6 && number.bytes().all(|b| b.is_ascii_digit()),
            "연금복권 자동 번호 형식 오류"
        );
        // Order creation may also have side effects: reserve before either mutation.
        s.reserve(&weekly)?;
        s.reserve(&purchase_key)?;
        let order = self.pension_call(s, "makeOrderNo.do", &format!("ROUND={round}&round={round}&LT_EPSD={round}&AUTO_SEL_SET=SA&SEL_CLASS=&SEL_NO={number}&BUY_TYPE=M&BUY_CNT=5"), &key)?;
        let order_no = required(&order, "orderNo")?;
        let order_date = required(&order, "orderDate")?;
        let buy_no = (1..=5)
            .map(|i| format!("{i}{number}"))
            .collect::<Vec<_>>()
            .join("%2C");
        let encode =
            |v: &str| url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>();
        let payload = format!("ROUND={round}&FLAG=&BUY_KIND=01&BUY_NO={buy_no}&BUY_CNT=5&BUY_SET_TYPE=SA%2CSA%2CSA%2CSA%2CSA&BUY_TYPE=A%2CA%2CA%2CA%2CA%2C&CS_TYPE=01&orderNo={}&orderDate={}&TRANSACTION_ID=&WIN_DATE=&USER_ID={}&PAY_TYPE=&resultErrorCode=&resultErrorMsg=&resultOrderNo=&WORKING_FLAG=true&NUM_CHANGE_TYPE=&auto_process=N&set_type=SA&classnum=&selnum=&buytype=M&num1=&num2=&num3=&num4=&num5=&num6=&DSEC=34&CLOSE_DATE=&verifyYN=N&curdeposit=&curpay=5000&DROUND={round}&DSEC=0&CLOSE_DATE=&verifyYN=N&lotto720_radio_group=on", encode(&order_no), encode(&order_date), encode(&self.config.username));
        let result = self.pension_call(s, "connPro.do", &payload, &key)?;
        ensure!(
            text(&result["resultCode"]) == "100",
            "연금복권 구매 성공 미확인. 사이트 구매내역 확인 필요; 자동 재전송 금지"
        );
        self.record_receipt(
            s,
            "win720",
            &weekly,
            &purchase_key,
            notify::purchase_pension(&round, &number, "조회 전"),
        )?;
        let balance = self.balance(s).unwrap_or_else(|_| "조회 불가".into());
        let receipt = notify::purchase_pension(&round, &number, &balance);
        self.update_receipt(s, "win720", &receipt)?;
        Ok(receipt)
    }
    pub fn check(&self, s: &mut Store, code: &str, name: &str) -> Result<Vec<(String, String)>> {
        let rows = self.ledger(s, code)?;
        if rows.is_empty() {
            return Ok(vec![(
                "empty".into(),
                notify::card(
                    &format!("📋 {name} · 당첨 확인"),
                    "최근 14일 구매내역이 없습니다.",
                ),
            )]);
        }
        let mut messages = Vec::new();
        for row in rows {
            let round = required(&row, "ltEpsd")?;
            let result = text(&row["ltWnResult"]);
            let amount = text(&row["ltWnAmt"]);
            let pending = result.is_empty()
                || amount.is_empty()
                || result.contains("미추첨")
                || result.contains("대기");
            let status = if pending {
                "⏳ 추첨 전 / 결과 미반영".to_string()
            } else if result == "낙첨" {
                "🍀 다음 기회에!".into()
            } else {
                format!(
                    "🎉 {} · {}원",
                    notify::escape(&result),
                    notify::money(&amount)
                )
            };
            let mut url = url::Url::parse(&format!(
                "{BASE}/mypage/{}",
                if code == "LO40" {
                    "lotto645TicketDetail.do"
                } else {
                    "lottery720select.do"
                }
            ))?;
            url.query_pairs_mut()
                .append_pair("ntslOrdrNo", &required(&row, "ntslOrdrNo")?);
            if code == "LO40" {
                url.query_pairs_mut().extend_pairs([
                    ("ltGdsCd", code.to_string()),
                    ("ltEpsd", round.clone()),
                    ("barcd", required(&row, "gmInfo")?),
                    (
                        "srchStrDt",
                        (today() - Duration::days(14)).format("%Y%m%d").to_string(),
                    ),
                    ("srchEndDt", today().format("%Y%m%d").to_string()),
                ]);
            }
            let v = self
                .http
                .get(s, url.as_str(), &format!("{BASE}/mypage/mylotteryledger"))?
                .json()?;
            let data = v.get("data").context("상세 조회 응답 data 없음")?;
            let games = if code == "LO40" {
                data["ticket"]["game_dtl"].as_array()
            } else {
                data["list"].as_array()
            }
            .context("복권 상세 응답 형식 변경")?;
            let mut details = Vec::new();
            for (index, game) in games.iter().enumerate() {
                let rank = text(&game[if code == "LO40" { "rank" } else { "wnRnk" }]);
                let label = if pending {
                    "대기".into()
                } else if rank == "0" {
                    "낙첨".into()
                } else if rank.is_empty() {
                    "미확인".into()
                } else {
                    format!("{rank}등")
                };
                if code == "LO40" {
                    let winning: Vec<String> = data["ticket"]["win_num"]
                        .as_array()
                        .map(|arr| arr.iter().map(text).collect())
                        .unwrap_or_default();
                    let nums = game["num"]
                        .as_array()
                        .context("로또 번호 형식 변경")?
                        .iter()
                        .map(|n| {
                            let raw = text(n);
                            let num = format!("{:02}", raw.parse::<u32>().unwrap_or(0));
                            if !pending && winning.contains(&raw) {
                                format!("[{num}]")
                            } else {
                                format!(" {num} ")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    details.push(format!(
                        "{} │ {nums} │ {label}",
                        (b'A' + index as u8) as char
                    ));
                } else {
                    details.push(format!("{} │ {label}", text(&game["ltGmInfoCn"])));
                }
            }
            let body = format!(
                "<b>{}회 · {}</b>\n구매일 {}\n\n<pre>{}</pre>{}",
                notify::escape(&round),
                status,
                notify::escape(&text(&row["eltOrdrDt"])),
                notify::escape(&details.join("\n")),
                if code == "LO40" && !pending {
                    "\n[ ] 안의 번호는 당첨번호와 일치합니다."
                } else {
                    ""
                }
            );
            messages.push((
                required(&row, "ntslOrdrNo")?,
                notify::card(&format!("📋 {name} · 당첨 확인"), &body),
            ));
        }
        Ok(messages)
    }
}
pub fn parse_ledger(v: &Value) -> Result<Vec<Value>> {
    let data = v
        .get("data")
        .filter(|d| d.is_object())
        .context("구매내역 조회 실패: data가 없습니다 (빈 내역/낙첨으로 처리하지 않음)")?;
    let list = data["list"].as_array().context("구매내역 list 형식 변경")?;
    let total: usize = required(data, "total")?
        .parse()
        .context("구매내역 total 형식 변경")?;
    ensure!(
        total == list.len(),
        "구매내역 {total}건 중 {}건만 수신: 불완전한 내역으로 판단하지 않음",
        list.len()
    );
    Ok(list.clone())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_fields_handle_input_and_text_without_guessing() {
        assert_eq!(
            html_field("<strong id='curRound'> 1234 </strong>", &["curRound"]).unwrap(),
            "1234"
        );
        assert_eq!(
            html_field(
                "<input value='2026/10/24' name='ROUND_DRAW_DATE'>",
                &["ROUND_DRAW_DATE"]
            )
            .unwrap(),
            "2026/10/24"
        );
        assert!(html_field("<html>로그인</html>", &["curRound"]).is_err());
    }
    #[test]
    fn rejected_or_truncated_ledger_is_not_empty_or_loss() {
        assert!(parse_ledger(&json!({"data":null})).is_err());
        assert!(parse_ledger(&json!({"data":{"list":[],"total":2}})).is_err());
        assert!(parse_ledger(&json!({"data":{"list":[]}})).is_err());
        assert!(parse_ledger(&json!({"data":{"list":[],"total":"0"}}))
            .unwrap()
            .is_empty());
    }
    #[test]
    fn rsa_key_can_come_from_session_bound_login_html() {
        assert_eq!(
            rsa_page_field("var rsaModulus = 'abcdef';", "rsaModulus").unwrap(),
            "abcdef"
        );
    }
}
