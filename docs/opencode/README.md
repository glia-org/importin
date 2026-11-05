# Switching the opencode `mail` agent to Importin

Do this after `importin doctor` is green (keychain dialog accepted, `exclude_accounts` set in
`~/.config/importin/config.toml`).

1. `~/mail/mail.json`: replace the `mail` server's `command` with `["/Users/<you>/.cargo/bin/importin"]`
   (keep `"type": "local"`, `enabled`, `timeout`).
2. `~/mail/agents/mail.md`: replace with `mail.md` from this directory (new scopes `from|to|cc|participant|
   subject|body`, `after:`/`from:` narrowing, `mail_get_thread`, `mail_attachment_text` now a server tool).
3. `~/mail/install.sh`: drop the `tools/mail.ts` link; `tools/mail.ts` and `tools/attachment_text.py` retire.
4. `opencode service restart`, then the yes/no/count smoke run through `opencode api`
   (`session.create` with `agent: "mail"`, `session.prompt`, `experimental.session.wait`,
   `session.message.list`).
