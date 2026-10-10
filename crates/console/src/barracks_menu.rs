use crate::{CommandSpec, ConsoleCommand, ConsoleRegistry};
use bevy::prelude::*;
use frame::UiMenuDvars;
use ui::barracks::{BarracksProfile, reward_rows};

#[derive(Default)]
pub(crate) struct BarracksMenuState {
    emblems: bool,
    page: usize,
    hover: usize,
    reward_hover: usize,
    rewards: Vec<u32>,
    status: String,
    /// What the card and reward dvars were last written from; `None` until they
    /// are written, and whenever they have to be written again.
    published: Option<BarracksInputs>,
}

/// Everything the card and reward dvars are worked out from besides the menu
/// state above, which only a barracks command changes. While none of it moves
/// and no command comes in, the dvars already hold what this frame would write.
#[derive(Debug, PartialEq)]
struct BarracksInputs {
    selection: sim::PlayerProfile,
    player_name: String,
    catalog: bevy::ecs::change_detection::Tick,
    loc: bevy::ecs::change_detection::Tick,
}

impl BarracksInputs {
    fn matches(
        &self,
        selection: sim::PlayerProfile,
        player_name: &str,
        catalog: bevy::ecs::change_detection::Tick,
        loc: bevy::ecs::change_detection::Tick,
    ) -> bool {
        self.selection == selection
            && self.player_name == player_name
            && self.catalog == catalog
            && self.loc == loc
    }
}

/// A console `set` can write any dvar, ours included, and the frontend route
/// that applies it may run before or after this one: the dvars are written this
/// frame and the next, as they used to be every frame.
pub(crate) fn writes_any_dvar(command: &ConsoleCommand) -> bool {
    matches!(command.name.as_str(), "set" | "seta" | "setfromdvar")
}

pub(crate) fn register(registry: &mut ConsoleRegistry) {
    for name in [
        "ui_barracks_open",
        "ui_barracks_pick",
        "ui_barracks_hover",
        "ui_barracks_page",
        "ui_barracks_reward",
        "ui_barracks_reward_hover",
        "ui_barracks_apply",
        "ui_barracks_reset",
    ] {
        registry.register(CommandSpec::new(name));
    }
}

fn localized(loc: &asset_game::LocalizeCatalog, key: &str) -> String {
    loc.text(key.trim_start_matches('@'))
        .unwrap_or(key)
        .to_owned()
}

