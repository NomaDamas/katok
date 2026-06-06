# Live MinSync / LanceDB / Jina Integration Notepad

## Goal
Fully connect Hydrogen Peroxide semantic indexing/search to a real local embedding flow using MinSync-compatible artifacts, LanceDB-backed vector storage, and a live Jina embedding HTTP endpoint, while preserving local-first privacy and mock/test ergonomics.

## Skills
- omo:ulw-loop: user explicitly requested ulw; evidence-bound scenarios required.
- omo:programming: Rust source/test edits.
- omo:debugging: runtime integration with external local embedding service/vector storage can fail in real execution.

## Success Criteria
1. Happy path: with a local fake Jina-compatible HTTP embedding server, `hype index --json` writes embeddings/vector index and `hype search semantic` returns ranked chunk ids from vector search, not keyword document scanning.
2. Edge path: without live Jina and without `HYPE_EMBEDDER=mock`, `hype index --json` fails safely with actionable local-server guidance and no private content leak.
3. Regression path: mock mode remains deterministic for synthetic QA; keyword/BM25/chunk lookup/configured paths still pass; all cargo gates pass.
4. Integration shape: semantic files/index layout is MinSync-compatible and uses LanceDB storage for live vector search.

## Evidence Plan
- RED/GREEN outputs in `.omo/evidence/live-minsync/red-green.txt`.
- tmux happy path transcript `.omo/evidence/live-minsync/C001-live-happy.txt`.
- tmux edge transcript `.omo/evidence/live-minsync/C002-live-edge.txt`.
- tmux regression transcript `.omo/evidence/live-minsync/C003-regression.txt`.

## Findings

## Final QA / Review
- RED/GREEN: `.omo/evidence/live-minsync/red-green.txt`
- Happy tmux QA: `.omo/evidence/live-minsync/C001-live-happy.txt`
- Edge tmux QA: `.omo/evidence/live-minsync/C002-live-edge.txt`
- Regression/cleanup audit: `.omo/evidence/live-minsync/C003-regression.txt`
- Code review blocker fixes: loopback parsed with `IpAddr::is_loopback`, semantic root/store forced through `ensure_private_dir`, large files split below 250 total lines.
- Final reviewers: goal PASS, QA PASS, security PASS, context PASS with caveats, code rereview PASS.
