# Importin: a read-only MCP server for Apple Mail — design

Amended 2026-11-04 during implementation: see §8 (get_thread, search from, scope body needs a query, its
hints), §4 (account labels from the Accounts database), §6 (blank text/plain parts) and §7 (opaque
signed-only messages, the openssl fallback for signed-data).

Amended 2026-11-05 to match the code as built: §2 (what the read-only test checks), §3 (the `accounts`,
`read` and `log` modules), §7, §11 and §14 (the openssl decryptor runs the `openssl` binary,
`OpensslCli`), §8 (result objects, bounds, `body_html`, `note`), §10 (`openssl_bin`, `IMPORTIN_CONFIG`,
limits) and §15 (decision 11).

Status: specification, written 2026-11-04 from the decisions in `docs/design-panel-2026-11-04.html` and the
user's picks of the same day (all recommendations taken; runtime changed to Rust; name Importin). Built 2026-11-04 from
`docs/plan-2026-11-04.md`; amended 2026-11-05 to match the code as shipped in 0.1.0.

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
  network, no `osascript`. `tests/readonly.rs` reads every line of `src/**/*.rs` and fails on any of
  `osascript`, `ImapClient`, `smtp`, `send_mail`, `move_message`, `delete_message`, `set_flag`,
  `SQLITE_OPEN_READ_WRITE`, `SQLITE_OPEN_CREATE`, `immutable=1`, `fs::write`, `File::create`,
  `OpenOptions`, `Connection::open(` (case-insensitive; `tests/` may name them, and bare `imap` is allowed
  because `envelope` parses `imap://` URLs). A second check fails unless `src/` has an `open_with_flags`
  call and each one has `SQLITE_OPEN_READ_ONLY` on its line or in the six lines before it. No
  `--read-only` flag exists.
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
| `accounts` | read the account labels (username, name, type, by uuid) from `~/Library/Accounts/Accounts4.sqlite` once at startup, read-only (§4) | `envelope` (its read-only open), rusqlite |
| `paths` | map a message ROWID and mailbox URL to its `.emlx` / `.partial.emlx` file and its `Attachments/` directory | `store` |
| `emlx` | split an `.emlx` into byte count, raw RFC 822 bytes and the plist trailer; repair the single-dash boundary; read the flag bits | plist |
| `mime` | parse the raw message: headers, address lists, text body (plain preferred, HTML stripped as fallback), parts, attachment states | mail-parser |
| `smime` | detect enveloped messages, base64 the body to DER, decrypt through a `Decryptor` (`SecurityCms` in production, `OpensslCli` when configured and in tests; both run a binary), unwrap the signed layer, hand back an inner MIME entity | `mime` |
| `attach` | locate attachment bytes (inline, external, not downloaded, not extractable) and extract text (PDF, docx, xlsx, pptx, HTML, text) | `paths`, `mime`, pdf-extract, calamine, zip, quick-xml |
| `read` | compose one message for `get_email` and `attachment_text` (index row, file, MIME, S/MIME) and run the `scope: body` scan | `envelope`, `accounts`, `paths`, `emlx`, `mime`, `smime`, `attach` |
| `tools` | the MCP tool definitions and their JSON shapes; glue only | rmcp, all above |
| `doctor` | the CLI health check (§9) | all above |
| `config` | `config.toml` and environment overrides | — |
| `log` | the stderr logger, level from `IMPORTIN_LOG` (§9) | — |

Data flow of a `get_email(id)`: `envelope` fetches the row and its recipients → `paths` finds the file →
`emlx` splits it → `mime` parses → if enveloped, `smime` decrypts and `mime` parses the inner entity →
`read` assembles the response and `tools` returns it as JSON. Nothing is stored between calls except the
`paths` cache (§5).

## 4. The Envelope Index