pub(crate) fn route(
    mut commands: MessageReader<ConsoleCommand>,
    catalog: Res<asset_game::MenuCatalog>,
    loc: Res<asset_game::LocalizeCatalog>,
    settings: Res<frame::GameSettings>,
    mut profile: ResMut<BarracksProfile>,
    mut state: Local<BarracksMenuState>,
    mut dvars: ResMut<UiMenuDvars>,
) {
    if !profile.loaded {
        commands.read().for_each(drop);
        state.published = None;
        return;
    }
    let commands: Vec<&ConsoleCommand> = commands.read().collect();
    let dvar_writes = commands.iter().any(|command| writes_any_dvar(command));
    let barracks: Vec<&ConsoleCommand> = commands
        .into_iter()
        .filter(|command| command.name.starts_with("ui_barracks_"))
        .collect();
    if barracks.is_empty()
        && !dvar_writes
        && state.published.as_ref().is_some_and(|published| {
            published.matches(
                profile.selection,
                &settings.player_name,
                catalog.last_changed(),
                loc.last_changed(),
            )
        })
    {
        return;
    }
    let rows = reward_rows(&catalog);
    for command in barracks {
        let arg = command.args.first().map(String::as_str).unwrap_or("");
        let index = arg.parse::<usize>().ok();
        let table_name = if state.emblems {
            "mp/cardIconTable.csv"
        } else {
            "mp/cardTitleTable.csv"
        };
        let size = if state.emblems { 48 } else { 27 };
        match command.name.as_str() {
            "ui_barracks_open" => {
                state.status.clear();
                if arg == "killstreaks" {
                    state.rewards = profile.selection.killstreaks.to_vec();
                    state.reward_hover = 0;
                } else if matches!(arg, "titles" | "emblems") {
                    state.emblems = arg == "emblems";
                    let selected = if state.emblems {
                        profile.selection.emblem
                    } else {
                        profile.selection.title
                    } as usize;
                    let size = if state.emblems { 48 } else { 27 };
                    state.page = selected / size;
                    state.hover = selected % size;
                }
            }
            "ui_barracks_page" => {
                let pages = catalog
                    .string_table(table_name)
                    .map_or(1, |table| table.rows.div_ceil(size))
                    .max(1);
                if let Ok(delta) = arg.parse::<isize>() {
                    let offset = delta.rem_euclid(pages as isize) as usize;
                    state.page = (state.page + offset) % pages;
                    state.hover = 0;
                }
            }
            "ui_barracks_hover" => {
                if let Some(index) = index.filter(|&i| i < size) {
                    state.hover = index;
                }
            }
            "ui_barracks_pick" => {
                if let Some(index) = index.filter(|&i| i < size) {
                    let row = (state.page * size + index) as u32;
                    if catalog
                        .string_table(table_name)
                        .is_some_and(|table| row < table.rows as u32)
                    {
                        if state.emblems {
                            profile.selection.emblem = row;
                        } else {
                            profile.selection.title = row;
                        }
                        state.hover = index;
                    }
                }
            }
            "ui_barracks_reward_hover" => {
                if let Some(index) = index.filter(|&i| i < rows.len()) {
                    state.reward_hover = index;
                }
            }
            "ui_barracks_reward" => {
                if let Some(index) = index.filter(|&i| i < rows.len()) {
                    let row = rows[index];
                    state.reward_hover = index;
                    state.status.clear();
                    if state.rewards.contains(&row) {
                        state.rewards.retain(|&selected| selected != row);
                    } else if let Some(table) = catalog.string_table("mp/killstreakTable.csv") {
                        let cost = table.cell(row as i32, 4);
                        if state
                            .rewards
                            .iter()
                            .any(|&selected| table.cell(selected as i32, 4) == cost)
                        {
                            state.status = "Choose rewards with different kill requirements".into();
                        } else if state.rewards.len() == 3 {
                            state.status = "Deselect a reward before choosing another".into();
                        } else {
                            state.rewards.push(row);
                        }
                    }
                }
            }
            "ui_barracks_apply" => {
                if let Ok(selected) = <[u32; 3]>::try_from(state.rewards.clone()) {
                    let mut selected = selected;
                    selected
                        .sort_by_key(|&row| rows.iter().position(|&candidate| candidate == row));
                    let next = sim::PlayerProfile {
                        killstreaks: selected,
                        ..profile.selection
                    };
                    if ui::barracks::valid_profile(&catalog, next) {
                        profile.selection = next;
                        state.status = "Rewards selected".into();
                    }
                } else {
                    state.status = "Choose three rewards".into();
                }
            }
            "ui_barracks_reset" => {
                state.rewards = profile.selection.killstreaks.to_vec();
                state.status.clear();
            }
            _ => {}
        }
    }
    let size = if state.emblems { 48 } else { 27 };
    let table = catalog.string_table(if state.emblems {
        "mp/cardIconTable.csv"
    } else {
        "mp/cardTitleTable.csv"
    });
    for index in 0..48 {
        let row = (state.page * size + index) as i32;
        let visible = index < size && table.is_some_and(|table| row < table.rows as i32);
        let stem = format!("ui_card_choice_{index}");
        dvars.set(&format!("{stem}_visible"), if visible { "1" } else { "0" });
        let selected = if state.emblems {
            profile.selection.emblem
        } else {
            profile.selection.title
        };
        dvars.set(
            &format!("{stem}_selected"),
            if visible && row as u32 == selected {
                "1"
            } else {
                "0"
            },
        );
        dvars.set(
            &format!("{stem}_image"),
            table.map_or("", |table| {
                table.cell(row, if state.emblems { 1 } else { 2 })
            }),
        );
        dvars.set(
            &format!("{stem}_label"),
            table.map_or(String::new(), |table| localized(&loc, table.cell(row, 1))),
        );
    }
    dvars.set(
        "ui_card_page",
        format!(
            "{} / {}",
            state.page + 1,
            table.map_or(1, |table| table.rows.div_ceil(size))
        ),
    );
    let titles = catalog.string_table("mp/cardTitleTable.csv");
    let emblems = catalog.string_table("mp/cardIconTable.csv");
    let title = profile.selection.title as i32;
    let emblem = profile.selection.emblem as i32;
    dvars.set(
        "ui_card_title_image",
        titles.map_or("", |table| table.cell(title, 2)),
    );
    dvars.set(
        "ui_card_title",
        titles.map_or(String::new(), |table| localized(&loc, table.cell(title, 1))),
    );
    dvars.set(
        "ui_card_emblem_image",
        emblems.map_or("", |table| table.cell(emblem, 1)),
    );
    dvars.set("ui_card_name", &settings.player_name);
    let hover = (state.page * size + state.hover) as i32;
    let preview_title = if state.emblems { title } else { hover };
    let preview_emblem = if state.emblems { hover } else { emblem };
    dvars.set(
        "ui_card_preview_title_image",
        titles.map_or("", |table| table.cell(preview_title, 2)),
    );
    dvars.set(
        "ui_card_preview_title",
        titles.map_or(String::new(), |table| {
            localized(&loc, table.cell(preview_title, 1))
        }),
    );
    dvars.set(
        "ui_card_preview_emblem_image",
        emblems.map_or("", |table| table.cell(preview_emblem, 1)),
    );

    let streaks = catalog.string_table("mp/killstreakTable.csv");
    for index in 0..15 {
        let row = rows.get(index).copied();
        let stem = format!("ui_streak_{index}");
        dvars.set(
            &format!("{stem}_visible"),
            if row.is_some() { "1" } else { "0" },
        );
        dvars.set(
            &format!("{stem}_selected"),
            if row.is_some_and(|row| state.rewards.contains(&row)) {
                "1"
            } else {
                "0"
            },
        );
        let cell = |column| {
            streaks
                .zip(row)
                .map_or("", |(table, row)| table.cell(row as i32, column))
        };
        dvars.set(
            &format!("{stem}_label"),
            format!("{}  {}", cell(4), localized(&loc, cell(2))),
        );
        dvars.set(&format!("{stem}_image"), cell(14));
    }
    let row = rows.get(state.reward_hover).copied();
    let cell = |column| {
        streaks
            .zip(row)
            .map_or("", |(table, row)| table.cell(row as i32, column))
    };
    dvars.set("ui_streak_preview", cell(14));
    dvars.set("ui_streak_name", localized(&loc, cell(2)));
    dvars.set("ui_streak_desc", localized(&loc, cell(3)));
    dvars.set(
        "ui_streak_summary",
        format!("{} / 3 rewards selected", state.rewards.len()),
    );
    dvars.set("ui_barracks_status", &state.status);
    state.published = (!dvar_writes).then(|| BarracksInputs {
        selection: profile.selection,
        player_name: settings.player_name.clone(),
        catalog: catalog.last_changed(),
        loc: loc.last_changed(),
    });
}

