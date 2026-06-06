# Hydrogen Peroxide / hype Architecture Plan

## TL;DR
> **Summary**: Build a macOS-only Rust CLI named `hype` that indexes KakaoTalk through a source adapter, stores a private normalized SQLite archive, chunks conversations with Kakao-specific reply/time rules, and exposes keyword, BM25, semantic, and chunk lookup commands.
> **Deliverables**:
> - Rust workspace and `hype` CLI.
> - `kakaocli`/`kakaotalk-mac` source adapter plus synthetic fixture adapter.
> - SQLite archive with stable messages, chunks, reply edges, FTS5 keyword/BM25.
> - MinSync/LanceDB semantic bridge with local Jina embeddings.
> - README story, privacy docs, CLI help, ignored local stores.
> **Effort**: Large
> **Parallel**: YES - 4 waves
> **Critical Path**: Task 1 -> Task 3 -> Task 4 -> Task 5 -> Task 6 -> Task 8 -> Final Verification

## Context
### Original Request
The user asked to make the project as Hydrogen Peroxide, short command `hype`, with careful chunk strategy, MinSync incremental indexing, semantic search, simple keyword search, BM25 search, and chunk-id content retrieval through CLI. The user explicitly emphasized KakaoTalk-specific chunking: consecutive messages by one nickname must be a single chunk; large time gaps should split chunks; replies must preserve parent-message/chunk backtracking metadata. They also asked to research similar projects, use local Jina embeddings, and choose an architecture that can effectively access KakaoTalk macOS DB data.

### Interview Summary
- No extra user interview is needed because the repo is greenfield and the request already selects the hard requirements.
- Default language is Rust because MinSync is Rust and exposes a library surface.
- Test strategy is TDD with synthetic fixtures only, matching `AGENTS.md`.
- Real KakaoTalk smoke tests are manual-only and must not print private content.

### Metis Review (gaps addressed)
- Gap: “latest Jina” can be heavier than a typical Mac can run. Addressed by targeting `jinaai/jina-embeddings-v4` first and documenting a local fallback to `jinaai/jina-embeddings-v3` if v4 serving is not viable.
- Gap: MinSync's default recursive chunker could split chat chunks. Addressed by making Hydrogen Peroxide own canonical chunking and feed deterministic semantic documents/segments that always map back to canonical `chunk_id`.
- Gap: Reply parent may arrive outside the indexed range. Addressed by schema support for unresolved parent edges and a follow-up backfill query path.
- Gap: Search snippets could leak private history. Addressed by minimal snippets, explicit `chunk get` for full content, and tests that assert no raw content appears in logs/errors.
- Gap: generated MinSync documents duplicate sensitive text. Addressed by putting all generated artifacts under ignored user-only app-support paths, never repo paths.

## Work Objectives
### Core Objective
Ship a working local-first macOS CLI surface named `hype` for incremental KakaoTalk indexing and search.

### Deliverables
- `hype doctor`
- `hype source chats`
- `hype sync`
- `hype index --full`
- `hype search keyword <query>`
- `hype search bm25 <query>`
- `hype search semantic <query>`
- `hype chunk get <chunk-id>`
- `hype wipe-index`
- README and CLI help aligned with privacy behavior and the Hydrogen Peroxide story.

### Definition of Done (verifiable conditions with commands)
- `cargo test --workspace` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- `cargo fmt --all -- --check` exits 0.
- `RUST_LOG=debug cargo run --bin hype -- search keyword "synthetic-secret"` does not log raw fixture message content outside intended result fields.
- `cargo run --bin hype -- --data-dir .omo/evidence/hype-qa sync --source fixture tests/fixtures/kakao/basic.jsonl` indexes synthetic messages.
- `cargo run --bin hype -- --data-dir .omo/evidence/hype-qa search keyword "보고서"` returns deterministic chunk metadata.
- `cargo run --bin hype -- --data-dir .omo/evidence/hype-qa search bm25 "보고서"` returns ranked results.
- `cargo run --bin hype -- --data-dir .omo/evidence/hype-qa search semantic "지난 회의에서 보고서 이야기"` returns chunk ids after local embedder setup or mock embedder integration test.
- `cargo run --bin hype -- --data-dir .omo/evidence/hype-qa chunk get <known-fixture-chunk-id>` returns full synthetic chunk content and reply parent metadata.

