# Importin: a read-only MCP server for Apple Mail — design

Amended 2026-11-04 during implementation: see §8 (get_thread, search from, scope body needs a query) and
§4 (account labels from the Accounts database).

Status: specification, written 2026-11-04 from the decisions in `docs/design-panel-2026-11-04.html` and the
user's picks of the same day (all recommendations taken; runtime changed to Rust; name Importin). Nothing is
built yet. The implementation plan follows this document.

## Name

Importin, chosen 2026-11-04 after the name round (importin, sensillum, caveola, pinosome, podocyte; twenty
candidates checked against the family's conditions). The cell: importin-α reads the nuclear localization
signal, the address tag on a cargo protein; importin-β carries the complex through the gated nuclear pore and
releases it inside. Exportins carry the outbound traffic. Importin reads the envelope, passes the seal, hands
the contents over, and never sends. Crate and binary `importin`, config `~/.config/importin/config.toml`,
GitHub `glia-org/importin`, Apache-2.0. In opencode the MCP server key stays `mail`, so the tools keep
appearing as `mail_*` and the existing `mail` agent and its permission map keep working.

## 1. Purpose and non-goals

**Purpose.** Let an AI agent (the opencode `mail` agent on a local Qwen, Claude Code, Claude Desktop) answer
questions about the mail Apple Mail already keeps on this Mac: find messages by who sent them, who received
them, who was in Cc, subject, date, mailbox; read one message, including S/MIME-encrypted ones, with the key
that is already in the login keychain; read attachments as text. Fast enough to feel like a local function,
no index to build, no watcher to babysit.

**Non-goals.** Sending, replying, moving, flagging, deleting (no code path exists). PGP. Exchange/EWS
accounts (Mail keeps no local files for them). Gmail labels. Semantic search. Writing plaintext of encrypted
mail anywhere. AppleScript or JXA of any kind. Running on anything but macOS 13+ with Apple Mail's `V10`
store (the version directory is discovered, not hardcoded).

## 2. Rules

- **Read-only by construction.** SQLite opened `SQLITE_OPEN_READ_ONLY`, `.emlx` opened for reading, no
  network, no `osascript`. A test greps the source tree for `osascript`, `imap`, `smtp`, `send_`, `move_`,
  `delete_`, `set_flag` and fails on a hit outside the test itself. No `--read-only` flag exists.
- **Never read the user's mail while developing.** All tests run on the synthetic store (§10). Real-store
  checks produce only numbers and booleans (`doctor --anon`), or go through an opencode session instructed to
  answer with booleans and counts.
- **No plaintext at rest.** Decrypted content lives in the response only. No cache of bodies on disk in
  phase 1 (the phase-2 accelerator, if ever built, indexes unencrypted mail only).
- **Loud failures.** A missing Full Disk Access grant, an unknown schema, or a decrypt failure is reported as
  such; an empty answer never stands in for an error.
- **Budgets** (release build, Apple Silicon, 100k-message store): process start to first MCP reply < 50 ms;
  `search` on header scopes < 50 ms; `get_email` < 30 ms unencrypted, < 300 ms encrypted; a bounded body
  scan ≤ 2 s at its candidate cap.

## 3. Architecture

One Rust crate, one binary, no runtime dependencies beyond macOS. Modules, each with one job:

| module | job | depends on |
|---|---|---|
| `store` | find `~/Library/Mail/V<n>` (highest n), classify access errors (FDA missing vs. absent store vs. undetermined), locate the Envelope Index | — |
| `envelope` | open the Envelope Index read-only, resolve the schema at runtime, detect the timestamp epoch, run the metadata queries (search, get, thread, mailboxes, accounts) | `store`, rusqlite |
| `paths` | map a message ROWID and mailbox URL to its `.emlx` / `.partial.emlx` file and its `Attachments/` directory | `store` |
| `emlx` | split an `.emlx` into byte count, raw RFC 822 bytes and the plist trailer; repair the single-dash boundary; read the flag bits | plist |
| `mime` | parse the raw message: headers, address lists, text body (plain preferred, HTML stripped as fallback), parts, attachment states | mail-parser |
| `smime` | detect enveloped messages, base64 the body to DER, decrypt through a `Decryptor` (`SecurityCms` in production, `OpensslPem` as fallback and in tests), unwrap the signed layer, hand back an inner MIME entity | `mime` |
| `attach` | locate attachment bytes (inline, external, not downloaded, not extractable) and extract text (PDF, docx, xlsx, pptx, HTML, text) | `paths`, `mime`, pdf-extract, calamine, zip, quick-xml |
| `tools` | the MCP tool definitions and their JSON shapes; glue only | rmcp, all above |
| `doctor` | the CLI health check (§9) | all above |
| `config` | `config.toml` and environment overrides | — |

Data flow of a `get_email(id)`: `envelope` fetches the row and its recipients → `paths` finds the file →
`emlx` splits it → `mime` parses → if enveloped, `smime` decrypts and `mime` parses the inner entity →
`tools` shapes the response. Nothing is stored between calls except the `paths` cache (§5).

## 4. The Envelope Index

- **Opening.** `Connection::open_with_flags(uri, READ_ONLY | URI | NO_MUTEX)` with
  `file:<path>?mode=ro`. WAL mode is Mail's; readers see a consistent snapshot and never block Mail. Busy
  timeout 2 s. Every query is one short read transaction. **`immutable=1` is never used**: it hides
  un-checkpointed WAL content (seen in practice: a 3.6 MB WAL newer than the main file). If the read-only
  open fails (observed with some permission states), fall back to a snapshot copy of DB + `-wal` + `-shm`
  into a private temp dir and open that; `doctor` reports which path is active.
- **Schema resolution at startup**, never hardcoded: `PRAGMA table_info` on `messages`, `subjects`,
  `addresses`, `mailboxes`, `recipients`, `summaries`. Required: `messages(ROWID, date_received, subject,
  sender, mailbox)`, `subjects(subject)`, `addresses(address)`, `mailboxes(url)`. Optional, used when
  present: `messages(date_sent, subject_prefix, read, flagged, deleted, conversation_id, flags, size)`,
  `addresses(comment)`, `recipients(message, address, type, position)`, `summaries(summary)`. A missing
  required column aborts the server with the actual schema printed to stderr.
- **Facts the queries rely on** (macOS 26.5, from the vendored mailgrep notes and three other readers):
  `messages.sender` joins `addresses.ROWID` directly; `subjects.subject` lacks the reply prefix, which sits
  in `messages.subject_prefix`, so display and matching use `prefix || subject`; `recipients.type` is 0 To,
  1 Cc, 2 Bcc, ordered by `position`; recipients are fetched by a correlated subquery or a second query per
  message, never by a join that multiplies rows; `conversation_id` groups a thread; `deleted = 1` rows are
  excluded unless asked for.
- **Timestamps are detected.** Sample `MAX(date_received)`: a value ≥ 1e9 means Unix seconds (macOS 26),
  otherwise Cocoa seconds (offset 978307200). `doctor` prints the detected epoch.
- **Accounts** are derived from `mailboxes.url` (`imap://user%40host@host/INBOX` → user, host). No JXA.
  `config.exclude_accounts` holds glob patterns on `user@host`; excluded accounts are invisible to every
  tool. The mailbox display name is the URL path, percent-decoded.
  *Amended 2026-11-04 (final fix wave):* the real store's URLs are `mailbox://<uuid>/<path>`, which carry
  no address, so the uuid is looked up in `~/Library/Accounts/Accounts4.sqlite` (`ZACCOUNT.ZIDENTIFIER` →
  `ZUSERNAME`, `ZACCOUNTDESCRIPTION`, `ZACCOUNTTYPE` → `ZACCOUNTTYPE.ZACCOUNTTYPEDESCRIPTION`), opened like
  the Envelope Index (read-only, `mode=ro`, never `immutable=1`) once at startup; `IMPORTIN_ACCOUNTS_DB`
  overrides the path. The account's identifier is the username when it holds an `@`, else the
  description, else the uuid; an unreadable database leaves the uuids and never stops the server.
  Exclusion globs are matched, lowercased, against user, host, `user@host`, uuid, username, description
  and type, empty ones skipped (`["Google"]` excludes the Google account).

## 5. Files on disk

- **Layout.** `V10/<account-uuid>/<Mailbox>.mbox[/<Sub>.mbox]/<store-uuid>/Data/<shard>/Messages/<ROWID>.emlx`
  (or `.partial.emlx`), attachments at `…/Data/<shard>/Attachments/<ROWID>/<part>/<filename>`.
- **Resolving a ROWID to a path**: compute first, walk second. Candidate: the mailbox URL's authority is the
  account uuid, its path segments become `<Segment>.mbox` directories, the store uuid is the first
  36-character child that contains `Data`, the shard is the digits of `ROWID / 1000` reversed, one directory
  per digit (1234567 → `4/3/2/1`; ROWID < 1000 → no shard). Try `<ROWID>.emlx` then `<ROWID>.partial.emlx`.
  On a miss, walk that mailbox's `Data` tree once and cache a ROWID → path map for the mailbox, invalidated
  when the Envelope Index's mtime or size changes. The shard formula is from one reader's source and is
  **unverified here**; `doctor` reports how many sampled ROWIDs resolved by formula vs. by walk, so a wrong
  formula shows up as numbers, not as missing mail.
- **emlx.** `<decimal count><whitespace><RFC 822 bytes><XML plist>`. Any whitespace ends the count. If the
  payload ends in a lone `-` and a plist follows, restore the missing dash. The trailer's `flags` integer:
  bit 0 read, 1 deleted, 2 answered, 3 encrypted, 4 flagged, 6 draft, bits 10–15 attachment count,
  bit 23 signed, 24 junk (two published layouts disagree; this is the one three parsers use; the fixture
  tests pin it and `doctor` cross-checks bit 3 against the content type on a sample).
- **`.partial.emlx` is not a predicate.** Attachment state is decided per part: `inline` (bytes in the
  body), `external` (empty body, bytes in `Attachments/`), `not_downloaded` (`X-Apple-Content-Length`
  header, empty body, no file), `not_extractable` (declared, nothing found). Resolution order: inline bytes,
  the exact part directory, then a filename scan across the message's part directories skipping dotfiles.
  Filenames are untrusted: resolve the path and check it stays inside the message's attachment directory.
- **ROWIDs are not stable.** Mail reassigns them on index rebuilds. `get_email` returns the RFC `Message-ID`
  header as `message_id` beside the numeric `id`; agents are told to use ids within a session only.

## 6. MIME and text

- `mail-parser` on the raw bytes. Headers of interest: From, To, Cc, Bcc, Reply-To, Date, Subject,
  Message-ID, In-Reply-To, References, Content-Type. Address lists are returned as `[{name, address}]`.
- Body text: the first `text/plain` part; otherwise the first `text/html` part with tags stripped and
  entities decoded; otherwise empty with `body_source: "none"`. `body_source` says which. A `max_chars`
  argument truncates with a `truncated: true` marker, default 20 000.
- Nested `message/rfc822` parts are listed as attachments of type `message/rfc822`, not descended into.

## 7. S/MIME

- **Detection.** Outer `Content-Type: application/pkcs7-mime` with `smime-type=enveloped-data` or
  `authEnveloped-data`, or an `application/pkcs7-mime` part named `smime.p7m`. The flags bit 3 is a hint
  for `search` results (`encrypted: true` without opening the file), the content type is the decision.
- **Decrypting.** The body is base64-decoded to DER. `Decryptor::decrypt(der) -> Result<Vec<u8>>`:
  - `SecurityCms` (production): `/usr/bin/security cms -D` with DER on stdin and the inner entity on stdout,
    no temp files, 10 s timeout, stderr captured into the error. Apple-signed binary, so the keychain ACL
    grant ("Always Allow" on the first prompt) survives Importin rebuilds. Uses the login keychain's
    identities; an identity that lives only in the iCloud/data-protection keychain is reachable by Mail but
    not by any other process. **Unverified on this Mac** (the temp-keychain probe failed with NSS -8147 "not
    a recipient" because that keychain was not on the search list); `doctor` runs the real check (§9).
  - `OpensslPem` (fallback, and the test path): `openssl` crate `CmsContentInfo::decrypt` with a PEM key and
    cert from config (`[smime] key = …, cert = …`). For people whose key is not in a macOS keychain. Needs
    `-legacy` handling documented for Keychain-exported `.p12` files (RC2-40).
- **Unwrapping.** The decrypted bytes are a MIME entity. If it is `multipart/signed`, the first part is
  the content and `signed: true`. If it is `application/pkcs7-mime; smime-type=signed-data`, run the decoder
  again (signed-data decodes without a key) and parse the result. Then the usual `multipart/alternative` or
  `mixed` handling of §6. Signature verification is **not** done in phase 1: the response carries
  `signature: "not verified"`. Signed-only (unencrypted) messages pass through §6 unchanged with
  `signed: true` and the `.p7s` part hidden from the attachment list.
- **Response fields.** `encryption: {encrypted, decrypted, method: "security"|"openssl", signed, signature,
  error}`. On failure `decrypted: false`, `body: ""`, `error` with the decoder's message; the headers and
  attachment list from the outer message are still returned.
- **Search** never looks inside encrypted bodies. Subject, addresses and dates of encrypted mail are in the
  Envelope Index and searchable like any other.

## 8. Tools (MCP, stdio)

All tools are read-only and annotated `readOnlyHint: true`. Dates in and out are ISO 8601 with offset;
`after`/`before` accept `YYYY-MM-DD` too. Addresses are `{name, address}`. Errors are MCP tool errors with a
one-line message and, where useful, a `hint` (for example "narrow with after: or scope: subject").

### `search`

Args: `query` (string, may be empty when filters do the work, except with `scope: body`), `scope` (`all` |
`subject` | `from` | `to` | `cc` | `participant` | `body`, default `all`), `after`, `before`, `mailbox`
(substring of the display name), `account` (substring of the account identifier, §4), `from`
(substring of the sender's address or name; combines with any scope, `body` included), `unread` (bool),
`flagged` (bool), `limit` (1–200, default 25), `offset`.

Semantics: `query` is split on whitespace into terms; every term must match (AND) within the chosen
scope. Header scopes match case-insensitively as substrings of `prefix || subject`, `address` or
`comment` of the sender, the To list, the Cc list, or any of those (`participant` = from + to + cc + bcc;
`all` = subject + participant + `summaries.summary` when present). Results ordered by `date_received`
descending (relevance ranking is out of scope; dates are what the agent prompt asks for).

`scope: body` needs a non-empty `query`; without one the call is a tool error ("scope body needs a query;
use another scope for header-only filters"). It runs the bounded scan: candidates are the header-filtered
rows (`after` defaults to 90 days before now when absent; `mailbox`/`account`/`from` narrow further), capped
at `config.body_scan_max` (default 500) newest rows; each candidate's `.emlx` is parsed and its text body
(§6; encrypted bodies are skipped) searched for all terms; stops when `limit` hits are found. The response
says `scanned`, `candidates_total`, `truncated` so the agent can narrow instead of guessing.

Returns: `[{id, date_received, subject, from, to, cc, mailbox, account, read, flagged, encrypted, signed,
has_attachments, snippet}]` where `snippet` is the Envelope Index summary when present, else null.

### `get_email`

Args: `id` (int), `max_chars` (default 20 000), `html` (bool, default false: return the stripped HTML
body as well).

Returns: `{id, message_id, in_reply_to, conversation_id, date_sent, date_received, subject, from, to, cc,
bcc, reply_to, mailbox, account, read, flagged, body, body_source, truncated, encryption, attachments:
[{index, filename, mime_type, size, state}]}`.

### `get_thread`

Args: `id`, `limit` (1–200, default 50). Returns `{messages, truncated}`: the newest `limit` non-deleted
messages sharing the row's `conversation_id`, oldest first, in the `search` result shape; `truncated: true`
when older messages of the conversation were left out. A message without a `conversation_id` (or a schema
without the column) comes back alone; an unknown or excluded `id` is a tool error.

### `attachment_text`

Args: `id`, `filename` (exactly as listed) or `index`, `max_chars` (500–100 000, default 20 000).
Returns `{filename, mime_type, state, text, truncated}`. Text from: PDF (pdf-extract, panics caught), docx
(`word/document.xml` `<w:t>`), xlsx/xls/ods (calamine, sheet by sheet, tab-separated), pptx
(`ppt/slides/slide*.xml` `<a:t>`), HTML (stripped), `text/*`. Images and everything else: a one-line note
with the size. `not_downloaded` → error "never fetched from the server; open it in Mail once".

### `list_mailboxes`

Args: `account` (optional substring). Returns `[{name, url, account, messages, unread}]`, excluded
accounts omitted.

### `list_accounts`

Returns `[{identifier, label, kind, uuid, user, host, scheme, mailboxes, messages}]` (`uuid` null for an
`imap://` URL, `label` and `kind` null without an Accounts database entry, §4), excluded accounts omitted.

Dropped from 0.5.0: `get_email_links`, `get_email_attachment` (wrote files to disk), `get_attachment`, the
`index://status` resource, every `filter=` preset (expressible with `unread`/`flagged`/`after`).

## 9. CLI

- `importin` or `importin serve`: the MCP server on stdio. Logs to stderr only, level from
  `IMPORTIN_LOG` (default `warn`).
- `importin doctor [--anon] [--json]`: Full Disk Access (actually opens the Envelope Index; ENOENT is
  reported as "missing or denied: indistinguishable"), store version directory, schema resolution (each
  optional column found or not), detected epoch, message/mailbox/account counts, excluded accounts,
  Mail.app running (pgrep), Envelope Index mtime (a lower bound on freshness), ROWID→path resolution on a
  sample (formula hits, walk hits, misses), flag-bit cross-check on a sample, keychain S/MIME identities
  (`security find-identity -v -p smime`, count only), and the **decrypt probe**: the newest enveloped
  message is decrypted with `security cms -D -n` (content suppressed) and only the exit status and stderr
  are reported. `--anon` prints no address, subject, name or path; counts, booleans, timings and hashes only.
  This is the only real-store output Claude sees.
- `importin --version`.

## 10. Configuration

`~/.config/importin/config.toml`, all keys optional:

```toml
mail_root = "~/Library/Mail"          # IMPORTIN_MAIL_ROOT overrides (tests point it at a fixture tree)
exclude_accounts = ["*@gmail.com"]     # globs on address, account name, type or uuid (§4)
body_scan_days = 90                    # default after: for scope: body
body_scan_max = 500                    # candidate cap for scope: body

[smime]
decryptor = "security"                 # or "openssl"
key = ""                               # PEM, openssl only
cert = ""                              # PEM, openssl only
```

Unknown keys are an error at startup (as apple-mail-mcp does; it catches typos).

## 11. Testing

- **Synthetic store.** `tests/support/fixture.rs` builds a temp `Mail/V10` tree: an Envelope Index created
  with the real table and column names used here (both epochs selectable), ~40 messages across two
  accounts and five mailboxes with To/Cc/Bcc, reply prefixes, deleted rows, a conversation; `.emlx` and
  `.partial.emlx` files covering inline, external, not-downloaded and not-extractable attachments, a
  single-dash boundary, HTML-only bodies, a `message/rfc822` part, a signed-only message, and one
  enveloped + signed message generated at test time by `openssl` with a throwaway cert (test skipped with a
  message if `openssl` is absent). Fixture attachments include a small PDF, docx, xlsx and pptx.
- **Unit tests** per module against the fixture; the `smime` tests use `OpensslPem` with the throwaway key.
- **Protocol tests** spawn the release binary with `IMPORTIN_MAIL_ROOT` set, speak JSON-RPC over stdio
  (`initialize`, `tools/list`, one `tools/call` per tool, an error case each) and check shapes and
  budgets (§2) on the fixture.
- **Read-only test**: the source grep of §2.
- **Real store**, anonymised: `importin doctor --anon --json` run by the user or by a subagent, numbers
  only. Then an opencode smoke run with the `mail` agent through `opencode api` (the `run` subcommand
  hangs intermittently on this Mac), prompt fixed to answer "yes/no/count" questions: did `search` with
  `scope: cc` return rows, did `get_email` return a non-empty `cc`, did an encrypted message decrypt. Tool
  call logs are hashed before Claude sees them.
- **Decrypt on the real keychain**: `doctor`'s probe, run by the user; the first run may show the keychain
  ACL dialog for `security`.

## 12. Distribution and integration

- Repo `glia-org/importin`, Apache-2.0, `THIRD_PARTY.md` for crate licences. Design notes that were
  learned from mailgrep (MIT) are credited in `docs/store-format.md`; no code is copied.
- `cargo install importin` from crates.io; GitHub release binaries for `aarch64-apple-darwin` and
  `x86_64-apple-darwin` built by Actions; a Homebrew tap later if the family gets one.
- opencode (`~/mail/mail.json`, the `mail` key): `"command": ["/Users/paul/.cargo/bin/importin"]`,
  `enabled: true`, timeout as today. The `mail` agent prompt gets the new scopes (`to`, `cc`, `participant`,
  `body` with its bounds) and loses the `mail_attachment_text` note and the "do not use
  get_email_attachment" line; `tools/mail.ts` and `attachment_text.py` retire. Full Disk Access stays with
  the app that launches opencode (Glia); the Automation→Mail grant is no longer needed by anything.
- Claude Code / Claude Desktop: the same binary path in their MCP config.

## 13. Phases

| phase | delivers | verified by |
|---|---|---|
| M0 | crate skeleton, `config`, `store`, `doctor` without the decrypt probe, fixture generator | `doctor --anon` on the real store shows FDA ok, schema resolved, epoch, counts |
| M1 | `envelope`, tools `search` (header scopes), `list_mailboxes`, `list_accounts`, `get_thread` | protocol tests; opencode smoke: a Cc question answered |
| M2 | `paths`, `emlx`, `mime`, `get_email` (unencrypted), attachment states | fixture tests; doctor path-resolution sample on the real store |
| M3 | `smime` with both decryptors, `doctor` decrypt probe | openssl path in tests; user runs doctor on the real keychain |
| M4 | `scope: body` bounded scan | fixture tests; timing on the real store (numbers) |
| M5 | `attachment_text` | fixture tests |
| M6 | opencode integration, agent prompt edit, README, release workflow, `HANDOFF.md` | end-to-end opencode smoke, budgets measured |

Phase 2 (not planned, only if M4's bounded scan proves too slow in use): an FTS5 accelerator in
`~/.cache/importin/` over unencrypted bodies, where the Envelope Index stays the truth and the cache can
only speed up, never hide, a message.

## 14. Risks and open points

| # | risk | handling |
|---|---|---|
| R1 | `security cms -D` cannot find the identity (iCloud-only keychain, ACL) | `doctor` probe before M3 is called done; `OpensslPem` as the documented fallback |
| R2 | `authEnveloped-data` (AES-GCM) unsupported by `security cms` | detected and reported as `error: "authEnveloped not supported by security cms"`; openssl path covers it |
| R3 | shard formula wrong on this store | walk fallback; doctor reports formula vs. walk hits |
| R4 | flag-bit layout differs from the assumed one | content type decides encryption; bits are hints; doctor cross-checks |
| R5 | Envelope Index schema changes with a macOS release | runtime resolution; required-column failure prints the schema |
| R6 | read-only open refused while Mail runs | snapshot-copy fallback; doctor says which path is active |
| R7 | `opencode run` hangs | `opencode api` with timeouts in the smoke harness |
| R8 | someone reserves the name | reserve crates.io, PyPI, npm, GitHub, `.dev` now |

## 15. Decisions

| # | decision | picked | alternatives (panel D-numbers) |
|---|---|---|---|
| 1 | Name | **Importin**, the user, 2026-11-04, after the cell-biology round | sensillum, caveola, pinosome, podocyte; paper-knife (panel D1, rejected: not in the family) |
| 2 | Runtime | **Rust**, the user, 2026-11-04 ("can we use rust because its pretty fast?"): rmcp, rusqlite, mail-parser, plist, calamine, pdf-extract | Python + uv (panel D2's recommendation), Swift, Node |
| 3 | Metadata source | **live read-only Envelope Index**, no own index (D3) | snapshot copy per query; own mirror + watcher |
| 4 | Body search | **bounded on-demand scan**, phase 1 (D4 A); FTS accelerator only as phase 2 | eager index (today) |
| 5 | S/MIME | **`security cms -D` subprocess**, openssl PEM fallback (D5 A + C) | in-process CMSDecoder (`security-framework` has it; possible later once the binary is signed with a stable certificate) |
| 6 | Accounts | **from mailbox URLs**, no JXA (D6) | osascript |
| 7 | Tools | **search / get_email / get_thread / attachment_text / list_mailboxes / list_accounts**, key `mail` (D7) | keep the TS attachment tool |
| 8 | Code reuse | **port mailgrep's knowledge, no code** (D8 adapted to Rust) | vendor mailgrep modules (Python only) |
| 9 | Distribution | **glia-org/importin, Apache-2.0, cargo install + release binaries** (D9) | PyPI |
| 10 | Read-only | **by construction + source grep test**, no flag (D10) | a `--read-only` switch |
