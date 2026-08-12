# katok

`katok`은 Apple Silicon Mac에서 카카오톡 대화를 로컬로 읽고, 키워드·벡터
검색과 사용자가 명시적으로 승인한 메시지 전송을 제공하는 CLI입니다.

읽기·검색·인덱싱을 위해 카카오톡 대화 내용을 별도의 `katok` 서버로 올리지
않습니다. macOS에 저장된 카카오톡 DB를 읽어 개인 Mac 안에 정규화된
아카이브와 검색 인덱스를 만들고, `katok search ...` 명령으로 필요한 대화를
찾습니다.

`katok send`는 카카오 원격 서버의 비공개 프로토콜이나 비공식 API를 직접
호출하지 않고, 사용자의 Mac에서 실행 중인 공식 KakaoTalk 앱을 macOS
Accessibility로 조작합니다. 이는 카카오의 승인이나 이용제한 면제를 의미하지
않습니다. 실제 사용 전에 [허용 사용 정책](ACCEPTABLE_USE_POLICY.md)과
[면책 고지](DISCLAIMER.md)를 읽으십시오.

## 무엇을 해주나

- 카카오톡 macOS 앱의 로컬 DB를 읽어 대화 아카이브를 만듭니다.
- 정확한 단어 매칭용 `keyword`, SQLite FTS5 기반 `bm25`, EmbeddingGemma 기반 `semantic` 검색을 제공합니다.
- 긴 대화는 카카오톡 흐름에 맞게 chunk로 나누고, 5분 안팎의 같은 채팅방 대화는 parent window로 묶어 벡터 검색 품질을 높입니다.
- 검색 결과는 짧은 snippet과 chunk id만 보여줍니다. 원문 전체는 사용자가 명시적으로 `katok chunk get <chunk-id>`를 실행할 때만 출력합니다.
- 에이전트는 Vercel Agent Skills/Codex Skills에서 `skills/katok/SKILL.md`를 통해 CLI만 호출하면 됩니다.
- 명시적인 정책 동의와 macOS Accessibility 권한 아래에서 텍스트·이미지를
  공식 KakaoTalk 앱 UI로 전달할 수 있습니다.

## 지원 환경

- Apple Silicon Mac
- macOS 카카오톡 앱
- 터미널 앱의 전체 디스크 접근 권한

Intel Mac은 지원하지 않습니다. 현재 로컬 임베딩 경로가 `fastembed`와 ONNX Runtime을 사용하며, 이 dependency set은 `x86_64-apple-darwin`용 prebuilt ONNX Runtime을 제공하지 않습니다.

## 설치

Homebrew:

```bash
brew tap NomaDamas/katok https://github.com/NomaDamas/katok.git
brew install katok
```

Cargo:

```bash
cargo install katok
```

기본 설치는 macOS에서 `katok send`를 포함합니다. 검색·아카이브 기능만 필요한
경우 전송 기능 없이 설치할 수 있습니다.

```bash
cargo install katok --no-default-features
```

Cargo로 설치했는데 `katok: command not found`가 나오면 현재 셸이 Cargo binary 경로를 못 보고 있는 상태입니다.

```bash
export PATH="$HOME/.cargo/bin:$PATH"
katok --help
```