### Must Have
- Canonical chunk id is stable and deterministic from source account hash, chat id, first message id, last message id, and chunk schema version.
- Consecutive same-nickname messages are never split into separate canonical chunks unless a configured time gap threshold is crossed.
- Default chunk gap thresholds: group chats 10 minutes, direct chats 30 minutes, configurable in `hype.toml`.
- Reply edges store parent message id, parent chunk id when resolved, unresolved reason when missing, and source message id that made the reply.
- Semantic results return canonical chunk ids and can be expanded with `hype chunk get`.
- Simple keyword and BM25 are separate commands even though both use SQLite FTS5.
- All automated fixtures are synthetic.
- Local generated data is ignored and created with user-only permissions where supported.

### Must NOT Have
- No raw KakaoTalk content in committed fixtures, logs, tests, screenshots, README examples, or plan evidence.
- No SQLCipher keys, auth caches, DB paths, embeddings, archives, indexes, or generated MinSync source documents committed.
- No remote embedding or LLM API by default.
- No KakaoTalk message sending.
- No automated test that depends on a real KakaoTalk installation.

## Verification Strategy
> ZERO HUMAN INTERVENTION - all verification is agent-executed.
- Test decision: TDD with Rust unit and integration tests using `cargo test`; every production task starts by adding failing tests.
- QA policy: Every task has CLI or data-surface scenarios with synthetic inputs.
- Evidence: `.omo/evidence/task-{N}-{slug}.{ext}`

## Execution Strategy
### Parallel Execution Waves
Wave 1: Tasks 1, 2, 3
Wave 2: Tasks 4, 5
Wave 3: Tasks 6, 7, 8, 9
Wave 4: Tasks 10, 11

### Dependency Matrix
| Task | Blocks | Blocked By |
| --- | --- | --- |
| 1 | 2, 3, 4, 6, 7, 8, 9, 10, 11 | none |
| 2 | 4, 6, 7, 8, 9, 10 | 1 |
| 3 | 4 | 1 |
| 4 | 5, 6, 7, 8, 9 | 1, 2, 3 |
| 5 | 6, 8, 9 | 4 |
| 6 | 8 | 2, 4, 5 |
| 7 | 10 | 2, 4 |
| 8 | 10 | 5, 6 |
| 9 | 10 | 4, 5 |
| 10 | 11 | 6, 7, 8, 9 |
| 11 | Final Verification | 10 |

## TODOs
> Implementation + Test = ONE task. Never separate.
> EVERY task MUST have: References + Acceptance Criteria + QA Scenarios.

