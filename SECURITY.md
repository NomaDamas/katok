# Security Policy

## Supported versions

Security fixes target the latest released version and the `main` branch.

## Reporting a vulnerability

Use GitHub's **Security > Report a vulnerability** flow so details remain
private. Include the affected version, impact, reproduction steps using only
synthetic data, and a proposed mitigation when available.

If private vulnerability reporting is unavailable, open a public issue asking
the maintainers for a private contact channel. Do not include exploit details,
KakaoTalk data, credentials, tokens, database material, or presigned URLs in a
public issue.

## Data handling

Never attach a real KakaoTalk database, katok archive, transcript, media file,
auth cache, or private search output to an issue or pull request. Reproductions
must use the synthetic fixtures under `tests/fixtures/` or newly generated
synthetic data.
