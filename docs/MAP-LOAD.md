# Map load: what lives where

Three floors: **disk** (`asset_transport`), **format** (`fastfile_*` → the
`asset_iw4` IR), **products** (`asset_*` + `assets`). `assets` uses transport for disk access and its source lanes to compile
format captures into products; `fastfile_*` knows the format and opens no files. Installing the match is `session`, not here.

```text
console/ui `map` → session::lifecycle   SessionSwapRequest, tear the previous
                     occupancy down, then MapLoadApproved{request_id}
  → assets::match_load    start_match_load onto load_pool() (MatchLoadBusy),
                          poll_match_load → PreparedMatchReady
  → session::match_apply  apply_prepared_match → scene + SimWorld + MatchInstalled
```

Cancellation is `MatchLoadAbort(request_id)`: the walk returns
`MatchLoadOutcome::Canceled` at its next checkpoint, throwing nothing away.
Failure is `MapLoadFailed`, from discovery *and* install; a request ends once.
Startup and later load failures return to the main menu with a map error dialog;
OK or Escape dismisses it. The log keeps the full error.

## The walk: `assets::session_load::load_prepared_match`

The single entry point; the composition root sees no `fastfile_*` and no
`AssetSink`. Inside are parallel walks on `load_pool()`: the archive, startup
materials, `common_mp`, the IW5/T5/T6 weapon bundles, language zones. `common_mp`
goes **before** the map — its techset tables are needed before
`build_world_draw`. Output is `MatchLoadOutcome::Ready` with a `PreparedMatch`
(world, one material population, clip, weapons, catalogs and `PreparedMap`).

A walk enqueues its own image plan the moment it is ready, and the catalog keeps
one row per image name. Two plans resolving the *same* archive entry into the
same payload share one decode; a name three games spell alike but fill
differently is an override, not a duplicate, and `load_jobs.csv`
([`BENCH.md`](BENCH.md)) counts the two apart. The per-game adapter is
`assets::lane` (`ZoneGame` → iw4/iw5/t5/t6); a lane gap is a typed `LaneGap`, never
silence.

`ZoneLane::load_common_mp` returns owned common builders and diagnostics. A lane whose preparation needs already assembled donors can retain a private, consumed-once `CommonFamilyCompiler` inside that result. The shared loader supplies owned weapon, material, FPV, world-model, projectile, animation and effect builders; compilation returns them together with dependency refusals. This stage finishes before the common set is published or cloned for a match. It is not invoked by frame execution.

T6 model/material/effect captures stay private to the T6 lane. Its compiler owns stand-in donor lookup, native technique linking, named texture adaptations, animation admission, weapon dressing and projectile binding. Missing model/effect donors return typed refusals, while accepted contributions and existing builders remain available; source conversion diagnostics remain in the load report. Other lanes finish these products within their existing common walk. All lanes publish through the same common-set and match installation path.

`ZoneLane::load_world` borrows the common `FilmVisionCatalog` immutably. The map's vision overrides a common vision, including a map parse error; a failed map parse does not silently fall back. Selection and the merged vision catalog belong to `LoadedWorld` output. Loading a map does not consume a caller's common vision entries.

These are CPU compilation contracts. They do not establish shader-port admission, decoded-media readiness, GPU residency or playable readiness. The existing common cache owns compiled builders under its path-based `CommonKey`; a retained key does not detect edits to source files in place. Such content needs a rebuilt common set. No per-family decoder payload escapes the deferred compiler, and changing source content must not reuse its captured payload with unrelated donor builders.

Installing it is `session::match_apply`. Preflight builds a `MatchInstallPlan` — mode, doors, objectives, scene conversion, drawable world —
and publishes nothing until it hands one over. Commit publishes it, boots the
sim and writes `MatchInstalled`. The authority then prepares bot navigation on
`load_pool()` from a world snapshot; it reports `BotNavigationReady` with the
world generation. Session owns readiness policy: dedicated advancement needs
navigation; listen advancement also needs rendering unless headless. Graphical
presentation and admission require generation-matched rendering and audio;
headless policy bypasses those presentation services.
Explicit silent audio satisfies that policy; missing or failed audio does not.
Old completion reports cannot release a newer world, and renderer systems do
not write the authority hold. Session also owns the local input predicate.
The walk graph is cached (`nav`), keyed by the content digest with the bake's
schema and hull, so the second start of a map reads it back instead of walking
the grid again; a match teardown drops the in-memory copy, not the file.

`MatchLoadOutcome::Ready` is the CPU package, not render-ready. The loading
screen also holds for GPU images, every queued pipeline (the HUD blood film is
queued as soon as its material exists) and the `first_person` stage: every
weapon's first-person materials admitted and compositions laid out
(`render_anim::PreparedFpv`), and every body, world-weapon, map-model and
projectile material plus the single-model DObjs (`PreparedModelMaterials`).
Remote kits and dropped-item compositions are built at install.
`load ledger:` in the log is one line per load of
what was handed over or compiled new against what an earlier load left behind.

