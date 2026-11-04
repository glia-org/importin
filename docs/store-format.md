# Apple Mail's store, as Importin reads it

The facts Importin relies on, as implemented (`src/store.rs`, `src/envelope.rs`, `src/paths.rs`,
`src/emlx.rs`, `src/mime.rs`). They were gathered for macOS 26.5 with Mail's `V10` store and are pinned by
the synthetic fixture (`tests/support/fixture.rs`); the ones marked *unverified* have not yet been checked
against a real store, and `importin doctor` reports numbers for each of them.

## Where the store is

- `<mail_root>/V<n>`, the highest `n` (`mail_root` defaults to `~/Library/Mail`); the version number is
  discovered, not hardcoded. The Envelope Index is `V<n>/MailData/Envelope Index`.
- Without Full Disk Access, a missing directory and a denied one are indistinguishable: `ENOENT`, `EPERM`
  and `EACCES` all report "missing or denied". Access is proven by reading the index's 16-byte SQLite
  header.

## The Envelope Index

**Opening.** `file:<path>?mode=ro` with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI | SQLITE_OPEN_NO_MUTEX`,
busy timeout 2 s, then one probe read (`SELECT count(*) FROM sqlite_master`), because SQLite opens lazily
and an unreadable `-wal` or `-shm` only shows on the first read. The index is in WAL mode: a reader sees a
consistent snapshot and never blocks Mail. `immutable=1` is never used: it hides un-checkpointed WAL
content (seen in practice: a 3.6 MB WAL newer than the main file). If the read-only open fails, the index
and its `-wal`/`-shm` are copied into a private temp dir and the copy is opened; an unreadable `-shm` is
left out (it is only the WAL's index and SQLite rebuilds it), an unreadable `-wal` is an error, so stale
data is never served silently. `doctor` reports which path is active (`read-only` or `snapshot`).

**Schema resolution** at startup, never hardcoded: `pragma_table_info` on `messages`, `subjects`,
`addresses`, `mailboxes`, `recipients`, `summaries`.

- Required: `messages(ROWID, date_received, subject, sender, mailbox)`, `subjects(subject)`,
  `addresses(address)`, `mailboxes(url)`. A missing one stops the server with the missing columns named
  on stderr.
- Optional, used when present: `messages(date_sent, subject_prefix, read, flagged, deleted,
  conversation_id, flags, size, summary)`, `addresses(comment)`, `recipients(message, address, type,
  position)` (used only when all four exist), `summaries(summary)` (used only with `messages.summary`).
  Without `messages.read`, mailbox unread counts come from `mailboxes.unread_count`.

**Joins.** `messages.sender` → `addresses.ROWID` directly; `messages.subject` → `subjects.ROWID`;
`messages.mailbox` → `mailboxes.ROWID`; `messages.summary` → `summaries.ROWID`. `addresses.comment` is the
display name.

**Subject prefix.** `subjects.subject` lacks the reply prefix; it sits in `messages.subject_prefix`
(`Re: `, `Fwd: `), so display and matching use `subject_prefix || subject`.

**Recipients.** `recipients.type` is 0 To, 1 Cc, 2 Bcc, ordered by `position`. They are fetched by a second
query per message, never by a join that would multiply rows; search terms on recipients use an `EXISTS`
subquery.

**Timestamps** are detected: `MAX(date_received)` ≥ 1e9 means Unix seconds (macOS 26), otherwise Cocoa
seconds since 2001-01-01 (offset 978 307 200). Values may be `REAL` or `INTEGER`; `date_sent` uses the same
epoch. `doctor` prints the detected one.

**Rows.** `deleted = 1` rows are left out of searches, threads and counts (a lookup by id still finds
them). `conversation_id` groups a thread. ROWIDs are not stable: Mail reassigns them when it rebuilds the
index, so the RFC `Message-ID` is the durable key.

**Accounts** come from `mailboxes.url`; nothing asks Mail. Two forms occur:

- `imap://user%40host@host/INBOX`: the user is percent-decoded; the account is the user when it holds an
  `@`, else `user@host`.
- `mailbox://<account-uuid>/INBOX`: the authority is the account's directory under `V<n>` and names the
  account.

The mailbox's display name is the URL path, percent-decoded (`Gesendete%20Objekte` → `Gesendete
Objekte`). `doctor` reports the form found (`imap-user-host`, `mailbox-uuid` or `mixed`).

## Files on disk

**Layout.** `V<n>/<account-uuid>/<Mailbox>.mbox[/<Sub>.mbox]/<store-uuid>/Data/<shard>/Messages/<ROWID>.emlx`
(or `<ROWID>.partial.emlx`); attachments at `…/Data/<shard>/Attachments/<ROWID>/<part>/<filename>`.

**ROWID to path**: compute first, walk second.

- Account directory: the uuid of a `mailbox://` URL; for a `user@host` URL, every directory under `V<n>`
  (except `MailData`) that holds the mailbox's `.mbox` chain, in name order.
- Each URL path segment becomes a `<Segment>.mbox` directory. The store uuid is the first (by name)
  36-character child that contains `Data`.
- Shard: the digits of `ROWID / 1000` reversed, one directory per digit (1 234 567 → `4/3/2/1`; ROWID
  below 1000 → no shard). *Unverified*: the formula comes from one reader's source.
- Try `<ROWID>.emlx`, then `<ROWID>.partial.emlx`. On a miss, walk the mailbox's `Data` trees once
  (skipping `Attachments`) and cache a ROWID → path map per mailbox, invalidated when the Envelope Index's
  mtime or size or its `-wal`'s mtime changes; a cached path that has gone costs one more walk. `doctor`
  reports how the 20 newest messages were found (formula, walk, missing), so a wrong formula shows up as
  numbers, not as missing mail.

**emlx framing.** `<decimal byte count><whitespace><that many bytes of RFC 822><XML plist>`. Any whitespace
ends the count, up to and including the first newline. Some files lose one dash of the closing MIME
boundary: when the payload's last line is `--token-` and the plist follows directly, the dash is restored
so the parser sees the closing boundary. Files over 64 MiB are refused before any byte is read.

**Flag bits** (the plist's `flags` integer; Importin reads `messages.flags` the same way): bit 0 read, 1
deleted, 2 answered, 3 encrypted, 4 flagged, 6 draft, bits 10–15 attachment count, 23 signed, 24 junk. Two
published layouts disagree; this is the one three parsers use. *Unverified* on a real store. The bits are
hints only (`search` reports `encrypted`, `signed` and `has_attachments` from them without opening a
file); the content type decides, and `doctor` cross-checks bit 3 against it on its sample.

**Attachment states**, decided per part (`.partial.emlx` is not a predicate):

| state | meaning |
|---|---|
| `inline` | the bytes are in the message body |
| `external` | empty body; the bytes are in `Attachments/<ROWID>/<part>/<filename>`, or found by a filename scan across the message's part directories (name order, dot directories skipped) |
| `not_downloaded` | empty body, no file, an `X-Apple-Content-Length` header (its value is the size): never fetched from the server |
| `not_extractable` | empty body, no file, no length header |

Filenames are untrusted: the path found is canonicalised and must stay inside the message's attachment
directory. The parts of a decrypted S/MIME message are read from the decrypted entity alone, without an
`Attachments/` lookup.

Several of these facts were first written down in hexrw/mailgrep's store-format notes (MIT); no code was copied.
