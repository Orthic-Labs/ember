# Localization parity

> **Last reviewed:** 2026-10-16 · **Last updated:** 2026-10-16 · **Change:** minor (#185: partial catalogs completed, machine-drafted) · **Target:** reference catalog editor 15.5 / reference editor 9.6

How far each interface language is from `full`. How to add or maintain a language is in
[localization.md](localization.md); per-language notes are in `localization-<code>.md`
([zh-hans](localization-zh-hans.md), [zh-hant](localization-zh-hant.md), [ja](localization-ja.md),
[pt-br](localization-pt-br.md), [es](localization-es.md), [de](localization-de.md),
[ru](localization-ru.md)).

## The target

reference editor 9.6 (installed on the owner's Mac) ships 16 interface languages, listed from its bundle's
`.lproj` folder names: English, German, Spanish, French, Italian, Japanese, Korean, Norwegian Bokmål,
Dutch, Polish, Portuguese (Brazil), Russian, Swedish, Thai, Simplified Chinese and Traditional
Chinese. It does **not** ship Hindi, Arabic, Indonesian or Vietnamese, so for those Ember would
go beyond the target.

## How the numbers are measured

**Measured** (a Python count over `crates/ui-egui/locales/*.json`, 2026-10-10):

- The denominator is every message any catalog knows: the union of the plain catalogs' keys (2,074
  English source strings: menus, commands, controls, panels, dialogs, tooltips, presets, What's New
  lines) plus the 230 messages of the formats catalogs (`*-formats.json`: text with runtime values and
  date patterns), 2,304 messages in all.
- A message counts as translated when the catalog has a non-empty value that differs from the English
  key. Some identical values are correct (`ISO`, `OK` in German), so the figure slightly understates
  coverage; on the other hand, strings that never reach a catalog (technical errors raised in lower
  crates, some `format!` text outside `tr_format!`) are not in the denominator at all. That is why no
  language is `full` even at 99%.
- The UI has 563 distinct `tr("…")` literals and 198 `tr_format!` messages in `crates/ui-egui` and
  `apps/`; command, control, menu and preset labels reach the catalogs through `display_label`.

Script support is checked against the code: `language_table!` in `crates/ui-egui/src/i18n.rs` lists
scripts `Latn`, `Cyrl`, `Hans`, `Hant`, `Jpan` only. The UI is egui, which has no bidirectional layout
and no complex-script shaping, so Arabic (RTL, joining) and Devanagari (conjuncts, reordering) cannot
render correctly today.

Native-speaker review: no catalog has had a formal review. Several catalogs have edits from community
contributors whose commits only touch that language (ja, zh-hans, zh-hant, fr, de, ru, uk, pt-br);
that is promising but is not recorded as review, so the column says `no (community edits)`.

## The twelve key languages

| # | Language | Code | UI strings translated | Dialogs / tooltips / help | Script support | Native review | Status | To `full` |
|---|---|---|---:|---|---|---|---|---:|
| 1 | English | `en` | 2,304 / 2,304 (100%, source) | all; Help ▸ What's New and the shortcut sheet; no full user manual | Latin (Inter) | n/a | **full** | 0 h |
| 2 | Simplified Chinese | `zh-hans` | 2,163 keys present (all shared keys; machine-drafted, see #185 note) | menus, panels, dialogs, presets, profiles, dates; lower-layer errors English | Hans via craft-fonts (Noto Sans CJK SC); IME through winit; no vertical text in UI (not in target either) | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 6–10 h |
| 3 | Spanish | `es` | 2,161 keys present (machine-drafted, see #185 note) | as zh-hans | Latin | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 6–10 h |
| 4 | Hindi | `hi` | 0 (0%) | none | **Devanagari shaping missing** (egui has no complex shaping) | no | none (target: not shipped) | 50–90 h (shaping 25–50 h, shared with Arabic) |
| 5 | Arabic | `ar` | 0 (0%) | none | **RTL layout and Arabic shaping missing** | no | none (target: not shipped) | 60–110 h (RTL mirroring 30–50 h + shaping, shared with Hindi) |
| 6 | French | `fr` | 2,161 keys present (machine-drafted, see #185 note) | as zh-hans | Latin | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 5–9 h |
| 7 | Portuguese (Brazil) | `pt-br` | 2,161 keys present (machine-drafted, see #185 note) | as zh-hans | Latin | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 6–10 h |
| 8 | Indonesian | `id` | 2,162 keys present (machine-drafted, see #185 note) | as zh-hans | Latin (Inter covers it) | no | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 10–16 h |
| 9 | Japanese | `ja` | 2,164 keys present (machine-drafted, see #185 note) | as zh-hans; vertical export watermarks | Jpan via craft-fonts (BIZ UDPGothic / UDMincho); IME through winit | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 6–10 h |
| 10 | German | `de` | 2,162 keys present (machine-drafted for the 40 keys added in #185) | as zh-hans; What's New translated line by line | Latin | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 4–8 h |
| 11 | Korean | `ko` | 2,162 keys present (machine-drafted, see #185 note) | as zh-hans; Hangul | `Kore`. No Hangul face of its own (Inter has none): Hangul falls back to the Noto Sans CJK SC face, allowed for Korean by the 2026-10-16 maintainer decision | no | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)); web rendering unverified | 14–22 h |
| 12 | Vietnamese | `vi` | 2,162 keys present (machine-drafted, see #185 note) | as zh-hans | Latin with stacked diacritics (Inter covers every Vietnamese letter, checked against its cmap) | no | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 10–16 h |

## Other languages Ember ships (3)

| Language | Code | UI strings translated | Script | Native review | Status | To `full` |
|---|---|---:|---|---|---|---:|
| Traditional Chinese (Taiwan) | `zh-hant` | 2,161 keys present (machine-drafted, see #185 note) | Hant; uses the Simplified Chinese face until craft-fonts has a TC face | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 8–14 h (incl. the font) |
| Russian | `ru` | 2,162 keys present (machine-drafted for the 8 keys added in #185) | Cyrillic (Inter) | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) | 8–12 h |
| Ukrainian | `uk` | 2,162 keys present (machine-drafted for the 11 keys added in #185) | Cyrillic (Inter) | no (community edits) | complete, machine-drafted (native review pending, [#233](https://github.com/Orthic-Labs/ember/issues/233)) (lower-layer errors English) | 2–4 h |

Target languages Ember lacks: Italian, Dutch, Polish, Swedish, Norwegian Bokmål, Thai
(Thai needs shaping and line breaking without spaces). Each Latin-script one is ~10–16 h; Thai
~20–35 h on top of the shaping work.

## Shared work before any language is `full` (counted once, ~20–35 h)

- Route lower-layer error messages (catalog, raw, codecs, engine) through translatable message ids
  instead of English `format!` strings (~12–20 h).
- A check in `cargo xtask ci` that every user-visible literal reaches a catalog (~4–8 h).
- A user manual / help pages beyond What's New and the shortcut sheet (reference editor links to online
  help; ~4–8 h for a translatable in-app help index).
- Native-speaker review passes (human; cannot be done by agents).

## Totals

- Bring the 9 shipped non-English catalogs to `full`: ~55–90 h (incl. the shared work).
- Add the 5 missing key languages: ~145–255 h, most of it RTL and complex shaping in egui.
- Reach the target's 16 languages: + ~60–100 h (Italian, Dutch, Polish, Swedish, Norwegian, Thai).

## Catalog completion batch (#185)

Every shipped partial catalog now carries every key of the shared English key set (the union of the plain
catalogs, 2,164 keys, of which the two `smart album value|Yes/No` contextual keys are Japanese-only). The
keys added in #185 were **machine-drafted** by an agent, not translated by a native speaker, and they need
native review before any language is called `full`. Each language's commit is `i18n: complete <language>
catalog (#185)`. Added per language: German 40, Russian 8, Ukrainian 11, Simplified Chinese 162, Traditional
Chinese 198, Japanese 199, Spanish 162, French 204, Brazilian Portuguese 160.

- The catalog format has no fuzzy or needs-review flag, so this note is the only record of the draft status.
- Contextual `smart album value|…` keys were added only to Japanese, which uses them; other languages keep
  their plain Yes / No.
- What's New lines were translated as wrapped fragments without checking the surrounding lines of
  `docs/whats-new.md` in every case; the seams between fragments are the first thing a reviewer should check.
- Face recognition terms (Face, Face Model, Detect Faces…) are new in these catalogs; their wording needs
  review together.
- The catalogs contain English-looking values that equal their key (see the per-language notes); those are
  not part of this batch and were not changed.
- Indonesian (`id`) was added once the partial catalogs were complete, as the #185 decision orders: a machine draft of the full key set, native review pending. Vietnamese (`vi`) was added second, with the same machine-draft status and the same plural-suffix gap. Korean (`ko`) was added last, with the same machine-draft status. Noto CJK for Hangul is allowed (maintainer decision, 2026-10-16); glyph coverage on the web is unverified (see docs/web.md). #185 stays open for native review. Plural slots (`photo{}`) take their suffix from the locale (54c867f5), so the no-inflection languages get none. Hindi and Arabic stay deferred until shaping and RTL exist.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | major | Created: per-language counts measured from the catalogs, the twelve key languages, script support, estimates |
| 2026-10-16 | minor | #185: the nine partial catalogs completed with machine-drafted keys (needs native review); counts updated |
| 2026-10-16 | minor | #185: Korean catalog added (machine-drafted, native review pending) |
| 2026-10-16 | minor | #185: Vietnamese catalog added (machine-drafted, native review pending) |
| 2026-10-16 | minor | #185: Indonesian catalog added (machine-drafted, native review pending) |