영구 적용은 사용하는 셸 설정에 추가합니다.

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.zshrc
exec zsh -l
```

처음 설치한 뒤에는 터미널에 전체 디스크 접근 권한을 주세요.

```bash
katok permissions macos
```

열린 System Settings에서 현재 사용하는 Terminal, iTerm, Codex 앱 또는 설치된 `katok` 실행 파일을 Full Disk Access에 추가하세요. macOS TCC 권한은 사용자가 시스템 설정에서 직접 허용해야 하므로 CLI가 자기 자신에게 권한을 영구 부여할 수는 없습니다.

```bash
katok doctor --json
```

`doctor`는 기본값으로 로컬 인덱스 freshness만 확인하므로 macOS 권한 prompt를 띄우지 않습니다.
또한 `freshness` 섹션에서 마지막 `sync`와 `index` 완료 시각을 보여줍니다.
카카오톡 앱, 컨테이너, DB 파일 개수, 인증 캐시 여부까지 확인하려면 아래처럼 명시적으로 실행합니다.

```bash
katok doctor --macos-probe --json
```

이 probe는 macOS가 "katok would like to access data from other apps" 권한 요청을 띄울 수 있습니다. 반복 요청을 줄이려면 `katok permissions macos`로 System Settings를 연 뒤 사용 중인 Terminal/iTerm/Codex 앱이나 설치된 `katok` 실행 파일을 Full Disk Access에 허용하세요.

`katok send`는 별도의 Accessibility 권한이 필요합니다.

```bash
katok permissions macos --accessibility
```

Accessibility 권한은 사용자의 로컬 Mac에서 KakaoTalk UI를 조작할 수 있게 할
뿐, 카카오가 자동 전송을 승인했다는 뜻은 아닙니다.

권한 설정을 처음부터 안내받으려면:

```bash
scripts/katok-macos-setup.sh
```

자세한 흐름은 `docs/macos-first-run.md`에 있습니다. 카카오톡 DB 스키마, 미디어 캐시 파일명 규칙, Pkv2 복호화, WAL 읽기 불변식 같은 내부 구조는 `docs/kakao-media-internals.md`에 정리돼 있습니다.

## 기본 사용 흐름

```bash
katok doctor --json
katok sync --source macos --json
katok index --json
katok search keyword "계약서" --json
katok search bm25 "지난주 미팅 자료" --json
katok search semantic "최근에 논의한 세금 신고 일정" --json
katok search bm25 "지난주 미팅 자료" --limit 30 --json
```

각 `search` 명령은 `--limit <N>`(기본 10)으로 반환할 결과 개수를 조절할 수 있습니다.

검색 최신성이 중요하면 검색 전에 항상 `katok doctor --json`의 `freshness`를 확인하세요. 이 기본 doctor는 macOS app data probe를 실행하지 않으므로 권한 prompt 없이 사용할 수 있습니다. `sync_before_search`가 `true`이면 `katok sync --source macos --json`을 먼저 실행하고, `index_before_semantic_search`가 `true`이면 `katok index --json`을 실행한 뒤 semantic search를 사용합니다. doctor와 semantic search는 archive revision을 현재 committed index generation과 비교하므로, sync 뒤 index가 오래됐거나 vector ID가 archive와 어긋나면 검색 전에 명시적으로 재인덱싱을 요구합니다.

검색 결과에서 더 넓은 맥락이 필요하면 chunk 명령을 사용합니다.

```bash
katok chunk get <chunk-id> --json
katok chunk context <chunk-id> --json
katok chunk parent <chunk-id> --json
```

- `chunk get`은 해당 chunk 원문을 가져옵니다.
- `chunk context`는 같은 채팅방의 바로 앞뒤 chunk를 보여줍니다.
- `chunk parent`는 semantic search가 사용한 더 큰 parent window를 보여줍니다.

카카오톡 첨부를 추출하려면 media 명령을 사용합니다. 사진(type 2), 앨범(type 27), 영상(type 3), 그리고 일반 파일(type 18)을 다룹니다. 일반 파일은 하나의 메시지 타입이 zip·pdf·xlsx·hwp·pptx 등 모든 확장자를 덮으므로 형식별 대응이 따로 필요하지 않습니다.

```bash
katok media get --chat <chat-id> --json
katok media get --chat <chat-id> --kind file --json
katok media get --chat <chat-id> --log <log-id> --out ./katok-media --no-cdn --json
```

각 프레임은 로컬 full 캐시(`.img`/`.vid`), CDN presigned GET, 로컬 thumbnail `.thm`, stub 순서로 해석됩니다. 추출 자체가 사용자가 명령을 실행해 opt in하는 기능이며, 네트워크를 사용하는 유일한 동작은 attachment metadata의 CDN presigned GET입니다. CDN 응답은 `cs` SHA-1과 일치한 bytes만 저장하고, `--no-cdn`을 주면 CDN tier를 끄고 로컬 캐시만 사용합니다. 기본 출력 위치는 katok data directory 아래 `media/<chat-id>/`입니다.

**일반 파일은 로컬 캐시가 없습니다.** 카카오톡은 사진·영상만 컨테이너에 캐시하고 파일 첨부는 디스크에 남기지 않으므로, 파일의 tier는 CDN 하나뿐이고 `--no-cdn`으로는 아무것도 받을 수 없습니다. 저장 파일명은 첨부의 원본 이름을 그대로 씁니다(`<logId>_<원본이름>`) — zip 본문은 확장자 sniffing으로 `.bin`이 되므로 이름이 확장자의 권위입니다.

**presigned 서명은 약 14일 뒤 만료되고, 만료되면 410으로 사라집니다.** 로컬 사본이 없는 파일 첨부에서는 이것이 곧 영구 유실을 뜻하므로, 정기적으로 `media backfill`을 돌려 창이 닫히기 전에 보존하는 것이 이 기능의 실제 사용법입니다.

```bash
katok media backfill --dry-run --json
katok media backfill --json
katok media backfill --kind file --kind video --json
```

`backfill`은 미디어가 있는 모든 방을 돌면서 아직 만료되지 않은 링크만 받습니다. 이미 저장된 프레임은 네트워크 호출 없이 건너뛰므로 재실행이 멱등하고, 중단된 실행을 그대로 이어받습니다. 기본 kind는 `file`입니다(사진·영상은 로컬 캐시가 있지만 파일은 없기 때문). `--dry-run`은 요청을 한 번도 보내지 않고 각 프레임이 어느 tier로 갈지만 보고하므로, 받을 대상과 만료된 대상을 미리 구분할 수 있습니다.

`--json` 출력 스키마의 주요 필드는 다음과 같습니다.

- `chat_id`, `log_id`, `limit`, `kinds`, `output_dir`, `cdn_enabled`
- `frame_count`: 읽은 frame 수
- `records[]`: `logId`, `idx`, `kind`, `name`, `w`, `h`, `cs`, `s`, `tier`, `tier_reason`, `path`, `sha1`, `sender`, `ts`
- `errors[]`: tier 실패 관측값, `logId`, `idx`, `stage`, `path`, `error`
- `tier_counts`: `full`, `cdn`, `thumb`, `stub`, `existing`, `planned` 별 개수

`tier_reason`은 왜 그 tier로 떨어졌는지 말합니다. `cdn-expired`는 서명 만료, `cdn-too-large`는 선언 크기가 `--max-bytes`를 넘어 요청 전에 거절, `cdn-unverifiable`은 `cs` 지문이 없어 검증할 수 없어 거절, `unavailable`은 로컬 캐시가 애초에 존재하지 않는 파일 첨부를 뜻합니다.

## 검색 방식

`katok search keyword`는 빠르고 결정적인 부분 문자열 검색입니다. 정확한 단어, 이름, 계좌번호, 고유명사처럼 그대로 기억나는 값을 찾을 때 씁니다.

`katok search bm25`는 SQLite FTS5 BM25 랭킹을 사용합니다. 여러 단어가 섞인 일반 질의에 적합합니다.

BM25 입력은 FTS5 연산식이 아니라 일반 검색어로 처리됩니다. `+`, `-`, 따옴표, 괄호 같은 문자가 포함되어도 문자 그대로 tokenizer에 전달되며 FTS5 column filter나 boolean 문법으로 실행되지 않습니다.

`katok search semantic`은 EmbeddingGemma로 만든 로컬 벡터 인덱스를 사용합니다. 표현이 정확히 기억나지 않아도 의미가 비슷한 대화를 찾을 수 있습니다.

`katok index`는 새 generation을 완전히 만든 뒤 `CURRENT` 포인터를 원자적으로 교체합니다. 실패하면 이전 generation이 그대로 유지되고 명령은 non-zero로 끝납니다. `--full`은 기존 vector를 재사용하지 않는 완전 rebuild이고, 기본 index는 healthy generation의 동일 vector만 재사용하며 무결성 불일치가 있으면 archive에서 self-heal합니다.

## EmbeddingGemma 로컬 벡터 검색

`katok index`는 기본값으로 `embeddinggemma-300m-q4`를 앱 프로세스 안에서 실행합니다.

- Python 서버가 필요 없습니다.
- Jina, TEI, 별도 로컬 HTTP embedding endpoint가 필요 없습니다.
- 첫 실행 때 모델 artifact를 Hugging Face/fastembed cache에 내려받고, 이후에는 로컬 cache를 재사용합니다.
- 벡터 인덱스와 semantic documents는 사용자 Mac 안의 katok data directory에만 저장됩니다.

설정 예:

```toml
embedder_model = "embeddinggemma-300m-q4"
embedding_batch_size = 64
vector_dimension = 768
semantic_dir = "semantic"
```

테스트나 오프라인 QA에서는 모델 다운로드 없이 deterministic vector를 사용할 수 있습니다.

```bash
KATOK_EMBEDDER=local-test katok index --json
KATOK_EMBEDDER=mock katok index --json
```

실사용 경로에서는 원격 embedding endpoint 설정을 받지 않습니다. 오래된 `embedder_base_url` 또는 `allow_remote_embeddings` 설정이 있으면 거부합니다.

## Vercel Agent Skills / Codex Skills에서 쓰기

이 저장소에는 얇은 agent skill wrapper가 포함되어 있습니다.

```text
skills/katok/SKILL.md
```

에이전트는 카카오톡 DB나 SQLCipher 내부를 직접 만지지 않고, 아래 흐름만 사용해야 합니다.

```bash
katok doctor --json
katok sync --source macos --json
katok index --json
katok search semantic "찾고 싶은 내용" --json
katok chunk get <chunk-id> --json
```

권장 패턴:

1. 검색 전에 `katok doctor --json`의 `freshness`를 봅니다.
2. `sync_before_search`가 `true`이거나 최신 대화가 중요하면 `katok sync --source macos --json`을 실행합니다.
3. semantic search 전에 `index_before_semantic_search`가 `true`이면 `katok index --json`을 실행합니다.
4. 처음에는 `katok search keyword`, `katok search bm25`, `katok search semantic`으로 후보를 좁힙니다.
5. 사용자가 특정 결과를 열어 달라고 하거나 chunk id를 제공했을 때만 `katok chunk get`으로 원문을 봅니다.
6. semantic search 결과의 `child_chunk_ids`에서 정확한 원문으로 이동할 때는 `katok chunk context`와 `katok chunk parent`를 사용합니다.
7. skill은 결과를 요약만 하고, indexing logic이나 DB 해독 logic을 자체 구현하지 않습니다.

## macOS 소스 어댑터

`katok sync --source macos`는 Rust 코드로 카카오톡 macOS 설치를 직접 읽습니다. 런타임에 Python, `kakaocli`, 별도 helper 서버가 필요 없습니다.

sync는 자주 실행해도 되도록 증분으로 동작합니다. 메시지가 실제로 바뀐 채팅방의 tail만 다시 계산하므로, 일반적인 append sync 비용은 전체 아카이브 크기보다 변경 범위에 가깝게 움직입니다. 전량을 다시 계산하는 경우는 세 가지입니다. 빈 아카이브에 처음 실행하는 sync, `chunk_gap_group_seconds`/`chunk_gap_direct_seconds` 를 바꾼 뒤 처음 실행하는 sync, 그리고 chunk 경계 규칙이 바뀐 버전으로 올린 뒤 처음 실행하는 sync 입니다. 이 버전을 기록하기 전에 만들어진 기존 아카이브도 여기 해당하므로 업그레이드 직후 sync 한 번은 전량을 다시 계산합니다. 그 뒤로는 다시 증분으로 돌아옵니다. 출력에 `rebuilt_chats`와 단계별 소요 시간(`timings_ms`의 `read_source`, `upsert_messages`, `rebuild_chunks`)이 포함되므로 느린 실행의 원인을 단계 단위로 확인할 수 있습니다.

요구사항:

- 터미널 앱이 `~/Library/Containers/com.kakao.KakaoTalkMac/` 아래 파일을 읽을 수 있도록 전체 디스크 접근 권한을 받아야 합니다.
- 카카오톡 앱에서 열렸거나 동기화된 채팅방의 로컬 DB 기록만 읽을 수 있습니다.
- 최초 sync 때 암호화된 SQLCipher DB에서 계정 식별자를 복구하고, `{user_id, uuid}`만 mode `0600` cache로 저장합니다. 키 material 자체는 저장하지 않습니다.

fixture로 개발/테스트할 때는 실제 카카오톡 설치가 필요 없습니다.

```bash
katok source chats --source fixture tests/fixtures/kakao/replies.jsonl --json
katok sync --source fixture tests/fixtures/kakao/replies.jsonl --json
```

합성 데이터로 실행할 때는 `--data-dir <임시경로>` 플래그로 반드시 격리하세요. `KATOK_DATA_DIR` 환경변수는 없습니다. 설정해도 조용히 무시되고 실제 아카이브에 기록됩니다.

## 메시지 전송

전송은 되돌릴 수 없고 다른 사람에게 도달할 수 있습니다. 먼저 대상 방만
확인하는 `--dry-run`을 사용하십시오.

```bash
katok send --chat <chat-id> --dry-run --json
katok send --room "정확한 방 이름" --dry-run --json
```

실제 텍스트·이미지 전송 또는 초안 입력에는
[`ACCEPTABLE_USE_POLICY.md`](ACCEPTABLE_USE_POLICY.md)와
[`DISCLAIMER.md`](DISCLAIMER.md)를 읽었다는 명시적 확인이 필요합니다.

```bash
katok send --chat <chat-id> --text "확인한 메시지" --accept-use-policy --json
katok send --chat <chat-id> --image ./photo.jpg --accept-use-policy --json
katok send --chat <chat-id> --text "검토할 초안" --draft --accept-use-policy --json
```

`--accept-use-policy`는 법률 준수나 카카오의 승인을 보증하지 않습니다. 불법
스팸, 사칭·계정 도용, 신고·차단·거부 이후의 연락, 스토킹·괴롭힘, 반복·대량·
무인 전송, 개인정보 침해 및 보호조치 우회에는 사용할 수 없습니다. 업무용
광고·알림은 카카오톡 채널, 비즈메시지, 알림톡 등 목적에 맞는 공식 제품을
사용하십시오.

이 구현의 네트워크 경계는 다음과 같습니다.

- `katok send` 자체는 HTTP, WebSocket, 소켓 또는 카카오 원격 비공개
  프로토콜·비공식 API를 직접 호출하지 않습니다.
- 로컬 Accessibility, `CGEvent`, pasteboard, AppKit, 로컬 파일과 katok
  아카이브만 사용합니다.
- 실제 네트워크 전송은 로그인된 공식 KakaoTalk 앱이 수행합니다.
- 이 설명은 `send` 경로에 한정됩니다. `media` 명령의 presigned CDN 다운로드
  및 최초 모델 artifact 준비 등 다른 기능은 네트워크를 사용할 수 있습니다.

## CLI 명령 요약

```bash
katok doctor --json
katok source chats --source macos --json
katok sync --source macos --json
katok sync --json
katok index --json
katok search keyword "보고서" --json
katok search bm25 "보고서" --json
katok search semantic "회의 보고서" --json
katok chunk get <chunk-id> --json
katok chunk context <chunk-id> --json
katok chunk parent <chunk-id> --json
katok transcript --chat <chat-id> --json
katok transcript --chat <chat-id> --since 2026-07-20T00:00:00+09:00 --json
katok media get --chat <chat-id> --no-cdn --json
katok wipe-index --yes --json
katok send --chat <chat-id> --dry-run --json
katok send --chat <chat-id> --text "메시지" --accept-use-policy --json
```

`katok transcript`는 한 채팅방에서 실제로 오간 말을 시간 순서대로 Markdown 파일로 내보냅니다. 검색이 "어떤 chunk가 관련 있나"에 답한다면 이 명령은 "무슨 말이 오갔나"에 답하므로, 방 하나를 밀린 채로 따라 읽을 때 씁니다. 라이브 카카오톡 DB가 아니라 아카이브를 읽으므로 최근 대화가 필요하면 `sync`를 먼저 실행합니다. 범위에 메시지가 없으면 파일을 만들지 않고, 파일 이름에 message id 범위가 들어가므로 나중 실행이 이전 결과를 덮어쓰지 않습니다. 카카오톡 시스템 메시지(입장·퇴장·초대)는 아카이브에는 남고 transcript에서만 빠집니다.

권한 진단이 필요할 때만:

```bash
katok doctor --macos-probe --json
```

`doctor --json`의 freshness 예:

```json
{
  "freshness": {
    "last_sync": {
      "completed_at": "2026-06-15T05:00:00Z",
      "source": "macos",
      "total_messages": 12345,
      "chunks": 6789
    },
    "last_index": {
      "completed_at": "2026-06-15T05:03:00Z",
      "embedder": "embeddinggemma/local",
      "vectorstore": "local",
      "semantic_units": "parent_windows",
      "embedded_texts": 42
    },
    "recommendation": {
      "sync_before_search": false,
      "index_before_semantic_search": false,
      "reason": "archive and semantic index have completed at least once; re-run sync/index when freshness matters"
    }
  }
}
```

## 개인정보와 로컬 파일

이 프로젝트가 다루는 파일은 모두 민감 정보로 취급합니다.

- 카카오톡 DB 경로와 SQLCipher 관련 정보
- 정규화된 메시지 아카이브
- semantic documents
- embedding cache와 vector index
- 검색 근거와 로그

생성된 아카이브, 인덱스, cache, 로그는 git에 넣지 않습니다. 자동화 테스트는 합성 fixture만 사용합니다. 실제 카카오톡 smoke test는 수동으로만 수행하고, 사용자가 명시하지 않은 대화 원문은 출력하지 않습니다.

## 개발

```bash
cargo fmt --all -- --check
cargo build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
python3 scripts/verify_release_config.py
```

## 참고 프로젝트

아래 프로젝트들은 조사 과정의 참고 자료입니다. 현재 `katok`의 주 경로는 macOS 로컬 DB를 개인 Mac 안의 아카이브, BM25 index, EmbeddingGemma vector index로 바꾸는 방식입니다.

- `silver-flight-group/kakaocli`: macOS local DB read/search/sync CLI.
- `JungHoonGhae/openkakao-cli`: local DB read/search plus LOCO-oriented flows.
- `xistoh162108/kakaotalk_analyzer`: export CSV analysis with embedding and SPLADE ideas.
- `teddylee777/kakaotalk-gpt`: export txt/csv RAG with FAISS/Chroma retrievers.
- `sanggubot/doppelganger-gpt`: KakaoTalk txt to Chroma example.
- `uoneway/kakaotalk_msg_preprocessor`: exported txt parser.
- `claudianus/kakaotalk-chat-analyzer`: CSV export to anonymized HTML report.
