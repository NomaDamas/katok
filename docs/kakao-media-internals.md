# KakaoTalk storage internals

Reverse-engineered detail behind `katok media get`, `katok sync --source macos`, and
`katok transcript`. Read it when debugging extraction or decryption; the README and
`skills/katok/SKILL.md` are the operational entry points.

Most of this was established empirically against a live install rather than from
documentation, so each claim below records how it was verified. It was originally
gathered for the separate `katok-followup` Python skill, whose functionality now lives
in this crate.

## SQLCipher open recipe

Order matters:

```
PRAGMA key = '<256-hex secure key>';
PRAGMA cipher_compatibility = 3;
```

Compatibility first resets cipher state and the open fails with `file is not a
database`.

### Key derivation

- Auth `{user_id, uuid}` is cached at `~/Library/Application Support/katok/kakao/auth.json`
  (mode 0600). The key material itself is never stored.
- The secure key is PBKDF2-HMAC-SHA256 (100000 rounds, dklen 128) over a reversed,
  F-joined string, salted with `uuid[len*0.3..]`, rendered as 256 lowercase hex.
  See `src/kakao/derive.rs`.
- **Self-check oracle:** the derived `database_name(user_id, uuid)` must equal the
  78-hex database filename actually on disk. That equality proves a derivation port is
  correct without needing a fixed test vector, and it is how the Python port was
  validated before this crate absorbed it.

## Paths

- Container: `~/Library/Containers/com.kakao.KakaoTalkMac/Data/Library/Application Support/com.kakao.KakaoTalkMac/`
- Database: the single 78-lowercase-hex file in that directory.

## `NTChatMessage` columns

`chatId, logId, authorId, type, message, sentAt, attachment, supplement`.

- `type`: 1 text, 2 single photo, 27 multi-photo album, 3 video, 0/feed system; others
  (12, 26, 71, 16385, …) are quotes, replies, links and other media.
- `attachment` is TEXT holding JSON. A single photo carries `{w,h,cs,s,mt,url,thumbnailUrl,k}`;
  an album carries parallel arrays `{wl,hl,csl,sl,mtl,kl,imageUrls,thumbnailUrls}`.
- `cs` (single) and `csl[i]` (album) are the SHA-1 of the **decoded plaintext image**.
  Decrypting a cached file and hashing the result must equal it, which is what makes
  decryption verifiable at all.
- System feed rows are JSON carrying `feedType`/`inviter`/`member`/`leaver`/`members`.
  They stay in the archive; `katok transcript` filters them out of the rendered output.

## On-disk media cache

```
<container>/<account 40-hex sha1>/<sha1(reverse(chatId))>/
    single photo (type 2):
      <sha1(reverse("p" + logId))>.img            full
      <sha1(reverse("t" + logId))>.thm            thumbnail
    album frame i (type 27):
      <sha1(reverse("p{i}_{logId}"))>.img         full,  i = 0..N-1
      <sha1(reverse("t{i}_{logId}"))>.thm         thumbnail
    video (type 3): thumbnail only, single-photo scheme
```

There can be more than one account directory.

### Album naming, reverse-engineered

An album frame's storage id is the **string** `"{idx}_{logId}"`, not the raw integer
logId used for single photos, so its stem is `sha1(reverse("p" + "{idx}_{logId}"))`.
The AES key is unchanged: `reverse("#{logId}%")`, shared by every frame of the album —
frames differ by file, not by key.

Established without disassembly, using the `cs` oracle as the decisive test: decrypt a
candidate file with the parent logId key and check `sha1(decoded) == csl[idx]`.

- One room: 4/4 album full frames verified, 11/11 thumbnails valid.
- Across 7 rooms: 40/40 album full frames verified, 225/225 thumbnails valid. The scheme
  is chat-independent; only the folder is per-chat.
- Every file in a sampled room folder was then accounted for — single `p`/`t` + logId,
  album `p`/`t` + `idx_logId`, video `t` + logId — leaving zero unexplained files.

### Account directory discovery is slow, exactly once

Enumerating the container can be very slow on a long-lived install: APFS keeps a bloated
directory b-tree after millions of media files churn, and a cold scan of about seven live
entries measured 100-340 seconds on the machine this was found on. Access to a known
account directory is instant — only discovery is slow. `MediaDirs::discover` therefore
enumerates once and reuses the result. The scan is bounded and fails loudly: a container
that cannot be listed raises rather than returning an empty list, which would silently
turn every image into a stub.

### Pkv2 container

```
bytes[0..4]   "Pkv2" magic
bytes[4..20]  IV (16 bytes)
bytes[20..]   AES-256-CBC ciphertext, PKCS7
keyString     reverse("#" + logId + "%")
aesKey        SHA256(utf8(keyString))
plaintext     256 ASCII-hex header bytes (skipped) followed by the image
```

Thumbnails use the **same** per-logId key as the full image.

## "Viewed but not found" is lazy caching, not a bug

Symptom: a photo the app clearly displays resolves to `not-cached`, and scanning recent
files for that logId finds nothing.

KakaoTalk populates the on-disk cache lazily, so "the app can show it" does not imply
"the bytes are on disk":

- Inline previews stream from the CDN.
- The `.thm` thumbnail is written when the message renders, on scroll into view.
- The full `.img` is written only when the user opens the photo.

A resolve is a point-in-time snapshot, so `not-cached` is correct at that instant.
Reproduced deterministically: a message that arrived at 12:04 had an `.img` with mtime
15:29, exactly when the user opened it, and only a `.thm` (mtime 14:39) before that. The
standard-named `.img` did exist and decrypted correctly, with `sha1(decoded) == cs` and
decoded size matching attachment `s`. The two alternate hypotheses were ruled out: the
bytes live at the standard Pkv2 path rather than a separate viewed-cache, thumbnail
database, WebKit store or temp location, and single photos use no per-image naming
variant.

So `not-cached` is a legitimate self-correcting state — open the photo in the app and
re-run. It is a fetch gap, not a decryption failure, which is why `decrypt-failed` stays
reserved for a file that exists and fails to decrypt.

## CDN tier

Every image attachment carries its own presigned CDN URL (`url` for a single photo,
`imageUrls[i]` for an album frame) with `credential`, `expires` and `signature` query
parameters, valid roughly three days from send. When the full image is not cached
locally, the resolver fetches that URL and keeps the bytes only if `sha1(body) == cs`.
This is the only network access in the crate, and `--no-cdn` turns it off for a purely
local run.

**An attachment with no `cs` skips the tier entirely.** There would be nothing to check
a fetched body against, and writing unverified network bytes under a contract that
promises verification is worse than not having the image: the reason lands in
`tier_reason` as `cdn-unverifiable` and an error record says so. An expired signature
short-circuits without a network call (`cdn-expired`), and a failed fetch or a
fingerprint mismatch records a `stage: "cdn"` error before falling through to thumbnail
and stub.

## WAL read consistency

The KakaoTalk database runs in WAL journal mode and the app holds it open while running,
so a `-wal` file with uncheckpointed commits is normally present.

**Invariant: do not open this database `immutable` or `-readonly`.** Doing so silently
drops the WAL, so freshly arrived rows that are still WAL-resident vanish from a read
while a normal read still shows them — the exact "in the database but missing from the
export" symptom. A read-write open coordinates through `-shm` and applies the WAL.

Verified against a live 1.7 MB `-wal`: a normal read and an `immutable=1` read returned
identical `COUNT(*)`, `MAX(logId)` and `MAX(sentAt)`, with no row visible to one and not
the other. A message that lands after a run's snapshot appears on the next run, which is
correct MVCC rather than staleness.
