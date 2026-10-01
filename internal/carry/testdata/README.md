# Real archived journals

These three journals come from one 16-core Zen 5 desktop: AMD Ryzen 9 9950X3D2, ASRock X870E Taichi, BIOS 4.43, microcode `0xb404038`, boost limit 5650 MHz. All three recorded the same BIOS context.

| Journal | Schema | Ruleset | JSON records | Uncompressed SHA-256 |
|---|---|---|---|---|
| `20260924T204352Z.jsonl.gz` | 1 | 1 (implicit) | 11589 | `63a97557b2a3dca1f4f865952008c82f551dea6b4cce021c29e024ec17bb2244` |
| `20260926T151414Z.jsonl.gz` | 2 | 2 | 8124 | `71ef967da7515179e171f26e3e364cb631b69b210c4089d2cff43c281f9ae188` |
| `20260927T221954Z.jsonl.gz` | 2 | 3 | 7083 | `e4392953e7b8cae8d49bbd20691acae08fdae18d1f0a6ade524e465452d0b4b7` |

Copied from the three matching archived `.jsonl` files, with case-insensitive normalization of the former application name to `togi`, and gzip-compressed with a zero modification timestamp. The hashes above describe the normalized, decompressed bytes, not the original archive bytes. Only branding strings changed, including messages, scope names and configuration paths; all decision and event evidence, schema and ruleset values, boot IDs and event times are retained. The journals include public Nix store paths and the normalized `/etc/togi/config.toml` configuration path.

Before committing, a scripted scan of every decompressed record checked hostname, user-name and serial-number fields and labels; the source machine's local hostnames and user names; DNS hostnames; home paths; email addresses and `@`; IPv4 and IPv6 addresses; and MAC addresses. All three journals had zero matches. The pull request demo includes the scan script and captured result. This pattern scan is not a guarantee that arbitrary unlabelled identifiers would be detected.

`TestPrepareRealArchiveChain` walks the newest archive back through rulesets 2 and 1, pinning the BIOS context, all three sources, 16 candidate edges and 13 failed marks, including each value's session, sequence and signal. Per #113, remove this historical-format regression at 1.0 together with the older-format readers tracked by #153. The later statistics program and simulator comparison are outside this fixture pull request.
