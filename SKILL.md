---
name: katok
description: "로컬 KakaoTalk(카카오톡) 맥앱 DB 를 읽어 대화를 동기화·검색·추출하는 Rust CLI(NomaDamas/katok) 사용 지침. local-first(서버 업로드 없음). Triggers (KR/EN): 카톡, 카톡 긁기, 카톡 export, 카톡 검색, 대화 추출, katok, KakaoTalk export, kakao chat search."
---

# /katok — 로컬 카카오톡 대화 동기화·검색 CLI

`katok` 은 [NomaDamas/katok](https://github.com/NomaDamas/katok) 의 Rust CLI 다. **Apple Silicon Mac 전용**으로, KakaoTalk 맥앱이 로컬에 보관한 메시지 DB(`~/Library/Containers/com.kakao.KakaoTalkMac`)를 읽어 동기화·키워드/BM25/시맨틱 검색·청크 추출을 한다.

> **local-first.** 모든 동작은 로컬에서만 일어난다 — 대화 내용을 어떤 서버에도 업로드하지 않는다. 시맨틱 임베딩도 로컬 모델(`embeddinggemma-300m-q4`)로 돈다.

## 전제 조건

- **플랫폼:** Apple Silicon Mac 전용. KakaoTalk 맥앱(`/Applications/KakaoTalk.app`)이 설치·로그인돼 있어야 한다.
- **전체 디스크 접근(Full Disk Access) 필수.** katok 이 카톡 컨테이너 DB 를 읽으려면 **실행하는 터미널 앱**에 전체 디스크 접근 권한을 줘야 한다. 이건 사람만 할 수 있는 콘솔 권한이다(에이전트 우회 불가):
  - `System Settings → 개인정보 보호 및 보안(Privacy & Security) → 전체 디스크 접근(Full Disk Access)` 에서 **터미널(Terminal / iTerm / 사용 중인 터미널 앱)** 을 추가하고 토글 ON → 터미널 재시작.
  - 권한 없으면 `katok sync` 가 DB 를 못 읽어 실패한다. `katok doctor --json` 의 `source_adapter.macos.auth_cached` 가 권한 여부 힌트.

## 설치

```bash
# Homebrew (권장)
brew tap NomaDamas/katok https://github.com/NomaDamas/katok.git
brew trust nomadamas/katok      # untrusted tap 거부 시
brew install katok

# 실패 시 cargo
cargo install katok

# 검증
which katok          # → /opt/homebrew/bin/katok
katok --help
katok doctor --json  # 환경·권한·DB 상태 진단
```

## 사용법

전 커맨드 `--json` 플래그로 기계 판독 출력. 스크립트/에이전트는 항상 `--json` 을 붙인다.

```bash
# 1) 진단 — 설치/권한/DB/임베더 상태
katok doctor --json

# 2) 동기화 — 카톡 맥앱 DB → 로컬 인덱스 (첫 실행은 임베딩 모델 다운로드로 수 분 소요)
#    ⚠ 인덱스는 자동 갱신되지 않는다 — 마지막 sync 시점의 스냅샷이다.
#    search/chunks 로 "최신" 대화를 물을 때는 반드시 sync 를 먼저 돌린다.
katok sync --source macos --json

# 3) 대화방 목록 — 방 이름·id 확인
katok source chats --source macos --json

# 4) 검색 (세 모드)
katok search keyword  "<쿼리>" --json   # 정확 키워드 매칭
katok search bm25     "<쿼리>" --json   # BM25 랭킹 (오탈자/어순에 견고)
katok search semantic "<쿼리>" --json   # 로컬 임베딩 시맨틱 검색

# 5) 청크(메시지 묶음) 추출
katok chunk get <chunk_id> --json                  # 단일 청크
katok chunk get <chunk_id> --redact --json         # PII 마스킹 추출(전화번호 등)
katok chunk context <chunk_id> --json              # 앞뒤 맥락 포함
katok chunk parent <chunk_id> --json               # 부모 청크
katok chunks --chat <chat_id> --json               # 한 방의 전체 청크

# 6) 인덱스 관리
katok index --full --json     # 전체 재인덱스
katok wipe-index              # 인덱스 초기화
```

전형적 흐름: `doctor` 로 권한 확인 → `sync` → `source chats` 로 방 식별 → `chunks --chat <id>` 로 export 하거나 `search` 로 탐색 → `chunk get --redact` 로 민감정보 마스킹 추출.

**Sync-first 규칙 (의무).** `katok search`/`chunk`/`chunks` 는 katok 자체 인덱스를 읽는데, 이 인덱스는 카톡 DB 를 자동 추적하지 않는다. 그래서 **세션에서 처음 질의하기 전, 그리고 "오늘/최근/방금" 류 최신성 질의 전에는 항상 `katok sync --source macos --json` 를 먼저** 돌린다(증분이라 두 번째부터는 수 초). sync 없이 검색하면 마지막 sync 이후 메시지가 조용히 누락된다 — 이건 "0건"이 아니라 stale 인덱스다. 신선도의 또 다른 전제는 **카톡 맥앱 실행 중**(`pgrep -x KakaoTalk`)일 것 — 앱이 꺼져 있으면 소스 DB 자체가 새 메시지를 못 받는다. (예외: `katok-followup` 의 kf_capture 는 카톡 라이브 DB 직결이라 sync 불필요.)

## 실전 팁 (0.1.0 macOS 어댑터 기준, 검증됨 2026-07-01)

- **그룹방 제목이 대부분 `chat-<id>` 플레이스홀더로 나온다.** `source chats` 결과에서 실제 이름이 붙는 방은 극소수(예: 262개 중 11개)고 나머지는 katok 이 방 제목을 못 읽어 `chat-<chat_id>` 로 표시된다. **방 이름으로 grep 하면 못 찾는다.**
- **이름으로 못 찾는 방은 내용검색으로 역추적한다.** 방과 관련될 키워드로 `search keyword|bm25 "<키워드>"` 를 돌려 결과의 `chat_name`(=`chat-<id>`)을 방별로 집계하면, 히트가 몰린 `chat_id` 가 그 방이다. 검색 결과 각 항목은 `chunk_id`·`chat_name`·`sender_nickname`·`started_at`·`snippet` 을 준다.
- **`chunks --chat <id>` 는 본문(text)을 안 준다.** chunk 메타데이터(id·sender·timestamp·message_count)만 나온다. 방 전체를 export 하려면 chunk_id 목록을 뽑아 각각 `chunk get <chunk_id> --json` 을 돌려 `text` 를 모아 시간순으로 조립한다(수천 청크면 `chunk get` 을 8~16 병렬로).
- **`chunk get --redact` 는 PII 만이 아니라 `text` 전체를 `[redacted]` 로 마스킹한다.** 가독 export 엔 부적합하고, 정리본에 꼭 인용할 한두 줄만 마스킹할 때 쓴다.
- **KakaoTalk 시스템 피드 메시지가 섞인다.** 초대/입장/퇴장 등은 `{"inviter":...}` / `{"member":...,"feedType":N}` 같은 JSON 문자열로 들어오니, 사람이 읽을 정리본에선 걸러낸다.
- **레포/CLI 최신화:** brew stable 은 릴리스 태그(v0.1.0)까지만 준다(`brew update` 후 `brew outdated` 로 확인). 포뮬러에 `head` stanza 가 없어 `brew install --HEAD` 는 안 되고, 미릴리스 `main` 을 쓰려면 `cargo install --git https://github.com/NomaDamas/katok` 인데 그러면 brew↔cargo 이중설치로 PATH 가 갈리니 주의(둘 중 하나로 SSoT 유지).

## 보안 주의 (필수)

- **읽기 전용으로만 쓴다.** katok 은 export/검색 도구다. 카톡으로 **메시지 발송·전송은 절대 하지 않는다.**
- **카톡 대화는 PII·민감정보다.** 전화번호·실명·사적 대화가 섞일 수 있다. export 원본을 **git 에 commit 하지 않는다** — `.gitignore` 경로(`_local/`·`*.local.*`·`secrets/`)에 두고, 정리본(비밀 없는 요약)만 git 에 올린다.
- 정리본에는 원본 export 의 **경로만 참조**하고 PII 본문을 옮겨 적지 않는다. 꼭 인용해야 하면 `katok chunk get --redact` 로 마스킹한 값을 쓴다.
- katok 의 로컬 데이터 디렉토리(`~/Library/Application Support/katok`)도 카톡 내용 인덱스라 민감하다 — 백업/공유 대상 아님.

## 플랫폼 제약

- Apple Silicon(arm64) Mac 전용. Intel Mac / Linux / Windows 미지원.
- KakaoTalk **맥앱** DB 만 읽는다(모바일 전용 대화는 맥앱에 동기화돼 있어야 보인다).
