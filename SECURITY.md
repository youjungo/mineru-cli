# Security Policy

## Reporting a Vulnerability

Please report security issues privately by opening a GitHub security advisory or contacting the repository owner. Do not publish API tokens, sample secrets, or private documents in public issues.

## Secret Handling

MinerU API tokens are user-provided credentials. The CLI can store tokens in the local `config.toml` file in plain text. Keep that file outside source control and do not share it.