- [ ] 1. Scaffold Rust workspace, CLI identity, ignored local stores

  **What to do**: Create a Rust workspace with crates `hype-cli`, `hype-core`, and `hype-adapters`. Configure binary name `hype`. Add `.gitignore` entries for `.hype/`, `.minsync/`, `*.sqlite*`, `*.lance/`, auth caches, generated archives, logs, and `.omo/evidence/hype-qa/`. Add README rename from Kakao Memory to Hydrogen Peroxide while preserving local-first intent.
  **Must NOT do**: Do not commit generated DBs, MinSync state, embeddings, or private examples.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 2, 3, 4, 6, 7, 8, 9, 10, 11 | Blocked By: none

  **References**:
  - Pattern: `AGENTS.md` - privacy and hygiene rules.
  - Pattern: `README.md` - current concept, adjacent project list, CLI sketch.
  - External: `../MinSync/Cargo.toml` - Rust version and dependency style.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_1_cli_identity` fails before implementation and passes after.
  - [ ] `cargo run --bin hype -- --help` prints `Hydrogen Peroxide` and command name `hype`.
  - [ ] `git status --ignored --short .hype .minsync .omo/evidence/hype-qa` shows generated local stores ignored after creating dummy files.

  **QA Scenarios**:
  ```text
  Scenario: CLI identity happy path
    Tool: tmux
    Steps: tmux new-session -d -s ulw-qa-cli 'cargo run --bin hype -- --help'; tmux capture-pane -pt ulw-qa-cli -S -200
    Expected: transcript contains "Hydrogen Peroxide" and "hype".
    Evidence: .omo/evidence/task-1-cli-help.txt

  Scenario: ignored data guard
    Tool: bash
    Steps: mkdir -p .hype .omo/evidence/hype-qa && touch .hype/archive.sqlite .hype/auth-cache.json .omo/evidence/hype-qa/tmp.log && git status --ignored --short .hype .omo/evidence/hype-qa
    Expected: all created files appear as ignored, not tracked.
    Evidence: .omo/evidence/task-1-ignore.txt
  ```

  **Commit**: YES | Message: `chore(scaffold): initialize hype rust workspace` | Files: `Cargo.toml`, `crates/**`, `.gitignore`, `README.md`

- [ ] 2. Implement private app data paths and configuration

  **What to do**: Add `hype-core::paths` and `hype-core::config`. Default data root is `~/Library/Application Support/Hydrogen Peroxide/hype/`; tests can override with `--data-dir`. Create dirs with `0700` permissions on Unix. Config file `hype.toml` includes source adapter, chunk gap thresholds, MinSync dir, embedder model, vector dimension, snippet length.
  **Must NOT do**: Do not put generated source documents in the repo root by default.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 4, 6, 7, 8, 9, 10 | Blocked By: 1

  **References**:
  - Pattern: `AGENTS.md` - user-only permissions and ignored generated stores.
  - API: `../MinSync/src/config.rs` - MinSync config shape for embedder and vectorstore dimensions.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_2_data_dir_permissions` fails before implementation and passes after.
  - [ ] `cargo run --bin hype -- --data-dir .omo/evidence/hype-qa doctor --json` reports resolved paths without creating world-readable dirs.

  **QA Scenarios**:
  ```text
  Scenario: explicit data dir
    Tool: bash
    Steps: rm -rf .omo/evidence/hype-qa && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa doctor --json && stat -f %Lp .omo/evidence/hype-qa
    Expected: JSON includes data_dir ".omo/evidence/hype-qa"; permissions are 700 on macOS.
    Evidence: .omo/evidence/task-2-data-dir.txt

  Scenario: malformed config
    Tool: bash
    Steps: printf 'chunk_gap_group_seconds = "bad"\\n' > .omo/evidence/bad-hype.toml && cargo run --bin hype -- --config .omo/evidence/bad-hype.toml doctor --json
    Expected: exits non-zero with structured config error and no raw private path beyond the provided config path.
    Evidence: .omo/evidence/task-2-bad-config.txt
  ```

  **Commit**: YES | Message: `feat(config): add private data paths` | Files: `crates/hype-core/src/paths.rs`, `crates/hype-core/src/config.rs`, `crates/hype-cli/src/main.rs`

- [ ] 3. Build source adapter abstraction with fixture and Kakao adapters

  **What to do**: Define `SourceAdapter` returning normalized `RawMessage` records: account hash, chat id/name, chat type, message id/log id, sender id, sender nickname, timestamp, text, message type, reply parent message id, source cursor. Implement `FixtureAdapter` for JSONL tests. Implement `KakaocliAdapter` that shells out only to read-only commands: `kakaocli chats --json`, `kakaocli messages --json`, and fallback helper `python3 scripts/kakaotalk_mac.py ... --json` when configured. Add `hype source chats`.
  **Must NOT do**: Do not parse SQLCipher directly in v1; do not print keys or DB paths.

  **Parallelization**: Can Parallel: YES | Wave 1 | Blocks: 4 | Blocked By: 1

  **References**:
  - Pattern: `/Users/jeffrey/.codex/skills/kakaotalk-mac/SKILL.md` - read-only command list and permission failure modes.
  - Pattern: `README.md` - source adapter direction.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_3_fixture_adapter_reads_jsonl` fails before implementation and passes after.
  - [ ] `cargo run --bin hype -- source chats --source fixture tests/fixtures/kakao/basic.jsonl --json` returns synthetic chat metadata.
  - [ ] Adapter errors redact command env and do not include SQLCipher keys.

  **QA Scenarios**:
  ```text
  Scenario: fixture chats
    Tool: bash
    Steps: cargo run --bin hype -- source chats --source fixture tests/fixtures/kakao/basic.jsonl --json
    Expected: JSON contains synthetic chat id "chat-group-1" and no real Kakao path.
    Evidence: .omo/evidence/task-3-fixture-chats.json

  Scenario: missing kakaocli
    Tool: bash
    Steps: PATH=/usr/bin:/bin cargo run --bin hype -- source chats --source kakaocli --json
    Expected: exits non-zero with actionable "kakaocli not found or not configured" and no key/path leakage.
    Evidence: .omo/evidence/task-3-missing-kakaocli.txt
  ```

  **Commit**: YES | Message: `feat(source): add kakao source adapters` | Files: `crates/hype-adapters/**`, `tests/fixtures/kakao/basic.jsonl`

- [ ] 4. Implement SQLite archive, cursors, reply graph, and ingestion

  **What to do**: Add SQLite schema and migrations for `messages`, `chats`, `sync_cursors`, `chunks`, `chunk_messages`, `reply_edges`, `chunk_parent_refs`, and FTS table. Use stable message identity from source account hash + chat id + message id/log id. Add `hype sync --source ...` that upserts messages incrementally by cursor and stores unresolved reply edges.
  **Must NOT do**: Do not store raw account secrets; only hashed account identity.

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 5, 6, 7, 8, 9 | Blocked By: 1, 2, 3

  **References**:
  - Pattern: `AGENTS.md` - stable identifiers and incremental cursors.
  - API: `../MinSync/src/manifest.rs` and `../MinSync/src/state.rs` - incremental state concepts to align with.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_4_incremental_cursor_idempotent` fails before implementation and passes after.
  - [ ] Running `hype sync` twice on the same fixture reports zero second-run inserts.
  - [ ] Reply edge with missing parent is stored as unresolved, not dropped.

  **QA Scenarios**:
  ```text
  Scenario: idempotent sync
    Tool: bash
    Steps: rm -rf .omo/evidence/hype-qa && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa sync --source fixture tests/fixtures/kakao/basic.jsonl --json && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa sync --source fixture tests/fixtures/kakao/basic.jsonl --json
    Expected: first run inserted_messages > 0; second run inserted_messages = 0 and updated_messages = 0.
    Evidence: .omo/evidence/task-4-idempotent-sync.json

  Scenario: malformed fixture row
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa sync --source fixture tests/fixtures/kakao/malformed.jsonl --json
    Expected: exits non-zero with row number and schema error, without dumping message text.
    Evidence: .omo/evidence/task-4-malformed-fixture.txt
  ```

  **Commit**: YES | Message: `feat(archive): add incremental sqlite ingestion` | Files: `crates/hype-core/src/archive/**`, `crates/hype-cli/src/commands/sync.rs`

- [ ] 5. Implement canonical Kakao chunker and reply-parent metadata

  **What to do**: Add `Chunker` that walks messages ordered by chat/time/message id. Start a new canonical chunk when sender nickname changes, chat changes, message type is non-text unsupported, or time gap exceeds threshold: 600 seconds for group chats, 1800 seconds for direct chats. Never split canonical chunks for size. Store all message ids in `chunk_messages`. Resolve reply parent message to parent chunk after chunking and write `chunk_parent_refs`; unresolved parent remains in `reply_edges` with reason `parent_not_in_archive`.
  **Must NOT do**: Do not let MinSync's recursive chunker define chat chunk boundaries.

  **Parallelization**: Can Parallel: NO | Wave 2 | Blocks: 6, 8, 9 | Blocked By: 4

  **References**:
  - User requirement: same nickname consecutive messages are one chunk.
  - API: `../MinSync/src/chunker/recursive.rs` - explains why default recursive chunking is not sufficient for canonical chat chunks.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_5_same_sender_chunking` fails before implementation and passes after.
  - [ ] `cargo test --workspace task_5_time_gap_splits_group_chat` fails before implementation and passes after.
  - [ ] `cargo test --workspace task_5_reply_parent_chunk_ref` fails before implementation and passes after.
  - [ ] `hype chunk get <child>` returns `parent_chunks` metadata for synthetic reply fixture.

  **QA Scenarios**:
  ```text
  Scenario: same sender chunk and reply metadata
    Tool: bash
    Steps: rm -rf .omo/evidence/hype-qa && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa sync --source fixture tests/fixtures/kakao/replies.jsonl --json && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa chunk get chunk_syn_child_reply --json
    Expected: output has one canonical child chunk with parent_chunks containing "chunk_syn_parent".
    Evidence: .omo/evidence/task-5-reply-chunk.json

  Scenario: group chat time gap split
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa sync --source fixture tests/fixtures/kakao/group_gap.jsonl --json && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa chunks --chat chat-group-gap --json
    Expected: same nickname before and after an 11-minute group gap appears in two chunk ids.
    Evidence: .omo/evidence/task-5-gap-split.json
  ```

  **Commit**: YES | Message: `feat(chunking): add kakao chunk semantics` | Files: `crates/hype-core/src/chunking/**`, `tests/fixtures/kakao/replies.jsonl`, `tests/fixtures/kakao/group_gap.jsonl`

- [ ] 6. Bridge canonical chunks into MinSync semantic index

  **What to do**: Add `SemanticIndexBridge` that writes generated MinSync source documents under `<data-dir>/semantic/source/chunks/`. Each semantic document includes YAML-like metadata lines for `chunk_id`, `chat_id`, `sender_nickname`, `time_range`, `parent_chunk_ids`, followed by chunk text. For chunks exceeding model max input, write multiple vector segment documents with `segment_index` but the same canonical `chunk_id`; semantic results are deduplicated by chunk id. Configure MinSync with local embedder id `tei:jinaai/jina-embeddings-v4`, `base_url = http://localhost:8080`, `query_prefix` and `passage_prefix` matching Jina retrieval prompts when supported, LanceDB dimension 2048, and high recursive max chunk size to avoid extra splitting. Add `hype index --full` and `hype index` commands.
  **Must NOT do**: Do not write generated MinSync source docs to the repo; do not use remote Jina API by default.

  **Parallelization**: Can Parallel: YES | Wave 3 | Blocks: 8, 10 | Blocked By: 2, 4, 5

  **References**:
  - API: `../MinSync/src/lib.rs` - available modules for library integration.
  - API: `../MinSync/src/config.rs` - embedder/vectorstore config.
  - API: `../MinSync/src/query.rs` - query result includes path/text/score.
  - Research: `jinaai/jina-embeddings-v4` README - 2048 default dense vectors, retrieval query/passage prompts.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_6_writes_stable_minsync_docs` fails before implementation and passes after.
  - [ ] `cargo test --workspace task_6_dedupes_vector_segments_by_chunk_id` fails before implementation and passes after.
  - [ ] `hype index --dry-run --json` reports changed canonical chunks without embedding calls.

  **QA Scenarios**:
  ```text
  Scenario: dry-run semantic source generation
    Tool: bash
    Steps: rm -rf .omo/evidence/hype-qa && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa sync --source fixture tests/fixtures/kakao/basic.jsonl --json && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa index --dry-run --json
    Expected: output lists generated semantic documents, chunk ids, and embedding_calls = 0.
    Evidence: .omo/evidence/task-6-index-dry-run.json

  Scenario: embedder unavailable
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa index --json
    Expected: exits non-zero with "local embedding server unavailable" and command hint, without falling back to remote API.
    Evidence: .omo/evidence/task-6-embedder-unavailable.txt
  ```

  **Commit**: YES | Message: `feat(semantic): add minsync index bridge` | Files: `crates/hype-core/src/semantic/**`, `crates/hype-cli/src/commands/index.rs`

- [ ] 7. Implement simple keyword and BM25 search

  **What to do**: Add FTS5 virtual table over canonical chunk text and minimal metadata. Implement `hype search keyword <query>` using deterministic match ordering by timestamp/chunk id with optional `--limit`. Implement `hype search bm25 <query>` using SQLite `bm25()` rank. Both return minimal snippets by default and metadata sufficient to locate chunk: chunk id, chat display name, sender nickname, time range, parent chunk ids.
  **Must NOT do**: Do not dump surrounding private history by default.

  **Parallelization**: Can Parallel: YES | Wave 3 | Blocks: 10 | Blocked By: 2, 4

  **References**:
  - Pattern: `AGENTS.md` - minimal snippets by default.
  - API: SQLite FTS5 `bm25()` ranking.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_7_keyword_exact_match_order` fails before implementation and passes after.
  - [ ] `cargo test --workspace task_7_bm25_ranks_more_relevant_chunk_first` fails before implementation and passes after.
  - [ ] CLI JSON output schema is identical except `ranker` and `score` semantics.

  **QA Scenarios**:
  ```text
  Scenario: keyword search
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa search keyword "보고서" --json
    Expected: output contains ranker "keyword", known synthetic chunk id, and snippet length <= configured default.
    Evidence: .omo/evidence/task-7-keyword.json

  Scenario: BM25 empty query
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa search bm25 "" --json
    Expected: exits non-zero with "empty query" and no SQL error.
    Evidence: .omo/evidence/task-7-bm25-empty.txt
  ```

  **Commit**: YES | Message: `feat(search): add keyword and bm25 search` | Files: `crates/hype-core/src/search/**`, `crates/hype-cli/src/commands/search.rs`

- [ ] 8. Implement semantic search CLI backed by MinSync

  **What to do**: Implement `hype search semantic <query>` that checks MinSync sync state, calls MinSync query, maps result paths/metadata to canonical chunk ids, deduplicates vector segments, hydrates chunk metadata from SQLite, and returns minimal snippets. Add `--show-segments` debug flag that still redacts raw text unless explicit `--format json --include-text` is provided.
  **Must NOT do**: Do not return generated MinSync document paths containing private user names unless explicitly requested in debug mode.

  **Parallelization**: Can Parallel: YES | Wave 3 | Blocks: 10 | Blocked By: 5, 6

  **References**:
  - API: `../MinSync/src/query.rs` - query behavior and errors.
  - API: `../MinSync/src/types.rs` - `QueryResult` fields.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_8_semantic_maps_result_to_chunk_id` fails before implementation and passes after using a mock MinSync query result.
  - [ ] `cargo test --workspace task_8_semantic_never_synced_error` fails before implementation and passes after.
  - [ ] `hype search semantic` returns actionable local embedder/index errors when not ready.

  **QA Scenarios**:
  ```text
  Scenario: semantic search with mock embedder
    Tool: bash
    Steps: HYPE_EMBEDDER=mock cargo run --bin hype -- --data-dir .omo/evidence/hype-qa search semantic "지난 회의 보고서" --json
    Expected: JSON contains ranker "semantic", canonical chunk_id, score, and no generated MinSync path by default.
    Evidence: .omo/evidence/task-8-semantic-mock.json

  Scenario: semantic before index
    Tool: bash
    Steps: rm -rf .omo/evidence/hype-empty && cargo run --bin hype -- --data-dir .omo/evidence/hype-empty search semantic "anything" --json
    Expected: exits non-zero with "semantic index has never been synced".
    Evidence: .omo/evidence/task-8-never-synced.txt
  ```

  **Commit**: YES | Message: `feat(search): add semantic search` | Files: `crates/hype-core/src/semantic/query.rs`, `crates/hype-cli/src/commands/search.rs`

- [ ] 9. Implement chunk retrieval and privacy-aware output controls

  **What to do**: Add `hype chunk get <chunk-id>` with `--json`, `--include-message-ids`, and `--redact` options. Default output includes full synthetic/private content only because user explicitly requested a chunk id; search remains snippet-only. Include parent/child reply metadata, source chat metadata, time range, sender sequence, and message count. Add `hype chunks --chat <id>` for QA/admin listing without text by default.
  **Must NOT do**: Do not expose SQLCipher source path, auth cache path, or embedding document path in chunk output.

  **Parallelization**: Can Parallel: YES | Wave 3 | Blocks: 10 | Blocked By: 4, 5

  **References**:
  - User requirement: chunk id must retrieve actual content.
  - Pattern: `AGENTS.md` - snippets minimal by default, exact output only on explicit request.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_9_chunk_get_includes_reply_backrefs` fails before implementation and passes after.
  - [ ] `cargo test --workspace task_9_chunk_get_unknown_id` fails before implementation and passes after.
  - [ ] `hype chunk get` returns full chunk text for a known synthetic chunk id.

  **QA Scenarios**:
  ```text
  Scenario: get known reply chunk
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa chunk get chunk_syn_child_reply --json
    Expected: JSON includes text, message_ids, parent_chunks, and no auth/db paths.
    Evidence: .omo/evidence/task-9-chunk-get.json

  Scenario: unknown chunk id
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa chunk get chunk_missing --json
    Expected: exits non-zero with structured not_found error.
    Evidence: .omo/evidence/task-9-chunk-missing.txt
  ```

  **Commit**: YES | Message: `feat(chunk): add chunk retrieval cli` | Files: `crates/hype-core/src/chunking/retrieve.rs`, `crates/hype-cli/src/commands/chunk.rs`

- [ ] 10. Add doctor, wipe-index, documentation, and README story

  **What to do**: Implement `hype doctor` checking macOS, Full Disk Access hint, `kakaocli` availability, helper availability, SQLite archive status, MinSync config, local embedder health, and ignored path warnings. Implement `hype wipe-index` that deletes semantic index/generated docs but preserves normalized archive unless `--all` is supplied. Update README with Hydrogen Peroxide story, adjacent project survey, install/setup, local Jina v4/v3 fallback, privacy model, and CLI examples.
  **Must NOT do**: Do not make wipe destructive without explicit flags and confirmation bypass for tests.

  **Parallelization**: Can Parallel: YES | Wave 4 | Blocks: 11 | Blocked By: 6, 7, 8, 9

  **References**:
  - Pattern: `README.md` - existing research notes to refresh.
  - Pattern: `/Users/jeffrey/.codex/skills/kakaotalk-mac/SKILL.md` - macOS permission failure modes.
  - External verification: `git ls-remote` confirmed `NomaDamas/MinSync`, `silver-flight-group/kakaocli`, `JungHoonGhae/openkakao-cli`, `teddylee777/kakaotalk-gpt`, and `jinaai/jina-embeddings-v4` exist.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_10_doctor_reports_missing_dependencies` fails before implementation and passes after.
  - [ ] `cargo test --workspace task_10_wipe_index_preserves_archive` fails before implementation and passes after.
  - [ ] README includes the project name story and states that remote embeddings are opt-in only.

  **QA Scenarios**:
  ```text
  Scenario: doctor JSON
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa doctor --json
    Expected: JSON includes macos, source_adapter, archive, semantic_index, embedder checks with pass/warn/fail states.
    Evidence: .omo/evidence/task-10-doctor.json

  Scenario: wipe semantic index only
    Tool: bash
    Steps: cargo run --bin hype -- --data-dir .omo/evidence/hype-qa wipe-index --yes --json && cargo run --bin hype -- --data-dir .omo/evidence/hype-qa search keyword "보고서" --json
    Expected: wipe reports semantic artifacts removed; keyword search still works from archive.
    Evidence: .omo/evidence/task-10-wipe-index.txt
  ```

  **Commit**: YES | Message: `docs(readme): document hype privacy and setup` | Files: `README.md`, `crates/hype-cli/src/commands/doctor.rs`, `crates/hype-cli/src/commands/wipe.rs`

- [ ] 11. Add thin agent skill wrapper plan artifact

  **What to do**: Add `skills/hype/SKILL.md` or equivalent local skill documentation that calls the CLI and summarizes results. It must explain privacy defaults, avoid owning indexing logic, and require exact user intent before printing full chunk content. This can be a repo-local draft unless the user wants it installed globally.
  **Must NOT do**: Do not embed KakaoTalk DB access logic in the skill.

  **Parallelization**: Can Parallel: YES | Wave 4 | Blocks: Final Verification | Blocked By: 10

  **References**:
  - Pattern: `AGENTS.md` - keep agent skill thin.
  - Pattern: `/Users/jeffrey/.codex/skills/kakaotalk-mac/SKILL.md` - privacy wording style.

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace task_11_skill_examples_match_cli_help` fails before implementation and passes after using snapshot/fixture checks.
  - [ ] Skill examples use `hype search ...` and `hype chunk get ...`; no direct DB or key handling.

  **QA Scenarios**:
  ```text
  Scenario: skill command examples
    Tool: bash
    Steps: rg -n "hype (search|chunk|get|sync)" skills/hype/SKILL.md && rg -n "SQLCipher key|auth-cache|KakaoTalk.db" skills/hype/SKILL.md
    Expected: first command finds CLI examples; second command finds no unsafe direct-secret handling except privacy warning text if intentionally included.
    Evidence: .omo/evidence/task-11-skill-rg.txt

  Scenario: full-content guard wording
    Tool: bash
    Steps: rg -n "chunk get|explicit" skills/hype/SKILL.md
    Expected: skill states full content is returned only for explicit chunk lookup or exact user request.
    Evidence: .omo/evidence/task-11-full-content-guard.txt
  ```

  **Commit**: YES | Message: `docs(skill): add thin hype skill wrapper` | Files: `skills/hype/SKILL.md`

## Final Verification Wave (MANDATORY - after ALL implementation tasks)
> ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.
- [ ] F1. Plan Compliance Audit
  - Command: `rg -n "TODO|DECISION NEEDED|remote embedding by default|raw Kakao" README.md crates skills tests`
  - Expected: no unresolved TODOs or unsafe defaults; privacy warnings are intentional.
- [ ] F2. Code Quality Review
  - Command: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  - Expected: all exit 0.
- [ ] F3. Real Manual QA
  - Command: run the full fixture flow in `.omo/evidence/hype-final-qa`: `doctor`, `sync`, `index --dry-run`, `search keyword`, `search bm25`, `search semantic` with mock/local embedder, `chunk get`, `wipe-index`.
  - Expected: every CLI command exits with the expected code and evidence files are captured.
- [ ] F4. Scope Fidelity Check
  - Command: `git diff --stat` plus manual review against this plan.
  - Expected: no source-level Kakao DB reverse engineering, no telemetry, no sending commands, no private data artifacts.

## Commit Strategy
- Use one Conventional Commit per task.
- Do not auto-commit unless the user explicitly authorizes commits.
- Each commit must be green for `cargo test --workspace` before moving to the next task.
- Suggested sequence:
  - `chore(scaffold): initialize hype rust workspace`
  - `feat(config): add private data paths`
  - `feat(source): add kakao source adapters`
  - `feat(archive): add incremental sqlite ingestion`
  - `feat(chunking): add kakao chunk semantics`
  - `feat(semantic): add minsync index bridge`
  - `feat(search): add keyword and bm25 search`
  - `feat(search): add semantic search`
  - `feat(chunk): add chunk retrieval cli`
  - `docs(readme): document hype privacy and setup`
  - `docs(skill): add thin hype skill wrapper`

## Success Criteria
- The `hype` CLI works through its actual command surface for sync, index, keyword search, BM25 search, semantic search, and chunk retrieval.
- Canonical chunks preserve same-nickname consecutive messages and time-gap splitting exactly as specified.
- Reply parent metadata supports parent chunk/message backtracking.
- MinSync performs incremental semantic indexing over generated chunk documents/segments and maps results back to canonical chunk ids.
- Local Jina embedding setup is documented and remote APIs are opt-in only.
- README contains the Hydrogen Peroxide naming story and updated related-project research.
- All tests and QA scenarios pass with synthetic data only.
