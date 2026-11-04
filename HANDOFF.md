# Handoff

## State (2026-11-04)

Branch `impl`. Implementation tasks 1–11 of [`docs/plan-2026-11-04.md`](docs/plan-2026-11-04.md) are
done: the crate (`store`, `envelope`, `accounts`, `paths`, `emlx`, `mime`, `smime`, `attach`, `read`,
`tools`, `doctor`, `config`, `log`), the binary (`importin`, `importin serve`, `importin doctor [--anon]
[--json]`, `importin --version`), the read-only source grep, the budget check, the docs and the release
workflow, the final fix wave (account labels from the macOS Accounts database, the deferred rulings
below), and the fix wave after the whole-branch review (listed under "Since resolved"). `cargo test`: 177
passed, 1 ignored (the release budget test), no warnings. Everything has run on the synthetic store only;
nothing has read `~/Library/Mail` yet. Task 12 (real store and opencode switch) remains.

## Build, test, run

```sh
cargo build --release          # target/release/importin
cargo test                     # all tests, on a synthetic store built in a temp dir
checks/budgets.sh              # release build; latency budgets on a 2 000-message fixture
cargo install --path .         # ~/.cargo/bin/importin
importin doctor --anon --json  # the real store, no account names, paths or addresses
```

The tests never touch `~/Library/Mail` or `~/Library/Accounts`: `tests/support/fixture.rs` builds a
`Mail/V10` tree and a synthetic `Accounts4.sqlite`, and the binary is pointed at them with
`IMPORTIN_MAIL_ROOT` and `IMPORTIN_ACCOUNTS_DB` (a test that builds a `Config` must set both). The S/MIME
tests need `openssl` on `PATH` (here Homebrew's OpenSSL 3; they skip without it). `tests/doctor.rs` runs
`security find-identity -v -p smime` on the real keychain (read-only, count only).

Budgets (DESIGN §2), last measured 2026-11-04 on this Mac (Apple Silicon), release build, 2 000 messages,
after one warm-up launch:

| measure | budget | measured (4 runs) |
|---|---|---|
| process start to `initialize` reply | < 50 ms | 13.9–14.5 ms |
| one header `search` (scope subject, "Filler") | < 50 ms | 5.4–6.1 ms |
| one `get_email` (plain message) | < 30 ms | 0.8–1.3 ms |

DESIGN states the budgets for a 100 000-message store; the fixture has 2 000. The first launch of a
freshly built binary costs ~0.7 s before `main` on macOS (a policy check), hence the warm-up.

## Unverified on a real store

- **Shard formula** (`ROWID / 1000`, digits reversed, one directory each): from one reader's source. A
  wrong formula costs a walk, not mail; `doctor`'s path sample shows formula vs. walk hits.
- **`security cms` on the real keychain**: whether it finds Mail's S/MIME identity on the keychain search
  list, whether the "Always Allow" dialog appears and sticks, and identities that live only in the iCloud
  keychain. `doctor`'s decrypt probe is the check.
- **`authEnveloped-data`**: refused before calling `security`; the openssl path passes it to `openssl cms
  -decrypt` but has never decrypted real AES-GCM data.
- **Flag-bit layout** (bit 3 encrypted, 23 signed, 10–15 attachment count, …): two published layouts
  disagree. The content type decides encryption; `doctor`'s flag check compares bit 3 against it.
- **Account labels**: `ZACCOUNT`/`ZACCOUNTTYPE` of `~/Library/Accounts/Accounts4.sqlite` were checked by
  hand for the four Mail uuids; `accounts::load` itself has only read the fixture. If it fails, accounts
  show their uuids (logged at debug).
- Seen on the real store on 2026-11-04 (`docs/store-format.md`, last section): URL form `mailbox-uuid`
  throughout, read-only open accepted (no snapshot), shard formula 20/20, flag bit 3 wrong on 2 of 20,
  3 of 4 account labels resolved, 120 378 messages answered in 2–3 ms. Still unseen: the snapshot fallback,
  the `all`/`participant` timings on the real store (added after that run), and the keychain dialog
  being accepted (the first probe timed out waiting for the click).

## Known gaps

- **Residual from the final review (not fixed):** calamine's `worksheet_range` allocates rows × cols cells;
  a ~1 KB xlsx with cells at A1 and XFD1048576 passes the zip-size guard and can abort the process on
  allocation failure, which `catch_unwind` cannot catch. Fix: cap the cell count before
  `worksheet_range` or read the sheet dimension first. Until then a hostile spreadsheet attachment can take
  the server down (opencode restarts it; nothing is written or leaked).

Collected from the review ledger (`.superpowers/sdd/plan-2026-11-04/progress.md`, kept out of git), the
`minor (deferred)` lines as written, by task.

**Task 1**

- UnknownKey detected by toml error-message prefix (config.rs:114-120); nested unknown key untested; load() home-dir fallback.

**Task 2**

- no test proves immutable=1 is never used (would need rows left only in the WAL); readonly grep in Task 11 covers the string.
- huge_emlx fixture file has no plist trailer (fixture.rs:371-376); no committed self-check of fixture invariants; store.rs Undetermined and non-SQLite-header branches untested; chmod-000 test fails under root; fixture `base` from wall clock (cross-epoch test may flake across UTC midnight); `V+5` parses as 5; flag bits 2/6/24 never set in fixture.

**Task 3**

- LIKE case-folding is ASCII-only (MÜLLER vs Müller); tempfile moved to runtime deps (needed for snapshot); mailboxes listed in ROWID order.
- schema-fallback branches untested except conversation_id; unread filter without `read` column degenerates; sub-second precision dropped in after/before; thread() deleted-anchor inconsistency; Counts.messages includes rows with no mailbox row; some date-ordered tests Unix-only; tests hard-code ROWIDs that have ids names; envelope.rs 892 lines (URL parsing could split).
- exclusion check and main query are two reads, not one transaction (microsecond window for a mailbox created in between).

**Task 4**

- user@host URL candidates include same-named mailboxes of other accounts (first sorted hit wins; ROWIDs unique); config.rs fails cargo fmt --check (pre-existing).
- read_dir denial inside .mbox/Data reported as Ok(None) (store_dirs, file_type Err → continue); walk picks first file per ROWID in DFS order (.partial may beat .emlx in a wrong shard); every Mail write invalidates walk caches via -wal mtime (cost shows in doctor formula/walk counts); fixture has no non-ASCII mailbox name (NFC/NFD untested).

**Task 5**

- strip_html runs words together at `</td>` and opening block tags; '>' inside quoted attribute ends a tag; source newlines kept as line breaks; `<title>` text leaks; decode_entities quadratic on '&' without ';'; control-char entities decode; filename scan may claim another part's file with duplicate names; find_external errors silently become NotDownloaded/NotExtractable; Unavailable Display uses Debug casing; no absolute-path filename test; unnamed text/calendar parts invisible; mime.rs 532 lines (html part could split).
- entity() returns first char only (two-codepoint HTML5 entities lose the second); invalid numeric entities → U+FFFD; metadata/read race on external files.

**Task 6**

- NotEnveloped also covers "enveloped but body not DER"; opaque-signed check uses signed_der().is_some(); config.rs rustfmt (pre-existing).
- openssl() test helper splits on whitespace (TMPDIR with spaces breaks it); try_wait error path kills without wait and reports Spawn; reader joins unbounded after normal exit; auth-enveloped policy keyed on name string; sleep-5 test script without exec leaves an orphan; no stderr-flood test; DER check line duplicated in mime.rs; opaque signed-data without DER/smime-type falls through as unsigned.

**Task 7**

- limit == 0 reports truncated; config.rs rustfmt (pre-existing).
- inner parse failure after decrypt fails the whole read; p7m filter case-sensitive; skipped not Option; duplicated load code; skips_encrypted test could pass vacuously; weak field-source tests; empty-terms body scan reads all candidates (decide at MCP layer in Task 9: require non-empty query for scope body); body_scan_days unbounded (chrono overflow for absurd values) — cap in config at 36500 (Task 9/11).

**Task 8**

- zip/xlsx inflated-size not bounded; pdf_extract has no time limit; catch_unwind relies on panic=unwind in the release profile (do not set panic=abort).
- slide ordering untested (fix round adds it); encrypted test skips silently without openssl; unbounded zip entry decompression; unknown XML entities dropped silently.
- search's body_text still has its own locate/read/parse copy.

**Task 9**

- three direct eprintln! in envelope.rs bypass the log format.
- scope description hard-codes "90 days"; unread:false/flagged:false behave as absent; protocol tests do not exercise mailbox/unread/flagged/offset; instructions checked by prefix only; envelope.rs raw eprintln!.
- (final fix wave) tools::date gap fallback panics on NaiveDate::MIN in positive-offset zones (use checked arithmetic → parse error); DESIGN §8 get_thread text needs newest-limit + truncated; IMPORTIN_TEST_STRAY_PRINT hook ships in release (harmless).

**Task 10**

- mail root without `V<n>` reports FDA "undetermined"; smime_identities None on non-zero exit too; doctor tests call `security find-identity` on the real keychain (read-only, count only).
- doctor tests spawn `security find-identity` (not hermetic); missing tests for failing probe / FAIL rows / security branch; scrub over-redacts "ProtonMail/".
- a certificate name on the first stderr line of the security probe would still show under --anon (status code suffices; consider dropping stderr entirely under anon in the final wave).
- scrub word-start set lacks < [ { ; and relative ../ paths; \r not a line end; cert CN on first stderr line survives; anon_probe_stderr test vacuous without openssl.

Since resolved: the `immutable=1` check (Task 2) is `tests/readonly.rs`; `config.rs`'s rustfmt drift
(Tasks 4, 6, 7) went in `style: cargo fmt`; the empty-query body scan and the `body_scan_days` cap (Task 7)
were settled in Task 9 (tool error; at most 36500); slide ordering (Task 8) is tested in `src/attach.rs`;
DESIGN §8 `get_thread` (Task 9) was amended in Task 11. The final fix wave resolved: the p7m filter's
case and the duplicated load code (Task 7), `body_text`'s own locate/read/parse copy (Task 8), the raw
`eprintln!`s in envelope.rs, the hard-coded "90 days" in the scope description, the missing protocol
coverage of `mailbox`/`unread`/`flagged`/`offset` and the `tools::date` overflow panic (Task 9), and the
certificate name in the probe's stderr under `--anon`, now dropped entirely (Task 10). The fix wave after
the whole-branch review resolved: zip inflation (Task 8; each docx/pptx part read is cut at 64 MiB, a
workbook whose declared sizes pass 64 MiB is refused, both with the note "attachment inflates past the 64
MiB limit"; the release profile now says why `panic = "abort"` must stay out), the missing non-ASCII
mailbox name (Task 4; the fixture's `Gesendete Objekte Ä` is NFC in the URL and NFD on disk), and the
opaque signed-only ruling (Task 7; such messages are now decoded, see DESIGN §7). It also made a missing
`IMPORTIN_CONFIG` file an error, skipped blank `text/plain` parts for the body, added `search_all` and
`search_participant` to doctor's timings, gave `security cms` an openssl fallback for signed-data it
refuses, made `serve` exit 2 when the store will not open, matched `scope` in any case, split the body
scan's hint (stopped at `limit` vs. candidate cap), and cut the tool error log lines to the error's
variant name.

Deferred rulings, not listed above: a body scan with `before` but no `after` defaults `after` to
`before − body_scan_days` (Task 9; done in the final fix wave); opaque signed-only messages (pkcs7-mime
signed-data, not encrypted) stayed undecoded (Task 7; reversed in the fix wave after the whole-branch
review: `get_email` and `attachment_text` decode them, the body scan still does not).

Found while writing the docs:

- DESIGN §4 says a missing required column aborts "with the actual schema printed to stderr". Done in
  the final fix wave: `serve` and `doctor` print `EnvelopeError::Schema`'s `dump` to stderr.
- DESIGN drift left in place: §2's grep list (`imap`, `send_`, `move_`, `delete_`, …) differs from
  `tests/readonly.rs`; §7 names `OpensslPem` and the `openssl` crate, but the code shells out to `openssl
  cms` (`OpensslCli`, `smime.openssl_bin`); §8 `get_email`'s `html` returns the raw HTML part as
  `body_html`, not a stripped one; §8 lacks `get_email`'s `max_chars` bounds (500–200 000).

## Next steps

1. Done 2026-11-04: installed at `~/.cargo/bin/importin`, `doctor --anon` numbers in
   `docs/store-format.md`. Open before the switch: (a) the user runs `importin doctor` once and clicks
   "Always Allow" in the keychain dialog so the decrypt probe can pass; (b) the user writes
   `~/.config/importin/config.toml` with `exclude_accounts` for the account that must stay invisible
   (patterns match address, account name, account type or uuid; `importin doctor` lists the
   identifiers). Without (b) every account is visible to the agent.
2. Done 2026-11-04: opencode switched (`~/mail/mail.json` → `~/.cargo/bin/importin`, new `mail` agent
   prompt from `docs/opencode/mail.md`, `tools/mail.ts` retired), `exclude_accounts` set to the one unnamed
   IMAP account, keychain grant accepted (decrypt probe 50 ms). Live probe on the real store through the
   stdio client, counts only: cc search 5 ms, get_email with To/Cc 1 ms, participant search 8 ms,
   body scan 5 hits after 192 of 500 candidates in 91 ms, thread 1 ms. The opencode Qwen session
   "importin smoke 2026-11-04" ran (13 k input / 459 output tokens); its transcript was not read by
   Claude (classifier), the user checks it in the TUI.
3. Before `cargo publish`: the release workflow runs on the first `v*` tag once the repo is on GitHub
   (`Cargo.toml` now carries `license`, `description`, `repository`, `keywords`, `categories`, `readme`).
