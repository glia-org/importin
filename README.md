# Importin

A read-only MCP server for Apple Mail. Importin reads what Mail.app already keeps on this Mac, the
Envelope Index (SQLite) and the `.emlx` message files, and gives an AI agent six tools: find messages by
sender, recipient, Cc, subject, date and mailbox; read one message, S/MIME-encrypted ones included; read an
attachment as text. No index to build, no watcher, no network. It cannot send, move, flag or delete mail:
no code path for it exists, and a test greps the source to keep it that way. The name is the cell:
importin reads the address tag on a cargo protein, carries it through the gated nuclear pore and releases
it inside; exportins carry the outbound traffic.

## Install

```sh
cargo install --path .     # from this checkout, into ~/.cargo/bin/importin
cargo install importin     # once it is published on crates.io
```

Then run `importin doctor` (below) from the app that will launch your MCP host.

**Full Disk Access.** `~/Library/Mail` sits behind Full Disk Access, and macOS asks the app at the top of
the process chain, not importin: grant it to the app that launches the MCP host (the terminal or Glia for
opencode and Claude Code, Claude.app for Claude Desktop) in System Settings → Privacy & Security → Full
Disk Access. Without it the server exits at startup with "missing or access denied (Full Disk Access not
granted?)" on stderr, and `importin doctor` shows `FAIL full disk access`.

## Tools

| tool | key arguments | returns |
|---|---|---|
| `search` | `query` (words; all must match), `scope` = `all` (default) \| `subject` \| `from` \| `to` \| `cc` \| `participant` \| `body`, `after`/`before` (RFC 3339 or `YYYY-MM-DD` = local midnight), `mailbox`, `account`, `from` (substrings), `unread`, `flagged`, `limit` 1–200 (25), `offset` | `hits`, newest first: `id`, dates, subject, from/to/cc, mailbox, account, read, flagged, encrypted, signed, has_attachments, `snippet`; `truncated` |
| `get_email` | `id`, `max_chars` 500–200 000 (20 000), `html` (also return the raw HTML part) | headers with to/cc/bcc/reply_to, `message_id`, `body` and `body_source` (plain, html, none), `encryption`, `attachments` with their `state` |
| `get_thread` | `id`, `limit` 1–200 (50) | the newest `limit` messages of the conversation, oldest first, in the search hit shape; `truncated` |
| `attachment_text` | `id`, `filename` or `index` as `get_email` lists them, `max_chars` 500–100 000 (20 000) | text of PDF, Word (docx), Excel/ODS, PowerPoint (pptx), HTML and text attachments; other types get a one-line note |
| `list_mailboxes` | `account` (substring, optional) | mailboxes with message and unread counts |
| `list_accounts` | none | accounts with mailbox and message counts |

The header scopes query the Envelope Index: case-insensitive substrings (ASCII letters only), `all` =
subject, sender, every recipient and Mail's summary, `participant` = sender, To, Cc and Bcc. `scope: body`
reads message files instead and needs a query: the candidates are the newest `body_scan_max` rows that
pass the other filters, received since `after` (default `body_scan_days` ago); encrypted bodies are never
searched; the reply adds `scanned` and `candidates_total`, and `truncated` with a hint to narrow when the
scan stopped at `limit` hits or the candidate cap was reached. Ids are only good within a session (Mail
renumbers rows when it rebuilds its index); `message_id` is durable. Deleted messages are left out of
searches, threads and counts; excluded accounts are invisible to every tool.

## Configuration

`~/.config/importin/config.toml`, or the file named by `IMPORTIN_CONFIG`. Every key is optional; these are
the defaults. An unknown key is an error at startup.

```toml
mail_root = "~/Library/Mail"    # IMPORTIN_MAIL_ROOT overrides it
exclude_accounts = []           # globs, e.g. ["*@gmail.com"], matched against user, host, user@host
                                # and the account uuid; an excluded account is invisible to every tool
body_scan_days = 90             # default window of scope body, at most 36500
body_scan_max = 500             # candidate cap of scope body

[smime]
decryptor = "security"          # or "openssl"
# key = "~/.config/importin/smime-key.pem"    # openssl only, required with it
# cert = "~/.config/importin/smime-cert.pem"  # openssl only, required with it
openssl_bin = "openssl"         # the openssl binary, looked up on PATH
```

Logs go to stderr only (stdout is the MCP channel); `IMPORTIN_LOG` = `error` | `warn` (default) | `info` |
`debug` | `trace`.

