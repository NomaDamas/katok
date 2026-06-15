# katok

`katok`은 Apple Silicon Mac에서 카카오톡 대화를 로컬로 읽고, 키워드 검색과 벡터 검색을 바로 쓸 수 있게 만드는 CLI입니다.

카카오톡 대화 내용을 서버로 올리지 않습니다. macOS에 저장된 카카오톡 DB를 읽어 개인 Mac 안에 정규화된 아카이브와 검색 인덱스를 만들고, `katok search ...` 명령으로 필요한 대화를 찾습니다.

## 무엇을 해주나

- 카카오톡 macOS 앱의 로컬 DB를 읽어 대화 아카이브를 만듭니다.
- 정확한 단어 매칭용 `keyword`, SQLite FTS5 기반 `bm25`, EmbeddingGemma 기반 `semantic` 검색을 제공합니다.
- 긴 대화는 카카오톡 흐름에 맞게 chunk로 나누고, 5분 안팎의 같은 채팅방 대화는 parent window로 묶어 벡터 검색 품질을 높입니다.
- 검색 결과는 짧은 snippet과 chunk id만 보여줍니다. 원문 전체는 사용자가 명시적으로 `katok chunk get <chunk-id>`를 실행할 때만 출력합니다.
- 에이전트는 Vercel Agent Skills/Codex Skills에서 `skills/katok/SKILL.md`를 통해 CLI만 호출하면 됩니다.

## 지원 환경

- Apple Silicon Mac
- macOS 카카오톡 앱
- 터미널 앱의 전체 디스크 접근 권한

Intel Mac은 지원하지 않습니다. 현재 로컬 임베딩 경로가 `fastembed`와 ONNX Runtime을 사용하며, 이 dependency set은 `x86_64-apple-darwin`용 prebuilt ONNX Runtime을 제공하지 않습니다.

## 설치

Homebrew:

```bash
brew tap NomaDamas/katok git@github.com:NomaDamas/katok.git
brew install katok
```

Cargo:

```bash
cargo install katok
```

처음 설치한 뒤에는 터미널에 전체 디스크 접근 권한을 주세요.

```bash
katok doctor --json
```

`doctor`는 카카오톡 앱, 컨테이너, DB 파일 개수, 인증 캐시 여부만 확인합니다. 대화 내용은 출력하지 않습니다.

권한 설정을 처음부터 안내받으려면:

```bash
scripts/katok-macos-setup.sh
```

자세한 흐름은 `docs/macos-first-run.md`에 있습니다.

## 기본 사용 흐름

```bash
katok doctor --json
katok sync --source macos --json
katok index --json
katok search keyword "계약서" --json
katok search bm25 "지난주 미팅 자료" --json
katok search semantic "최근에 논의한 세금 신고 일정" --json
```

검색 결과에서 더 넓은 맥락이 필요하면 chunk 명령을 사용합니다.

```bash
katok chunk get <chunk-id> --json
katok chunk context <chunk-id> --json
katok chunk parent <chunk-id> --json
```

- `chunk get`은 해당 chunk 원문을 가져옵니다.
- `chunk context`는 같은 채팅방의 바로 앞뒤 chunk를 보여줍니다.
- `chunk parent`는 semantic search가 사용한 더 큰 parent window를 보여줍니다.

## 검색 방식

`katok search keyword`는 빠르고 결정적인 부분 문자열 검색입니다. 정확한 단어, 이름, 계좌번호, 고유명사처럼 그대로 기억나는 값을 찾을 때 씁니다.

`katok search bm25`는 SQLite FTS5 BM25 랭킹을 사용합니다. 여러 단어가 섞인 일반 질의에 적합합니다.

`katok search semantic`은 EmbeddingGemma로 만든 로컬 벡터 인덱스를 사용합니다. 표현이 정확히 기억나지 않아도 의미가 비슷한 대화를 찾을 수 있습니다.

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

1. 처음에는 `katok search keyword`, `katok search bm25`, `katok search semantic`으로 후보를 좁힙니다.
2. 사용자가 특정 결과를 열어 달라고 하거나 chunk id를 제공했을 때만 `katok chunk get`으로 원문을 봅니다.
3. semantic search 결과의 `child_chunk_ids`에서 정확한 원문으로 이동할 때는 `katok chunk context`와 `katok chunk parent`를 사용합니다.
4. skill은 결과를 요약만 하고, indexing logic이나 DB 해독 logic을 자체 구현하지 않습니다.

## macOS 소스 어댑터

`katok sync --source macos`는 Rust 코드로 카카오톡 macOS 설치를 직접 읽습니다. 런타임에 Python, `kakaocli`, 별도 helper 서버가 필요 없습니다.

요구사항:

- 터미널 앱이 `~/Library/Containers/com.kakao.KakaoTalkMac/` 아래 파일을 읽을 수 있도록 전체 디스크 접근 권한을 받아야 합니다.
- 카카오톡 앱에서 열렸거나 동기화된 채팅방의 로컬 DB 기록만 읽을 수 있습니다.
- 최초 sync 때 암호화된 SQLCipher DB에서 계정 식별자를 복구하고, `{user_id, uuid}`만 mode `0600` cache로 저장합니다. 키 material 자체는 저장하지 않습니다.

fixture로 개발/테스트할 때는 실제 카카오톡 설치가 필요 없습니다.

```bash
katok source chats --source fixture tests/fixtures/kakao/replies.jsonl --json
katok sync --source fixture tests/fixtures/kakao/replies.jsonl --json
```

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
katok wipe-index --yes --json
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
