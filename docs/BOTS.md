# `bots` — host AI

Authority tick: observation → memory → utility → task → route → motor → UserCmd.

`bot dummy [N]` creates bots with no controller or perception queries. They keep
their aim and weapon, accept `bot tp/give/fire`, and take damage and respawn normally.
They retain normal bot loadouts/perks; `bot hold off` leaves dummies idle.
Bots submit `TickInput.cmds` like players — [`SIM-STEP.md`](SIM-STEP.md).

| file | owns |
|---|---|
| `observation.rs` / `memory.rs` | what a bot may know, and for how long |
| `sensor.rs` | visibility probes and the rotation that spends them |
| `controller.rs` / `task.rs` / `intent.rs` | scores, task choice, stages |
| `nav.rs` | the baked graph, A\*, resumable route requests |
| `query.rs` | the shared collision budget every subsystem draws on |
| `weapon.rs` / `motor.rs` | the weapon channel; aim and movement |

## The three rules that shape the rest

**A negative needs a probe that ran.** Visibility answers seen, evaluated and
not seen, or not evaluated. Only the second ends a contact; the third holds it,
briefly and without coordinates. Firing needs a positive observation of that
client, fresh on the tick the command goes out.

**A denied query is not a collision.** Perception, connectors, execution and
combat share one 96-query quota. A query that ran reports what it found even on
the last token; one that never ran comes back as denial, so no refusal is ever
fabricated from an empty budget.

**Work survives a tick that is granted nothing.** A route request owns its
phase — approach, attachments, search, validation — and every cursor inside it
is resumable, so a zero allocation repeats nothing and A\* keeps its frontier
under the shared 2048-expansion quota. Answers that follow from graph topology
alone are cached per request key until the graph changes; collision-dependent
refusals are rechecked.

## Movement and combat

The graph bakes world-aligned tiles at 48 units, refined to 24 near missing
support or a height change, with directed Walk, Drop and BreakGlass edges that
keep their entry and exit through execution. A fight moves to a *position*: one
baked support per sector around the threat, checked for line of fire and
exposure, then reached through the ordinary route executor — reachability is
never inferred from a component label.

Every map's bake leaves a few dozen nodes with no walk edge (a prop or a clip
brush fails every probe around them), each its own component, and a few tiny
pockets. Roam goals, firing positions and fall-back points come from the
component of the nearest node within snapping range that has at least 16 nodes,
not from the nearest node outright, so a bot that spawns beside one of these is
not sent to the node under its feet for good. A roam goal is at least 64 units
away; with none, the bot steps 64 units onto clear floor instead of holding. A
hunt or a search that holds for 2 s with no route that will move it (its goal
reached, no goal, or a goal the graph cannot reach) counts as stuck and steps
off the same way, and the periodic aggregate counts it. With no clear step in
any direction (midway up a flight of stairs between two baked steps, where no
node is within a step's height to route from either) the bot walks straight
at the nearest node and lets the movement code take the stairs. A route that
could not start (no baked node near enough to walk to) is not asked for again
from the same spot for a second: another goal from there fails the same way,
and each attempt spends shared quota on probes the other bots are waiting for.

Some map volumes kill: MW2's scripts suicide any player touching a
`trigger_hurt`, poison and then kill players in the `radiation` fields and blow
up players in the `minefield` ones. The host reads those triggers' volumes
from the live level (`SimWorld::hazard_pieces`, every 20 s) and marks the nodes
a standing player would touch them at. Marked nodes are never roam goals,
firing positions, fall-back points or escape steps, and a route pays a large
cost for stepping into one, so it goes round unless the detour is very long.
A bot that finds itself inside one anyway walks out to the nearest node
outside before anything else.

The weapon channel reloads the gun in hand or switches to a loaded one; with
nothing loaded anywhere it switches to a gun that still has reserve ammunition
and reloads it there instead of keeping the empty one. Only usable guns are
switched to: selectable, and bullet or a launcher that fires without a lock
(never a Javelin, Stinger or Strela, which need a lock the bots never take, and
never the riot shield). A held weapon that is not one counts as empty. Both
magazines of an akimbo pair count. A launcher is neither raised nor fired with
an enemy inside 300 units (or 1.5 times its blast), where it would kill the bot
too.

The motor taps the trigger of a single-shot weapon (pistols, bolt and lever
actions, the AT4): down for one command, then up, pressed again after a gap of
150 to 250 ms per bot and never sooner than the weapon's fire time, since the
weapon fires once per press. Full-auto and burst weapons hold it. An akimbo
pair gets both buttons: attack fires the left gun, throw the right one.

Some bots fight with their melee. A riot shield's attack is the bash on the
melee button (its fire button only clicks), so a shield bot walks up to the
enemy, straight across clear floor or along its route, and bashes in reach, or
lunges up to 128 units when the weapon can; against an enemy that stays beyond
600 units or out of reach for 3 s it takes out its sidearm. A bot with no gun
it can use at that range (all dry, or only a launcher too close) knifes the
same way instead of backing off, and any bot with an enemy in knife reach
knifes it. Each swing is its own press, and fire and melee never share a
command. A bot backs off to reload or switch only while there is a reload or
switch to make. It falls back to a baked support further from the threat (one
with walk edges, out of the threat's line of fire where one is), tries each
such point once, and with none reachable holds where it is, crouched, rather
than asking the route for the same unreachable point on every think.

A shot whose trace would land on the target's riot shield is not a shot: the
bot tries the head and the feet, then another firing position, as players
flank a shield. A fight is judged by the damage it does. The enemy's health
when seen stands for what a player reads from hit markers, and four seconds
of attacking (firing or swinging, not walking or reloading) without it
falling sends the bot to a firing position at least 96 units from where it
stood. Nine seconds and the bot gives that enemy up for twelve, and goes back
to its objective or its hunt. Against a player who takes no damage (god mode,
a shield it cannot get round) bots attack, move, leave and come back, instead
of standing in front of him for the rest of the match.

Objectives come from the compass markers a human reads. In Capture the Flag
the flag carrier (the escort marker rides on him) takes the enemy flag to his
team's own capture zone, the `waypoint_defend_flag` marker, and walks onto
its marker once the route is spent, since touching the zone with the flag
scores. That goal outranks every capture, and near home searching and
planting too; an enemy in view still comes first. While the team's own flag
is away the zone is hidden and the carrier has no such goal.

`HostController::decision()` exposes scores, stage and objective; the periodic
aggregate adds per-subsystem query counts and stuck bots. Both read live from a
running match, which is where bot behaviour is judged: `make map mp_boneyard`
([`RUN.md`](RUN.md)) and watch the aggregate.
