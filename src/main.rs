mod config;
mod crypto;
mod http;
mod lottery;
mod notification;
mod state;

use anyhow::{bail, Context, Result};
use config::Config;
use lottery::{week, Lottery};
use notification::{card, enqueue, error, flush};
use state::Store;

fn main() {
    if let Err(e) = run() {
        eprintln!("오류: {e:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    if command == "help" || command == "--help" {
        println!("lottery-bot <doctor|status|preflight|notify-test|notify|buy|check> [--env /absolute/path/.env]\n  doctor: 로컬 설정 확인 (외부 요청 없음)\n  status: 로그인/잔액 확인 (구매 없음)\n  preflight: 로그인/게임/구매내역 사전 점검 (구매 없음)\n  notify-test: 설치 확인 텔레그램 알림 1회\n  notify: 보관 중인 미전송 알림 전송\n  buy: 로또 COUNT게임 + 연금복권 5매 구매\n  check: 최근 14일 구매/당첨 확인 및 알림\n기본 .env는 현재 작업 디렉터리 기준, 프로세스 환경변수 우선");
        return Ok(());
    }
    let env_file = match args.next() {
        Some(arg) if arg == "--env" => args.next().context("--env 경로 필요")?,
        None => ".env".into(),
        _ => bail!("지원하지 않는 인수 (--help 참조)"),
    };
    if args.next().is_some() {
        bail!("불필요한 인수 (--help 참조)");
    }
    if ![
        "doctor",
        "status",
        "preflight",
        "notify-test",
        "notify",
        "buy",
        "check",
    ]
    .contains(&command.as_str())
    {
        bail!("알 수 없는 명령 (--help 참조)");
    }
    let config = Config::load(&env_file)?;
    let mut s = Store::open(&config.state_dir)?;
    if command == "doctor" {
        println!("설정 정상: 로또 {}게임 + 연금복권 5매, 요청 간격 {}초, 알림 {}, 상태 {}\nKST 주간 기준: {}\n외부 요청/구매 없음", config.count, config.interval_ms / 1000, if config.webhook.is_some() { "사용" } else { "미사용" }, config.state_dir.display(), week());
        s.available()?;
        return Ok(());
    }
    if command == "notify-test" {
        anyhow::ensure!(config.webhook.is_some(), "TELEGRAM_WEBHOOK_URL 설정 필요");
        let body = format!("이 컴퓨터의 Rust 복권 봇 알림 연결을 확인했습니다.\n\n📅 <b>자동 실행 일정 · KST</b>\n월요일 19:00 · 복권 구매\n토요일 22:00 · 당첨 확인\n\n🎟 로또 자동 {}게임\n🎫 연금복권 같은 번호 1~5조\n\n번호 표시 예시 (실제 구매 아님)\n<pre>A │ 01  07  13  24  32  45\nB │ 02  08  19  27  35  42</pre>\n\n이 메시지는 알림 연결 테스트입니다.\n복권 구매는 실행하지 않았습니다.", config.count);
        enqueue(
            &mut s,
            "installation-test:v1",
            card("✅ 복권 봇 · 알림 연결 확인", &body),
        )?;
        return flush(&config, &mut s);
    }
    if command == "notify" {
        return flush(&config, &mut s);
    }
    let result = execute(&config, &mut s, &command);
    // Scheduled authentication, HTTP, parsing, and purchase failures all get a real alert.
    if let Err(ref reason) = result {
        if command == "buy" || command == "check" {
            enqueue(
                &mut s,
                &format!("error:{command}:{}", week()),
                error(&command, &format!("{reason:#}")),
            )?;
        }
    }
    let delivery = if command == "buy" || command == "check" {
        flush(&config, &mut s)
    } else {
        Ok(())
    };
    if let Err(ref e) = delivery {
        eprintln!("알림: {e:#}");
    }
    result?;
    delivery
}
fn execute(config: &Config, s: &mut Store, command: &str) -> Result<()> {
    let check_key = format!("check:{}", week());
    if command == "check" && s.data.completed_checks.contains_key(&check_key) {
        println!("이번 주 확인 완료: 중복 접속 생략");
        return Ok(());
    }
    if command == "buy"
        && ["lotto645", "win720"].iter().all(|p| {
            s.data
                .purchases
                .get(&format!("{p}:week:{}", week()))
                .is_some_and(|v| v == "success")
        })
    {
        println!("이번 주 구매 처리 완료: 중복 접속 생략");
        return Ok(());
    }
    s.available()?;
    let api = Lottery::new(config)?;
    api.login(s)?;
    match command {
        "preflight" => api.preflight(s)?,
        "status" => println!("로그인 정상, 예치금 {}원", api.balance(s)?),
        "buy" => {
            for product in ["lotto645", "win720"] {
                let id = format!("purchase:{product}:{}", week());
                let message = if product == "lotto645" {
                    api.buy_lotto(s)?
                } else {
                    api.buy_pension(s)?
                };
                println!("{message}");
                enqueue(s, &id, message)?;
                // Delivery failure never prevents the other product from completing.
                if let Err(e) = flush(config, s) {
                    eprintln!("알림: {e:#}");
                }
            }
        }
        "check" => {
            for (code, name) in [("LO40", "로또 6/45"), ("LP72", "연금복권 720+")] {
                for (order, message) in api.check(s, code, name)? {
                    println!("{message}");
                    enqueue(s, &format!("result:{code}:{}:{order}", week()), message)?;
                }
            }
            s.data.completed_checks.insert(check_key, true);
            s.save()?;
        }
        _ => unreachable!(),
    }
    Ok(())
}