- **Measured 2026-11-06** (README "Measured", `checks/measure.sh`, synthetic 100 000-message store, M2 Pro):
  start 15 ms, `subject` 15 ms, `get_email` 0.6 ms (S/MIME via openssl 23 ms), `get_thread` 7 ms, body
  scan of 500 candidates 60 ms, all within budget; `participant` 60 ms and `all` 74 ms miss the 50 ms
  header budget on that store (one address join per row and a recipient `EXISTS`; `ulower()` is used only
  for a non-ASCII term). The budget stands as the target; the page quotes the figures as measured.
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
  `config.exclude_accounts` holds glob patterns (matched as below); excluded accounts are invisible to
  every tool. The mailbox display name is the URL path, percent-decoded.
  *Amended 2026-11-04 (final fix wave):* the real store's URLs are `mailbox://<uuid>/<path>`, which carry
  no address, so the uuid is looked up in `~/Library/Accounts/Accounts4.sqlite` (`ZACCOUNT.ZIDENTIFIER` →
  `ZUSERNAME`, `ZACCOUNTDESCRIPTION`, `ZACCOUNTTYPE` → `ZACCOUNTTYPE.ZACCOUNTTYPEDESCRIPTION`), opened like
  the Envelope Index (read-only, `mode=ro`, never `immutable=1`) once at startup; `IMPORTIN_ACCOUNTS_DB`
  overrides the path (an environment variable only; there is no config key). The account's identifier is
  the username when it holds an `@`, else the description, else the uuid; an unreadable database leaves
  the uuids and never stops the server.
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
- Body text: the first `text/plain` part that is not blank (whitespace only); otherwise the first
  `text/html` part with tags stripped and entities decoded; otherwise empty with `body_source: "none"`.
  `body_source` says which. A `max_chars` argument truncates with a `truncated: true` marker, default
  20 000.
- Nested `message/rfc822` parts are listed as attachments of type `message/rfc822`, not descended into.

## 7. S/MIME

- **Detection.** Outer `Content-Type: application/pkcs7-mime` (or `x-pkcs7-mime`) with
  `smime-type=enveloped-data` or `authEnveloped-data`, or without `smime-type` and named `smime.p7m`; a
  `.p7m` deeper in the tree is an attachment. The flags bit 3 is a hint for `search` results
  (`encrypted: true` without opening the file), the content type is the decision.
- **Decrypting.** The body is base64-decoded to DER. `Decryptor::decrypt(der) -> Result<Vec<u8>>`:
  - `SecurityCms` (production): `/usr/bin/security cms -D` with DER on stdin and the inner entity on stdout,
    no temp files, 10 s timeout (the child is killed), the last 600 bytes of stderr captured into the
    error. Apple-signed binary, so the keychain ACL grant ("Always Allow" on the first prompt) survives
    Importin rebuilds. Uses the login keychain's identities; an identity that lives only in the
    iCloud/data-protection keychain is reachable by Mail but not by any other process. **Unverified on
    this Mac** (the temp-keychain probe failed with NSS -8147 "not a recipient" because that keychain was
    not on the search list); `doctor` runs the real check (§9).
    It is never handed `authEnveloped-data`: that is reported as `authEnveloped-data not supported by
    security cms` without running it.
  - `OpensslCli` (`smime.decryptor = "openssl"`, and the test path): `openssl cms -decrypt -inform DER
    -inkey <key> -recip <cert>` run as `smime.openssl_bin` (default `openssl` on `PATH`), DER on stdin,
    with the PEM key and cert from config (`[smime] key = …, cert = …`, both required with this decryptor)
    and the same timeout and stderr handling. No `openssl` crate is linked. For people whose key is not in
    a macOS keychain, and for `authEnveloped-data`, which it passes on (untested against real mail).
    Keychain-exported `.p12` files may need `-legacy` when converted to PEM (RC2-40).
- **Unwrapping.** The decrypted bytes are a MIME entity. If it is `multipart/signed`, the first part is
  the content and `signed: true`. If it is `application/pkcs7-mime; smime-type=signed-data`, run the decoder
  again (signed-data decodes without a key) and parse the result. `security cms -D` verifies the signature
  and fails on one it cannot verify; `SecurityCms` then decodes signed-data with `openssl cms -verify
  -noverify -nosigs` (`smime.openssl_bin`), the command `OpensslCli` uses from the start. Then the usual
  `multipart/alternative` or `mixed` handling of §6. Signature verification is **not** done in phase 1:
  the response carries `signature: "not verified"`. Signed-only (unencrypted) `multipart/signed` messages
  pass through §6 unchanged with `signed: true` and the `.p7s` part hidden from the attachment list.
  Opaque signed-only messages (top-level signed-data) are decoded the same way and their content is the
  message (body and attachments, `smime.p7m` not listed), with `encrypted: false, decrypted: false,
  signed: true`; when decoding fails, the outer message as it is plus `error`. The body scan does not
  decode them.