## Campaign maps

A zone that walks a `ClipMapSp` or `GameWorldSp` (`trainer`, `roadkill`) is a campaign map. It
plays free-roam under the multiplayer rules, `dm` and `war` only: its world, props and entities,
none of its scripts. `MapFacts::campaign` carries the fact from the lane to the walk.

* **Entities.** `asset_world::sp_entities_for_iw4_rules` rewrites the string in the lane, and the
  scripts, the spawn list and the intermission camera all read the rewrite. `info_player_start`
  becomes `mp_dm_spawn`; up to 23 pathnodes nearest it, at least 192 units apart, join it as dm
  and tdm spawns (a ring when a map has none), with `_start` points, an `mp_global_intermission`
  and the `care_package` that `_airdrop` wants. Actors, vehicles and animated models are renamed, never
  removed: a script model's identity is its ordinal in the string.
* **Scripts.** The match walk drops the zone's `.gsc` for `maps/mp/<zone>` (the loader, the two
  sides, the zone's destructible animation tables) and `iw4l_maps/campaign`, which starts before
  the gametype and sets `level.isSP = false`; `isSP()` otherwise answers from the `mp_` prefix.
* **Soldiers.** With no `mp_body_*` in the zone, `BodyMeshCatalog::kits` takes the campaign's
  `body_*` and `head_*` (Rangers against OpFor where the zone has both).
* **Weapons.** The zone's FPV models are left out, hands included, and a clip that shares a name
  with common_mp's loses to common_mp's (`XAnimBuild::absorb_missing`): the multiplayer weapons
  are built on common_mp's, and the campaign's `viewmodel_m4` is another model.

Not there yet: the film vision when the zone carries none (`roadkill` does; `trainer`'s lives in
the campaign `common.ff`, which no walk opens), ambient effects and sound, and the static models a zone does not carry
(probably in `common.ff`: `trainer` leaves 1,421 of 6,772 slots unresolved, `roadkill` 59 of
17,422).

### Asking for one: `sp:<stem>`

A campaign zone is `<stem>.ff` of the IW4 game, and four of the stems (`favela`, `invasion`,
`estate`, `boneyard`) are stems of multiplayer maps too: a bare stem resolves to `mp_<stem>` first
(`discover::find_zone_stem`), so `map favela` is the multiplayer map. The key `sp:<stem>`
(`discover::parse_zone_key`) is the zone `<stem>.ff` and never `mp_<stem>`. Only the campaign's own
stems are in the namespace (`asset_transport::campaign::CAMPAIGN_MAPS`, with the mission titles);
`sp:common` is refused. The zone that comes of it is still named by its stem: `mapname`, the
script module `maps/mp/<zone>` and the ambient lookups all see `favela`. `LaunchIdentity::map_key`
keeps the key it was asked for by, which is what `map_restart` asks for again.

`discover::list_map_packs` is the multiplayer packs followed by the pack "Campaign (free roam)",
the installed campaign zones in mission order (`list_mp_map_packs` and `list_mp_maps` stay
multiplayer: the approved scenarios and the console choose from those). The lobby lists them by
mission (`ui::frontend::maps::map_label`), has no preview image for them (an empty material, no
name for an image that is in no archive) and starts them in Free for All or Team Deathmatch only: the
mode list shows those two, picking a campaign map in another mode sets Free for All and says so, and
`ui_start_match` refuses the rest in words. `preflight_match_install` refuses them too for the
`map` path, with no level abort. A campaign map is for a private match: `ui_lobby_privacy`,
`ui_select_map` on a public lobby and the lobby joins refuse it, and
`net::content_required_by_map("sp:…")` names no content for the master. Its loading screen is
titled by the mission and keeps the solid colour (`loadscreen_<stem>` is in no archive).

Not covered: a demo or clip recorded on a campaign map names the bare stem, so `favela`,
`invasion`, `estate` and `boneyard` replay on the multiplayer map of that name.

## Cache, and poking it: `iw4l-artifacts/cache/<kind>/<prefix>/<key>`

Content-addressed leaf in `asset_transport::artifact_cache` (`cache_get` /
`cache_put`, `fnv1a64`). A miss is silent — the caller computes the value anyway
— and a hit must be the **same bytes** a miss would have written. The key names
every input; if the encoder changed, bump the format word. Live kinds: `mips`,
`wgsl`, `localize`, `nav` and `xwma_pcm`, whose miss is decoded by the native Rust T5 WMA2 decoder.
`IW4L_GAMES` holds the game trees; no folder name is hardcoded. Live it is `make
map mp_boneyard` ([`RUN.md`](RUN.md)), with stages in `LoadProgress`.
