# SDD ledger — plan: docs/plan-2026-11-04.md
Spec: DESIGN.md (reachable). Branch: impl (created from main 2c3c1bc; new repo, no remote).
Ruling: work on branch `impl` in the main checkout, no worktree — new repo, nothing else runs here; cost if wrong: none (branch is isolated).

## Preflight scan (2026-11-04)
| pair / task | produces vs consumes | finding | ruling |
|---|---|---|---|
| T1 config ↔ T6 smime | SmimeConfig{decryptor,key,cert,openssl_bin} ↔ from_config | match | — |
| T2 fixture ↔ T3..T10 | Fixture{ids,smime,mail_root} | T2 Ids list lacks a "200 days old unique word" message that T7 needs | Ruling: T2 adds `ids.old_unique` (dated 200 days ago, body word "zymurgy"); cost if wrong: one fixture field |
| T2 store ↔ T3 envelope | Store{envelope_index} | match | — |
| T3 envelope ↔ T4 paths | parse_mailbox_url → UrlParts | match | — |
| T3 Address ↔ T5 mime Headers | envelope::Address reused | match | — |
| T4 emlx ↔ T5 mime | Emlx.raw → mime::parse | match | — |
| T5 mime ↔ T6 smime | is_enveloped/enveloped_der/is_signed/signed_first_part | match | — |
| T7 read ↔ T8 attach | T7 declares attachment_text returning attach::Extracted before T8 exists | conflict | Ruling: T7 does not implement attachment_text; T8 adds it fully. cost: none |
| T7 SearchHit flatten of MessageRow "minus" fields | ambiguous | Ruling: define `pub struct HitRow` view with the kept fields; cost: naming only |
| T9 tools ↔ T7 read | Context/open/get_email/search | match | — |
| T10 doctor ↔ T3 newest_encrypted_ids, T4 Locator | match | — |
| T11 readonly grep "imap://" | fixture URLs contain imap:// in tests only; src/envelope.rs parses the scheme string "imap" | Ruling: grep list excludes the literal "imap://" (parse needs it); keep osascript/smtp/write-flag checks; cost: none |
| each task self-consistency | tests vs code | consistent | — |