- **Response fields.** `encryption: {encrypted, decrypted, method: "security"|"openssl", signed, signature,
  error}`, `method` null when nothing was decrypted. On failure `decrypted: false`, `body: ""`, `error`
  with the decoder's message; the headers and attachment list from the outer message (less its
  `smime.p7m`) are still returned.
- **Search** never looks inside encrypted bodies. Subject, addresses and dates of encrypted mail are in the
  Envelope Index and searchable like any other.

## 8. Tools (MCP, stdio)

All tools are read-only and annotated `readOnlyHint: true` (`destructiveHint: false`, `idempotentHint:
true`, `openWorldHint: false`). Each returns one JSON object, as pretty-printed text and as
`structuredContent`. Dates in and out are ISO 8601 with offset; `after`/`before` accept `YYYY-MM-DD` too
(local midnight), and `after` must be earlier than `before`. Addresses are `{name, address}`. Errors are
MCP tool errors (`isError: true`) with a one-line message, for example "limit must be between 1 and 200;
got 0"; a bounded number outside its range is such an error, never clamped. `hint` is a field of the
`search` result, not of errors.

### `search`

Args: `query` (string, may be empty when filters do the work, except with `scope: body`), `scope` (`all` |
`subject` | `from` | `to` | `cc` | `participant` | `body`, default `all`, any case), `after`, `before`, `mailbox`
(substring of the display name), `account` (substring of the account identifier, §4), `from`
(substring of the sender's address or name; combines with any scope, `body` included), `unread` (bool),
`flagged` (bool), `limit` (1–200, default 25), `offset` (default 0).

Semantics: `query` is split on whitespace into terms; every term must match (AND) within the chosen
scope. Header scopes match case-insensitively (SQLite `LIKE`: ASCII letters only) as substrings of
`prefix || subject`, `address` or `comment` of the sender, the To list, the Cc list, or any of those
(`participant` = from + to + cc + bcc; `all` = subject + participant + `summaries.summary` when present).
Results ordered by `date_received` descending (relevance ranking is out of scope; dates are what the agent
prompt asks for).

`scope: body` needs a non-empty `query`; without one the call is a tool error ("scope body needs a query;
use another scope for header-only filters"). It runs the bounded scan: candidates are the header-filtered
rows (`after` defaults to `body_scan_days`, 90, before `before`, or before now, when absent;
`mailbox`/`account`/`from` narrow further), capped
at `config.body_scan_max` (default 500) newest rows; each candidate's `.emlx` is parsed and its text body
(§6; encrypted bodies are skipped) searched for all terms; stops when `limit` hits are found. The response
says `scanned`, `candidates_total`, `truncated` so the agent can narrow instead of guessing, `skipped`
(candidates whose file could not be located, read or parsed), and a `hint`:
"more hits may exist; raise limit or use offset" when the scan stopped at `limit` hits, else "narrow with
after:, from: or mailbox:" when the candidate cap was reached.

Returns `{hits, scanned, candidates_total, truncated, hint, skipped}`, `hits` = `[{id, date_received,
date_sent, subject, from, to, cc, mailbox, account, read, flagged, conversation_id, snippet, encrypted,
signed, has_attachments}]` where `snippet` is the Envelope Index summary when present, else null, and
`encrypted`, `signed`, `has_attachments` come from the index's flag bits (no file read). For header
scopes `scanned`, `candidates_total` and `hint` are null, `skipped` is 0, and `truncated: true` means
`limit` hits came back, so more may exist.

### `get_email`

Args: `id` (int), `max_chars` (500–200 000, default 20 000), `html` (bool, default false: also return the
first `text/html` part, raw and unstripped, as `body_html`, cut at `max_chars`).

Returns: `{id, message_id, in_reply_to, conversation_id, date_sent, date_received, subject, from, to, cc,
bcc, reply_to, mailbox, account, read, flagged, body, body_html, body_source, truncated, encryption,
attachments: [{index, part, filename, mime_type, size, state}]}`. `body_html` is null unless `html` is set
and the message has an HTML part; `truncated` says `body` or `body_html` was cut; `part` is the IMAP-style
part number ("2", "2.2").

### `get_thread`

Args: `id`, `limit` (1–200, default 50). Returns `{messages, truncated}`: the newest `limit` non-deleted
messages sharing the row's `conversation_id`, oldest first, in the `search` hit shape; `truncated: true`
when older messages of the conversation were left out. A message without a `conversation_id` (or a schema
without the column) comes back alone; an unknown or excluded `id` is a tool error.

### `attachment_text`

Args: `id`, `filename` (exactly as listed) or `index` (used when `filename` is absent or matches none),
`max_chars` (500–100 000, default 20 000). Returns `{filename, mime_type, state, text, truncated, note}`.
Text from (by extension first, then MIME type): PDF (pdf-extract on a thread of its own, panics caught,
20 s limit), docx (`word/document.xml` `<w:t>`), xlsx/xlsb (calamine's cell readers, sheet by sheet,
tab-separated; rows without a value and trailing empty cells left out; stops past 4 × `max_chars` bytes;
20 s limit; xls and ods are not read, calamine builds them as dense ranges and a crafted file would abort
the process), pptx
(`ppt/slides/slide*.xml` `<a:t>`), HTML (stripped), `text/*` and `.txt`/`.md`/`.csv`. Images and
everything else: empty `text` and a one-line `note` with the type and size; a document that fails to
parse gives empty `text` and a `note`, not an error. `not_downloaded` → error "<filename> was never
fetched from the server; open it in Mail once to download it"; `not_extractable` → error "<filename> is
declared but not present on disk". An encrypted message's attachments come from the decrypted entity; a
decryption failure is an error.

### `list_mailboxes`

Args: `account` (optional, case-insensitive substring of the account identifier). Returns `{mailboxes:
[{name, url, account, messages, unread}]}`, counts of non-deleted messages, excluded accounts omitted.

### `list_accounts`

Returns `{accounts: [{identifier, label, kind, uuid, user, host, scheme, mailboxes, messages}]}`, one
entry per identifier and uuid (`uuid` null for an `imap://` URL, `label` and `kind` null without an
Accounts database entry, §4), excluded accounts omitted.

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
  message is decrypted with `security cms -D -n` (content suppressed; 60 s timeout so the keychain dialog can
  be clicked) or, with `decryptor = "openssl"`, through the openssl decryptor, and only the exit status and
  stderr are reported (`--anon`: the status only). `--anon` prints no address, subject, name or path; counts, booleans, timings and hashes only.
  This is the only real-store output Claude sees.
- `importin --version`.

## 10. Configuration

`~/.config/importin/config.toml`, all keys optional:

```toml
mail_root = "~/Library/Mail"          # IMPORTIN_MAIL_ROOT overrides (tests point it at a fixture tree)
exclude_accounts = ["*@gmail.com"]     # globs on address, account name, type or uuid (§4)
body_scan_days = 90                    # default after: for scope: body, at most 36500
body_scan_max = 500                    # candidate cap for scope: body

[smime]
decryptor = "security"                 # or "openssl"
# key = "~/.config/importin/smime-key.pem"    # PEM path, openssl only, required with it
# cert = "~/.config/importin/smime-cert.pem"  # PEM path, openssl only, required with it
openssl_bin = "openssl"                # looked up on PATH: OpensslCli, and signed-data security refuses (§7)
```

Unknown keys are an error at startup (as apple-mail-mcp does; it catches typos). So are `body_scan_days`
above 36 500 and `decryptor = "openssl"` without both `key` and `cert`. `IMPORTIN_CONFIG` names another
config file, which must then exist (a missing default file means the defaults). `IMPORTIN_ACCOUNTS_DB`
sets the Accounts database path (§4); it has no config key. Paths here and in these variables may start
with `~/`.

## 11. Testing

- **Synthetic store.** `tests/support/fixture.rs` builds a temp `Mail/V10` tree: an Envelope Index created
  with the real table and column names used here (both epochs selectable), ~40 messages across two
  accounts and five mailboxes with To/Cc/Bcc, reply prefixes, deleted rows, a conversation; `.emlx` and
  `.partial.emlx` files covering inline, external, not-downloaded and not-extractable attachments, a
  single-dash boundary, HTML-only bodies, a `message/rfc822` part, a signed-only message, and one
  enveloped + signed message generated at test time by `openssl` with a throwaway cert (test skipped with a
  message if `openssl` is absent). Fixture attachments include a small PDF, docx, xlsx and pptx.
- **Unit tests** per module against the fixture; the `smime` tests use `OpensslCli` with the throwaway key.
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
- opencode (`~/mail/mail.json`, the `mail` key): `"command": ["/Users/<you>/.cargo/bin/importin"]`,
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
| R1 | `security cms -D` cannot find the identity (iCloud-only keychain, ACL) | `doctor` probe before M3 is called done; `OpensslCli` (`smime.decryptor = "openssl"`, PEM key and cert) as the documented fallback |
| R2 | `authEnveloped-data` (AES-GCM) unsupported by `security cms` | detected and reported as `error: "authEnveloped-data not supported by security cms"` without running `security`; `OpensslCli` passes it to `openssl cms -decrypt` (untested against real mail) |
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
| 2 | Runtime | **Rust**, the user, 2026-11-04, for speed: rmcp, rusqlite, mail-parser, plist, calamine, pdf-extract | Python + uv (panel D2's recommendation), Swift, Node |
| 3 | Metadata source | **live read-only Envelope Index**, no own index (D3) | snapshot copy per query; own mirror + watcher |
| 4 | Body search | **bounded on-demand scan**, phase 1 (D4 A); FTS accelerator only as phase 2 | eager index (today) |
| 5 | S/MIME | **`security cms -D` subprocess**, openssl PEM fallback (D5 A + C) | in-process CMSDecoder (`security-framework` has it; possible later once the binary is signed with a stable certificate) |
| 6 | Accounts | **from mailbox URLs**, no JXA (D6) | osascript |
| 7 | Tools | **search / get_email / get_thread / attachment_text / list_mailboxes / list_accounts**, key `mail` (D7) | keep the TS attachment tool |
| 8 | Code reuse | **port mailgrep's knowledge, no code** (D8 adapted to Rust) | vendor mailgrep modules (Python only) |
| 9 | Distribution | **glia-org/importin, Apache-2.0, cargo install + release binaries** (D9) | PyPI |
| 10 | Read-only | **by construction + source grep test**, no flag (D10) | a `--read-only` switch |
| 11 | Account labels | **the macOS Accounts database** (`Accounts4.sqlite`, read-only, once at startup), 2026-11-04 final fix wave: the real store's `mailbox://<uuid>/…` URLs carry no address; revises 6, still no JXA | uuids as names; osascript |
| 12 | Website | **A V2 P1 C2 S1 H1 M1 R2**, the user, 2026-11-06, from `docs/design-panel-website-2026-11-06.html`: the pore as the hero (an SVG animation of envelopes carried in through the nuclear pore, a still under reduced motion), serif prose with mono code, dark only, the full section set, `site/` in this repository published by GitHub Pages (needs the repository public: Pages on a private repository is a paid feature and glia-org is on the free plan), the pore-ring mark, a drawn stand-in labelled as such until a real screenshot exists; speed figures measured on a synthetic 100 000-message store, no figure of the author's store on the page. **Amended 2026-11-06 (the user): I3 + chase, S3**: click or tap feeds a letter to the pore, letters are draggable, one already inside stops at the pore from within (the pull test), free letters flee the pointer and gain speed when chased, an importin takes any herded within reach; the header is sticky and, while the hero is out of view, faint one-colour letters on their cups drift in from the window's sides and rise like balloons behind the page, bumping off one another, 6–10 a second, leaving above the top (earlier the same day: letters carried into the header's mark, dropped for this) (the scroll-linked turning of the ring and a scroll-paced letter in the mark were built and dropped the same day: the turn was not wanted). Earned arrivals (fed, dragged or herded by the visitor, idle ones not) get a small moment and light one subunit each; eight set off a short display, then the count resets. One local script, the page complete without it, everything still under reduced motion; nothing ever leaves through the pore | B transcript, C gate; V1 mono, V3 sans; P2 transcript, P3 still; C1 by system, C3 light; S2 short; H2 site repo, H3 folder in the Glia site repo; M2 envelope, M3 wordmark; R1 real screenshot, R3 none |