pub(crate) fn sync_profile(
    profile: Res<BarracksProfile>,
    (generation, has_world, role): (
        Res<frame::WorldGeneration>,
        Res<frame::HasWorld>,
        Res<frame::RuntimeRole>,
    ),
    local: Option<Res<net::LocalPresentClient>>,
    link: Option<Res<net::UdpClientLink>>,
    mut inbox: Option<ResMut<net::ClientActionInbox>>,
    mut seq: ResMut<net::ActionRequestIds>,
    mut sent: Local<Option<(frame::WorldGeneration, sim::ClientId, sim::PlayerProfile)>>,
) {
    if !has_world.0 || *role == frame::RuntimeRole::Replay {
        *sent = None;
        return;
    }
    let (Some(local), Some(inbox)) = (local, inbox.as_deref_mut()) else {
        return;
    };
    if let Some(link) = link
        && (link.connection.is_none()
            || link.assigned_client != Some(local.0)
            || !link.has_entered_match())
    {
        *sent = None;
        return;
    }
    if !profile.loaded {
        return;
    }
    let selection = profile.selection;
    let next = (*generation, local.0, selection);
    if sent.as_ref() == Some(&next) {
        return;
    }
    let request_id = seq.allocate();
    if let Err(error) = inbox.push(
        local.0,
        sim::ClientAction::SetProfile {
            request_id,
            profile: selection,
        },
    ) {
        diag::warn!(
            Console,
            "profile: request_id={request_id} not queued — {error}"
        );
    } else {
        *sent = Some(next);
    }
}

#[cfg(test)]
mod publish_tests {
    use super::*;
    use asset_game::{CapturedStringTable, LocalizeCatalog, MenuCatalog};
    use std::collections::BTreeMap;

    fn table(
        name: &str,
        columns: usize,
        rows: usize,
        cell: impl Fn(usize, usize) -> String,
    ) -> CapturedStringTable {
        CapturedStringTable {
            name: name.to_owned(),
            columns,
            rows,
            cells: (0..rows)
                .flat_map(|row| (0..columns).map(move |col| (row, col)))
                .map(|(row, col)| cell(row, col))
                .collect(),
        }
    }