## Progress
Task 1: minor (deferred): UnknownKey detected by toml error-message prefix (config.rs:114-120); nested unknown key untested; load() home-dir fallback.
Task 1: complete (commits 2c3c1bc..9dd439d, review clean)
Task 2: Ruling: exclude_accounts globs are matched against each of: user, host, user@host, account uuid (any match excludes); AccountInfo.identifier = user when it contains '@', else user@host, else uuid — because fixture/real users are full addresses and mailbox://uuid URLs carry no address; cost if wrong: over-broad exclusion, visible in doctor.
Task 2: note for Task 11: Options.messages = extra filler count (default 0); pass 2000-42 for the budget fixture.
Task 2: minor (deferred): no test proves immutable=1 is never used (would need rows left only in the WAL); readonly grep in Task 11 covers the string.
Task 2: fix round 1/5 (1 addressed pending re-review — false uniqueness claim for cc_target; commit 0450332..68d9947)
Task 2: minor (deferred): huge_emlx fixture file has no plist trailer (fixture.rs:371-376); no committed self-check of fixture invariants; store.rs Undetermined and non-SQLite-header branches untested; chmod-000 test fails under root; fixture `base` from wall clock (cross-epoch test may flake across UTC midnight); `V+5` parses as 5; flag bits 2/6/24 never set in fixture.
Task 2: Ruling (plan-mandated note): envelope_mtime stats only the main file, not -wal; Task 4 cache key and doctor freshness may lag until checkpoint — accepted for now; Task 4 may add the -wal mtime to the cache key at no cost. Cost if wrong: stale path cache after a Mail index rebuild until the next checkpoint.
Task 2: complete (commits 9dd439d..68d9947, review clean after round 1)
Task 3: Ruling: excluded-mailbox set is recomputed per query (not once at open) — a mailbox created later in an excluded account must stay invisible; cost if wrong: sub-ms per query.
Task 3: Ruling: after open, run a probe read (SELECT 1 FROM messages LIMIT 1); on failure fall back to the snapshot copy — SQLite opens lazily so -wal/-shm errors surface on first read; cost: one trivial query at startup.
Task 3: Ruling: exclude globs match case-insensitively (lowercase both sides); cost: none.
Task 3: Ruling: Filter gets `from: Option<String>` (substring on sender address/comment, combinable with any scope) and the search tool (Task 9) exposes `from`; spec §8 amended accordingly — the body-scan hint "narrow with from:" needs it; cost: one optional arg.
Task 3: minor (deferred): LIKE case-folding is ASCII-only (MÜLLER vs Müller); tempfile moved to runtime deps (needed for snapshot); mailboxes listed in ROWID order.
Task 3: review approved (68d9947..815a368); fix round 1 dispatched for the four rulings above (not review findings). Accepted extra: EnvelopeError::Pattern.
Task 3: minor (deferred): schema-fallback branches untested except conversation_id; unread filter without `read` column degenerates; sub-second precision dropped in after/before; thread() deleted-anchor inconsistency; Counts.messages includes rows with no mailbox row; some date-ordered tests Unix-only; tests hard-code ROWIDs that have ids names; envelope.rs 892 lines (URL parsing could split).
Task 3: Ruling: snapshot copy skips an unreadable -shm (SQLite rebuilds it from -wal) but fails on an unreadable -wal — stale data must never be served silently; cost if wrong: none known.
Task 3: minor (deferred): exclusion check and main query are two reads, not one transaction (microsecond window for a mailbox created in between).
Task 3: fix round 1/5 (4 addressed, 0 open; commits 815a368..a99f505)
Task 3: complete (commits 68d9947..a99f505, review clean)
Task 4: Ruling: a path-cache hit whose file no longer exists drops that mailbox's cache entry and re-walks once (one stat per get_email) — a moved file must not surface as NotFound while the index still lists it; cost: one stat.
Task 4: Ruling: dash repair only for a last line `--token-` before the plist (no boundary= header check) — accepted; cost if wrong: a parse hiccup on a boundary token ending in '-' (rare).
Task 4: minor (deferred): user@host URL candidates include same-named mailboxes of other accounts (first sorted hit wins; ROWIDs unique); config.rs fails cargo fmt --check (pre-existing).
Task 4: review needs fixes (a99f505..19b56c1): cache hit returns gone path (ruled); fix round 1 dispatched.
Task 4: minor (deferred): read_dir denial inside .mbox/Data reported as Ok(None) (store_dirs, file_type Err → continue); walk picks first file per ROWID in DFS order (.partial may beat .emlx in a wrong shard); every Mail write invalidates walk caches via -wal mtime (cost shows in doctor formula/walk counts); fixture has no non-ASCII mailbox name (NFC/NFD untested).
Task 4: fix round 1/5 (2 addressed, 0 open; commits 19b56c1..981a623)
Task 4: complete (commits a99f505..981a623, review clean)
Task 5: Ruling: `smime.p7m` filename implies enveloped only when no smime-type parameter is present (RFC 5751 names signed-data smime.p7m too); x-pkcs7-* aliases accepted; text parts with filename/attachment disposition are attachments; nameless attachments named part-<n>; states serialize snake_case. Cost if wrong: none known.
Task 5: Ruling: body(want_html) truncates html at max_chars too; attachment_bytes caps external reads at MAX_EMLX_BYTES (64 MiB) → MimeError::TooLarge. (fix round)
Task 5: Ruling: Task 6 may add `mime::Parsed::signed_der()` (DER of a top-level pkcs7-mime signed-data body); Task 7 hides the outer smime.p7m part from attachments when decryption fails.
Task 5: review needs fixes (981a623..1c4f4d4): partial entity table; enveloped detection beyond root. Ruling: enveloped detection root-only. Fix round 1 dispatched with the two ruled items (html truncation, 64 MiB external cap).
Task 5: minor (deferred): strip_html runs words together at </td> and opening block tags; '>' inside quoted attribute ends a tag; source newlines kept as line breaks; <title> text leaks; decode_entities quadratic on '&' without ';'; control-char entities decode; filename scan may claim another part's file with duplicate names; find_external errors silently become NotDownloaded/NotExtractable; Unavailable Display uses Debug casing; no absolute-path filename test; unnamed text/calendar parts invisible; mime.rs 532 lines (html part could split).
Task 5: fix round 1/5 (4 addressed, 0 open; commits 1c4f4d4..dc1b33c)
Task 5: minor (deferred): entity() returns first char only (two-codepoint HTML5 entities lose the second); invalid numeric entities → U+FFFD; metadata/read race on external files.
Task 5: complete (commits 981a623..dc1b33c, review clean)
Task 6: Ruling: OpensslCli::decode_signed adds `-nosigs` (phase 1 does not verify signatures; content must come back even when the digest check would fail) — fix round. Cost if wrong: none (signature status is reported "not verified" anyway).
Task 6: minor (deferred): NotEnveloped also covers "enveloped but body not DER"; opaque-signed check uses signed_der().is_some(); config.rs rustfmt (pre-existing).
Task 6: review approved (dc1b33c..4702c64); fix round 1 dispatched for rulings (-nosigs; manual Debug on Unwrapped hiding decrypted bytes).
Task 6: minor (deferred): openssl() test helper splits on whitespace (TMPDIR with spaces breaks it); try_wait error path kills without wait and reports Spawn; reader joins unbounded after normal exit; auth-enveloped policy keyed on name string; sleep-5 test script without exec leaves an orphan; no stderr-flood test; DER check line duplicated in mime.rs; opaque signed-data without DER/smime-type falls through as unsigned.
Task 6: fix round 1/5 (2 addressed, 0 open; commits 4702c64..d9bff55)
Task 6: complete (commits dc1b33c..d9bff55, review clean)
Task 7: Ruling: decrypted messages list attachments from the inner entity via attachments(None) (inline bytes only; Mail cannot externalise parts inside an envelope); Task 8 attachment_text follows the same path for encrypted ids. Cost if wrong: an externalised inner part would read as NotDownloaded.
Task 7: Ruling: opaque signed-only (pkcs7-mime signed-data, unencrypted) stays undecoded in phase 1 (signed: true, empty body) — deferred; cost: rare mail shape unreadable until phase 2.
Task 7: Ruling: Task 9 adds a minimal stderr logger honouring IMPORTIN_LOG; read.rs's private debug() is replaced by it then.
Task 7: minor (deferred): limit == 0 reports truncated; config.rs rustfmt (pre-existing).
Task 7: review approved (d9bff55..1d163fe). ⚠️ resolved by grep: MimeError/EmlxError/PathError Display carry no message bytes; AuthEnvelopedUnsupported text matches; env.search orders date_received DESC.
Task 7: Ruling: body-scan `truncated` requires a candidate left unscanned (stricter than ruled) — accepted as more accurate; cost: none.
Task 7: minor (deferred): inner parse failure after decrypt fails the whole read; p7m filter case-sensitive; skipped not Option; duplicated load code; skips_encrypted test could pass vacuously; weak field-source tests; empty-terms body scan reads all candidates (decide at MCP layer in Task 9: require non-empty query for scope body); body_scan_days unbounded (chrono overflow for absurd values) — cap in config at 36500 (Task 9/11).
Task 7: complete (commits d9bff55..1d163fe, review clean)
Task 8: Ruling: accepted departures — ReadError::Decrypt for attachment_text on an undecryptable message; index fallback after filename miss; pptx paragraph newlines; no global panic hook (catch_unwind around every extractor); extract reports Inline and the caller overwrites state. Cost if wrong: none known.
Task 8: minor (deferred): zip/xlsx inflated-size not bounded; pdf_extract has no time limit; catch_unwind relies on panic=unwind in the release profile (do not set panic=abort).
Task 8: review (1d163fe..32c1a74): two Important — duplicated loader, enveloped success path untested; fix round 1 dispatched.
Task 8: minor (deferred): slide ordering untested (fix round adds it); encrypted test skips silently without openssl; unbounded zip entry decompression; unknown XML entities dropped silently.
Task 8: fix round 1/5 (2 addressed pending re-review; commits 32c1a74..63a37ef). Ruling: two helpers (load + unwrap_outer) accepted instead of one Loaded struct (avoids double parse of borrowed bytes). minor (deferred): search's body_text still has its own locate/read/parse copy.
Task 9: rulings to carry: search tool exposes `from`; scope body with an empty query → tool error "scope body needs a query"; minimal stderr logger honouring IMPORTIN_LOG (warn default) replaces read.rs debug(); config caps body_scan_days at 36500 (Invalid above).
Task 8: complete (commits 1d163fe..63a37ef, review clean after round 1)
Task 9: Ruling: `YYYY-MM-DD` in after/before = local midnight (user's timezone), not UTC — fix round; cost if wrong: a few hours' window shift.
Task 9: Ruling: get_thread on unknown/excluded id → error; store that will not open → stderr message naming Full Disk Access and exit 2 before serving (loud failure); unknown tool args ignored (rmcp default). Context.env is Mutex<Envelope> (rusqlite Connection is !Sync).
Task 9: note: first process launch per test binary costs ~0.7 s before main (macOS), server ready ~9 ms after main; Task 11 budget check must warm up once.
Task 9: minor (deferred): three direct eprintln! in envelope.rs bypass the log format.
Task 9: review approved (63a37ef..901a2aa). Fix round 1 dispatched with rulings: local-midnight dates; panic payload never logged (hook prints location only); inverted window → error; get_thread returns newest `limit` oldest-first with `truncated`; stdout fd guard (dup fd1 for transport, fd1 → fd2) — cost if wrong: libc dependency and a small startup step.
Task 9: minor (deferred): scope description hard-codes "90 days"; unread:false/flagged:false behave as absent; protocol tests do not exercise mailbox/unread/flagged/offset; instructions checked by prefix only; envelope.rs raw eprintln!.
Task 9: Ruling (deferred to final fix wave): body scan with `before` given and `after` absent defaults `after` = before − body_scan_days (today: silently empty when before < now − 90 d).
Task 9: fix round 1/5 (5 addressed, 0 open; commits 901a2aa..c0c73e1)
Task 9: minor (deferred, final fix wave): tools::date gap fallback panics on NaiveDate::MIN in positive-offset zones (use checked arithmetic → parse error); DESIGN §8 get_thread text needs newest-limit + truncated; IMPORTIN_TEST_STRAY_PRINT hook ships in release (harmless).
Task 9: complete (commits 63a37ef..c0c73e1, review clean)
Task 10: Ruling (fix round): doctor's decrypt probe uses a 60 s timeout (keychain ACL dialog needs a click); the get_email timing uses the newest non-encrypted message so the keychain is touched once. Accepted extras: healthy(), scrub(), DecryptProbe.method, Options::default, ~ substitution, smime::run pub(crate).
Task 10: minor (deferred): mail root without V<n> reports FDA "undetermined"; smime_identities None on non-zero exit too; doctor tests call `security find-identity` on the real keychain (read-only, count only).
Task 10: review needs fixes (c0c73e1..15083f5): --anon scrub not airtight for free text. Fix round 1 dispatched with the two rulings (60 s probe timeout + shared security args; unencrypted timing sample).
Task 10: minor (deferred): doctor tests spawn `security find-identity` (not hermetic); missing tests for failing probe / FAIL rows / security branch; scrub over-redacts "ProtonMail/".
Task 10: minor (deferred): a certificate name on the first stderr line of the security probe would still show under --anon (status code suffices; consider dropping stderr entirely under anon in the final wave).
Task 10: fix round 1/5 (3 addressed, 0 open; commits 15083f5..5f6ba63)
Task 10: minor (deferred): scrub word-start set lacks < [ { ; and relative ../ paths; \r not a line end; cert CN on first stderr line survives; anon_probe_stderr test vacuous without openssl.
Task 10: complete (commits c0c73e1..5f6ba63, review clean)
Task 11: budgets measured (release, 2000 msgs, warm): initialize 13.9–14.5 ms, header search 5.4–6.1 ms, get_email 0.8–1.3 ms.
Task 11: minor (deferred, final fix wave): EnvelopeError::Schema.dump never printed to stderr (DESIGN §4); Cargo.toml lacks license/description/repository metadata; DESIGN drift listed in task-11-report (§2 grep list, §7 OpensslPem naming, §8 html raw, max_chars bounds).
Task 12 (step 1, real store, anonymised): FDA ok; read-only mode; schema complete; epoch unix; 120378 msgs / 58 mailboxes / 4 accounts; url_form mailbox-uuid; path sample 20/20 by formula; flag_check 18 agree / 2 disagree; smime_identities 1; decrypt probe timed out at 60 s (keychain ACL dialog needs the user's click); timings open 3 ms, search 2 ms, get_email 0 ms.
Task 12: Ruling: no exclusion is configured yet and uuid-form URLs carry no address; Accounts4.sqlite (~/Library/Accounts) maps uuid → type/username/description read-only — final fix wave adds account label resolution from it (identifier = username when present, label = description; exclusion globs also match username/description/type; fallback uuid). The classifier blocked reading the labels here, so the user runs `importin doctor` and sets exclude_accounts; opencode is NOT switched to importin before that (privacy regression otherwise). Cost if wrong: one more read-only sqlite open at startup.
Task 12: note: flag bit 3 disagrees with content type on 2/20 → hint only; final wave adds direction counts to doctor.
Task 11: review needs fixes (5f6ba63..d7431c8): README lacks release-zip install route. Fix round 1 dispatched (with doc wording minors).
Task 11: minor (deferred): readonly test window heuristic limits (doc them); budgets_release should assert release build; release zip lacks MIT dependency notices (cargo about before first tag); softprops two-job release race to watch on first tag.
Task 11: fix round 1/5 (1 addressed, 0 open; commits d7431c8..f7777a8)
Task 11: complete (commits 5f6ba63..f7777a8, review clean)
Final fix wave: dispatched from final-wave-brief.md at f7777a8 (items A1-4, B5-14).
Final fix wave: complete (commits f7777a8..3db23f3). Ruling: IMPORTIN_ACCOUNTS_DB read in config::parse into Config::accounts_db (accepted). minor (deferred): schema dump also prints under doctor --anon; doctor does not show whether labels loaded; `account` filter matches only the identifier.
Final review (fable, 2c3c1bc..3db23f3): no Critical; Important 1-6 (IMPORTIN_CONFIG missing file silently defaults; empty text/plain hides HTML; doctor times only scope subject; zip inflation unbounded; security decode_signed verifies signatures (no -nosigs) → fallback to openssl -nosigs; opaque signed-only undecoded). One fix dispatch at 3db23f3.
Final review triage: promoted Task 8 zip-bound minor; all other deferred minors may stay (listed in HANDOFF).
