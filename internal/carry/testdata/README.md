# Real archived journals

These three journals come from one 16-core Zen 5 desktop: AMD Ryzen 9 9950X3D2, ASRock X870E Taichi, BIOS 4.43, microcode `0xb404038`, boost limit 5650 MHz. All three recorded the same BIOS context.

| Journal | Schema | Ruleset | JSON records | Uncompressed SHA-256 |
|---|---|---|---|---|
| `20260924T204352Z.jsonl.gz` | 1 | 1 (implicit) | 11589 | `80ba0e2292f5a6cd4bc69e0feecee431dea5932bbacfd7d41207f6ed2c28b785` |
| `20260926T151414Z.jsonl.gz` | 2 | 2 | 8124 | `edbd98ab6fba3eec1679e3b5b78e31352176ce503c4f0b51d455b47356979061` |
| `20260927T221954Z.jsonl.gz` | 2 | 3 | 7083 | `28b085acb3f2bd2b2e662f6cd9726a6631e321738f813da94166fb3e39c68ade` |

Copied from the three matching archived `.jsonl` files and gzip-compressed with a zero modification timestamp. Decompression reproduces the original bytes; nothing was redacted. Boot IDs and event times are retained because they affect replay and crash detection. The journals include public Nix store paths and the former `/etc/shycler/config.toml` configuration path.

Before committing, a scripted scan of every decompressed record checked hostname, user-name and serial-number fields and labels; the source machine's local hostnames and user names; DNS hostnames; home paths; email addresses and `@`; IPv4 and IPv6 addresses; and MAC addresses. All three journals had zero matches. The pull request demo includes the scan script and captured result. This pattern scan is not a guarantee that arbitrary unlabelled identifiers would be detected.

`TestPrepareRealArchiveChain` walks the newest archive back through rulesets 2 and 1, pinning the BIOS context, all three sources, 16 candidate edges and 13 failed marks, including each value's session, sequence and signal. Per #113, remove this historical-format regression at 1.0 together with the older-format readers tracked by #153. The later statistics program and simulator comparison are outside this fixture pull request.
