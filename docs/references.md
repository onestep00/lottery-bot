# 관련 GitHub 프로젝트 조사

2026-10-19에 웹 검색과 저장소의 실제 소스를 함께 확인했습니다. 아래 커밋은 조사 당시 내려받은 HEAD입니다. 자동 실행 도중 최신 코드를 내려받거나 업데이트하지 않습니다.

| 프로젝트 | 확인한 커밋 | 확인·반영 사항 |
|---|---|---|
| [roeniss/dhlottery-api](https://github.com/roeniss/dhlottery-api) | `0d5b7566af42a1e31638650aeec44ddd232718a4` | `/login`, RSA 로그인 필드, 로그인 성공 경로, `saleMdaDcd=10`, 현재 구매 API |
| [rajephon/k-lottery](https://github.com/rajephon/k-lottery) | `bfd7c182f423271364dcadce18c17c4be987664a` | `src/klottery/endpoints.py`, `dhlottery.py`, `docs/dhlottery-api-spec.md`: `LO40`, `LP72` 조회 구분, 텍스트 요소인 `curRound`, 사이트에서 읽은 추첨일, 구매내역 `total` 검증, 추첨 전 null 결과의 의미, 대기열 감지 |
| [Torres-09/dhlottery-mcp](https://github.com/Torres-09/dhlottery-mcp) | `0b0de51442dded6486e12ba555bf872a96c4b8c1` | `internal/client/auth.go`, `buy.go`: 로그인 HTML 내 RSA 키 사용으로 별도 키 조회 절약, 게임 필드 확인 |

원본 Python 프로젝트의 연금복권 암호화/주문 프로토콜은 Rust로 옮겼습니다. 로또와 달리 최신 외부 프로젝트 소스에서 연금복권 구매 프로토콜을 추가 검증하지는 못했습니다. 연금복권 회차는 원본의 날짜 추측을 제거하고 실제 게임 페이지가 사용하는 `/selectLtEpsd.do`에서 확인하게 했습니다. 실제 사이트의 `/lotto/js/crypt/encrypt.js`도 한 번 조회하여 PBKDF2-SHA256 1,000회·AES-128-CBC·JSESSIONID 앞 32자 프로토콜을 대조했습니다. 신형 `DHJSESSIONID`를 구형 게임용 `JSESSIONID`에 연결하는 원본의 호환 처리도 유지합니다. 필드·암호화 키·응답 형식이 달라지면 결제 요청 전에 중단하도록 했습니다.

[HA-DongHang-Lottery](https://github.com/1bobby-git/HA-DongHang-Lottery)는 검색 결과에서 확인했지만, 직접 clone은 저장소 접근 불가로 실패했습니다. 이 프로젝트의 소스를 확인했다고 간주하거나 구현 근거로 사용하지 않았습니다.

새 코드는 현재 프로젝트의 동작을 이관하며 프로토콜과 필드 이름을 대조한 것입니다. 외부 저장소 소스를 통째로 복사하거나 실행 시 의존하지 않습니다. IP·프록시 전환이나 UA 순환을 사용하지 않으며, 요청 빈도와 재시도 횟수를 줄이고 차단 응답을 존중합니다.