## Doctor

```sh
importin doctor           # a checklist
importin doctor --json    # the same as JSON
importin doctor --anon    # no account names, patterns, paths or addresses
```

It checks Full Disk Access (by opening the Envelope Index), the store directory, the schema (which
optional columns exist), whether the index is read in place or from a snapshot copy, the timestamp epoch,
message, mailbox and account counts, excluded accounts, whether Mail is running, the index's mtime, the
mailbox URL form, how the 20 newest messages' files were found (formula, walk, missing), the encrypted
flag bit against the content type on the same sample, the number of S/MIME identities in the keychain, a
decrypt probe on the newest encrypted message, and three timings. It exits 0 when Full Disk Access is
granted and the schema resolved, else 2. The report never holds message content or subjects; `--anon`
also drops the account identifiers, exclusion patterns and mail root, replaces paths, URLs and addresses
in error text with `<path>`, `<url>` and `<addr>`, and keeps only the first line of the probe's stderr.

## S/MIME

Encrypted messages are decrypted per request; the plaintext lives in the response only and is never
written to disk. Signatures are not verified: a signed message says `signature: "not verified"`.

- **`security` (default).** importin pipes the encrypted body to `/usr/bin/security cms -D`, which finds
  the private key among the identities of the keychain search list, normally your login keychain (`security
  find-identity -v -p smime` lists them; `doctor` counts them). An identity that lives only in the iCloud
  keychain is not visible to it.
- **First run.** macOS asks whether `security` may use the private key: choose **Always Allow**. The grant
  belongs to Apple's signed `security`, so it survives rebuilds and updates of importin. Run `importin
  doctor` once from a terminal to get the dialog while you are there: its probe waits 60 s, the server's
  decryptor gives up after 10 s.
- **`authEnveloped-data`** (AES-GCM) is not supported by `security cms`; importin reports
  `authEnveloped-data not supported by security cms` instead of trying. The openssl decryptor passes it on
  (OpenSSL 3 supports it; this path is untested).
- **openssl fallback**, for a key that is not in a macOS keychain or for `authEnveloped-data`: export the
  identity as `.p12`, convert it to PEM, and point the config at it.

  ```sh
  # add -legacy to both if OpenSSL 3 refuses the file
  openssl pkcs12 -in me.p12 -nocerts -nodes -out ~/.config/importin/smime-key.pem
  openssl pkcs12 -in me.p12 -clcerts -nokeys -out ~/.config/importin/smime-cert.pem
  chmod 600 ~/.config/importin/smime-key.pem
  ```

  ```toml
  [smime]
  decryptor = "openssl"
  key = "~/.config/importin/smime-key.pem"
  cert = "~/.config/importin/smime-cert.pem"
  openssl_bin = "/opt/homebrew/bin/openssl"    # OpenSSL 3; /usr/bin/openssl is LibreSSL
  ```

  The key file holds your private key unencrypted: keep it readable by you alone and out of shared
  backups.

## MCP clients

The binary is `~/.cargo/bin/importin`; it speaks MCP on stdio. JSON configs are not passed through a shell,
so write the path out in full there.

opencode (`opencode.json`; the key `mail` makes the tools appear as `mail_*`):

```json
{
  "mcp": {
    "mail": {
      "type": "local",
      "command": ["/Users/you/.cargo/bin/importin"],
      "enabled": true
    }
  }
}
```

Claude Code (the shell expands `~`):

```sh
claude mcp add --scope user mail -- ~/.cargo/bin/importin
```

## Layout

| path | what |
|---|---|
| `src/` | the crate, one module per job: `store`, `envelope`, `paths`, `emlx`, `mime`, `smime`, `attach`, `read`, `tools`, `doctor`, `config`, `log` |
| `tests/` | integration tests on a synthetic Mail store built at test time (`tests/support/fixture.rs`); `readonly.rs` greps `src/` for write paths |
| `checks/budgets.sh` | the latency budgets, release build, 2 000-message fixture |
| `docs/` | the store format notes, the design panel, the implementation plan |

## More

- [`DESIGN.md`](DESIGN.md): the specification and its decisions.
- [`docs/store-format.md`](docs/store-format.md): what importin relies on in Apple Mail's store.
- [`HANDOFF.md`](HANDOFF.md): the state, how to build and test, what is unverified on a real store.
- [`THIRD_PARTY.md`](THIRD_PARTY.md): the licences of the dependencies.

Apache-2.0, see [`LICENSE`](LICENSE).