    fn catalog() -> MenuCatalog {
        let mut catalog = MenuCatalog::default();
        for (name, columns, rows) in [
            ("mp/cardTitleTable.csv", 3, 31),
            ("mp/cardIconTable.csv", 2, 57),
        ] {
            catalog.string_tables.insert(
                name.to_owned(),
                table(name, columns, rows, |row, col| {
                    format!("{name}:{row}:{col}")
                }),
            );
        }
        let names = [
            "none",
            "uav",
            "airdrop",
            "sentry",
            "predator_missile",
            "harrier",
            "nuke",
        ];
        catalog.string_tables.insert(
            "mp/killstreakTable.csv".to_owned(),
            table(
                "mp/killstreakTable.csv",
                15,
                names.len(),
                |row, col| match col {
                    1 => names[row].to_owned(),
                    4 => ((row * 7) % 26).to_string(),
                    _ => format!("ks{row}:{col}"),
                },
            ),
        );
        catalog
    }

    fn app() -> App {
        let mut app = App::new();
        let mut profile = BarracksProfile::default();
        profile.loaded = true;
        app.add_message::<ConsoleCommand>()
            .insert_resource(catalog())
            .init_resource::<LocalizeCatalog>()
            .insert_resource(frame::GameSettings::default())
            .insert_resource(profile)
            .init_resource::<UiMenuDvars>()
            .add_systems(Update, route);
        app
    }

    fn dvars(app: &App) -> BTreeMap<String, String> {
        app.world()
            .resource::<UiMenuDvars>()
            .iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }

    type Change = fn(&mut World);

    #[test]
    fn skipped_frames_leave_the_dvars_every_frame_would_have_written() {
        // `written` only rewrites the dvars when something changed; `every`
        // gets a console `set` each frame, which makes the route write them
        // every frame as it used to (nothing here applies the `set` itself).
        let mut written = app();
        let mut every = app();
        let frames: Vec<(&[&str], Option<Change>)> = vec![
            (&[], None),
            (&[], None),
            (&["ui_barracks_open titles"], None),
            (&["ui_barracks_hover 5"], None),
            (&[], None),
            (&["ui_barracks_page 1"], None),
            (
                &[],
                Some(|world| world.resource_mut::<BarracksProfile>().selection.title = 3),
            ),
            (&[], None),
            (
                &[],
                Some(|world| {
                    world.resource_mut::<frame::GameSettings>().player_name = "Soap".into()
                }),
            ),
            (&[], None),
            (
                &[],
                Some(|world| {
                    let mut catalog = world.resource_mut::<MenuCatalog>();
                    let table = catalog
                        .string_tables
                        .get_mut("mp/cardTitleTable.csv")
                        .expect("titles");
                    table.cells[3 * 3 + 2] = "renamed".into();
                }),
            ),
            (&["ui_barracks_open emblems", "ui_barracks_pick 4"], None),
            (&[], None),
            (
                &[
                    "ui_barracks_open killstreaks",
                    "ui_barracks_reward 2",
                    "ui_barracks_reward 4",
                ],
                None,
            ),
            (
                &["ui_barracks_reward 1", "ui_barracks_reward_hover 3"],
                None,
            ),
            (&["ui_barracks_apply"], None),
            (&[], None),
            (
                &[],
                Some(|world| world.insert_resource(LocalizeCatalog::default())),
            ),
            (&[], None),
            (
                &[],
                Some(|world| world.resource_mut::<BarracksProfile>().loaded = false),
            ),
            (&["ui_barracks_hover 2"], None),
            (
                &[],
                Some(|world| world.resource_mut::<BarracksProfile>().loaded = true),
            ),
            (&[], None),
            (&["set ui_card_title mine"], None),
            (&[], None),
            (&[], None),
        ];
        let mut skipped = 0;
        for (at, (lines, change)) in frames.into_iter().enumerate() {
            for (app, force) in [(&mut written, false), (&mut every, true)] {
                if let Some(change) = change {
                    change(app.world_mut());
                }
                for line in lines
                    .iter()
                    .copied()
                    .chain(force.then_some("set iw4l_test_force 1"))
                {
                    app.world_mut()
                        .write_message(ConsoleCommand::parse(line).expect("command"));
                }
                let before = app.world().resource_ref::<UiMenuDvars>().last_changed();
                app.update();
                if !force && app.world().resource_ref::<UiMenuDvars>().last_changed() == before {
                    skipped += 1;
                }
            }
            assert_eq!(dvars(&written), dvars(&every), "frame {at}");
        }
        assert!(dvars(&written).len() > 200);
        // The quiet frames really were skipped (unloaded frames write nothing
        // either way).
        assert!(skipped >= 10, "skipped {skipped}");
    }
}
