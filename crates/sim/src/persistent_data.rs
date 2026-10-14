use crate::ClientId;
use bevy_ecs::prelude::Resource;
use std::{collections::BTreeMap, sync::Arc};
use structured_data_iw4::{Definition, DefinitionSet, Key, Value};

pub const PLAYER_DATA_BUFFER_BYTES: usize = 8188;
const PLAYER_SCHEMA: &str = "mp/playerdata.def";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountId(pub [u8; 16]);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSnapshot {
    pub account: AccountId,
    pub revision: u64,
    pub version: i32,
    pub checksum: u32,
    pub bytes: Vec<u8>,
    pub skills: crate::SkillRatings,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistentDataError {
    MissingSchema,
    MissingDefaults,
    InvalidDefaults,
    InvalidSchema,
    InvalidIdentity,
    UnknownAccount,
    UnknownClient,
    AccountInUse,
    ClientInUse,
    InvalidBuffer,
    SchemaMismatch,
    RevisionOverflow,
    InvalidOpponent,
    Skill(crate::SkillRatingError),
    Field(structured_data_iw4::Error),
}
impl From<structured_data_iw4::Error> for PersistentDataError {
    fn from(error: structured_data_iw4::Error) -> Self {
        Self::Field(error)
    }
}
#[derive(Clone, Debug)]
struct AccountRecord {
    definition: Arc<Definition>,
    bytes: Vec<u8>,
    dirty: Vec<u8>,
    revision: u64,
    saved_revision: Option<u64>,
    temporary: bool,
    skills: crate::SkillRatings,
}
#[derive(Resource, Clone, Debug, Default)]
pub struct PersistentDataStore {
    schemas: BTreeMap<String, Arc<DefinitionSet>>,
    accounts: BTreeMap<AccountId, AccountRecord>,
    clients: BTreeMap<ClientId, AccountId>,
    defaults: Option<Arc<crate::PlayerDataDefaults>>,
}

impl PersistentDataStore {
    pub fn schemas(&self) -> &BTreeMap<String, Arc<DefinitionSet>> {
        &self.schemas
    }

    pub fn install_schemas(
        &mut self,
        schemas: BTreeMap<String, Arc<DefinitionSet>>,
    ) -> Result<(), PersistentDataError> {
        for set in schemas.values() {
            for definition in &set.definitions {
                definition
                    .validate()
                    .map_err(|_| PersistentDataError::InvalidSchema)?;
            }
        }
        let mut replacements = Vec::new();
        for (account, record) in &self.accounts {
            let set = schemas
                .get(PLAYER_SCHEMA)
                .ok_or(PersistentDataError::MissingSchema)?;
            let definition =
                select_definition(set, record.definition.version, record.definition.checksum)?;
            if record.temporary {
                validate_buffer_size(definition, &record.bytes)?;
            } else {
                validate_buffer(definition, &record.bytes)?;
            }
            replacements.push((*account, Arc::new(definition.clone())));
        }
        self.schemas = schemas;
        for (account, definition) in replacements {
            self.accounts
                .get_mut(&account)
                .ok_or(PersistentDataError::UnknownAccount)?
                .definition = definition;
        }
        Ok(())
    }

    pub fn import(&mut self, snapshot: AccountSnapshot) -> Result<(), PersistentDataError> {
        validate_identity(snapshot.account)?;
        if self.accounts.contains_key(&snapshot.account) {
            return Err(PersistentDataError::AccountInUse);
        }
        let set = self
            .schemas
            .get(PLAYER_SCHEMA)
            .ok_or(PersistentDataError::MissingSchema)?;
        let definition = select_definition(set, snapshot.version, snapshot.checksum)?;
        validate_buffer(definition, &snapshot.bytes)?;
        self.accounts.insert(
            snapshot.account,
            AccountRecord {
                definition: Arc::new(definition.clone()),
                bytes: snapshot.bytes,
                dirty: vec![0; PLAYER_DATA_BUFFER_BYTES.div_ceil(8)],
                revision: snapshot.revision,
                saved_revision: Some(snapshot.revision),
                temporary: false,
                skills: snapshot.skills,
            },
        );
        Ok(())
    }

    pub fn initialize(
        &mut self,
        account: AccountId,
        defaults: &crate::PlayerDataDefaults,
    ) -> Result<(), PersistentDataError> {
        validate_identity(account)?;
        if self.accounts.contains_key(&account) {
            return Err(PersistentDataError::AccountInUse);
        }
        let definition = self
            .schemas
            .get(PLAYER_SCHEMA)
            .ok_or(PersistentDataError::MissingSchema)?
            .definitions
            .first()
            .ok_or(PersistentDataError::InvalidSchema)?;
        let bytes = crate::persistent_defaults::initialize(definition, defaults)?;
        self.create(account, definition.version, definition.checksum, bytes)
    }

    pub fn create(
        &mut self,
        account: AccountId,
        version: i32,
        checksum: u32,
        bytes: Vec<u8>,
    ) -> Result<(), PersistentDataError> {
        if self.accounts.contains_key(&account) {
            return Err(PersistentDataError::AccountInUse);
        }
        self.import(AccountSnapshot {
            account,
            revision: 0,
            version,
            checksum,
            bytes,
            skills: crate::SkillRatings::default(),
        })?;
        self.accounts
            .get_mut(&account)
            .ok_or(PersistentDataError::UnknownAccount)?
            .saved_revision = None;
        Ok(())
    }

    pub fn install_defaults(&mut self, defaults: Option<Arc<crate::PlayerDataDefaults>>) {
        self.defaults = defaults;
    }

    fn preflight_binding(
        &self,
        client: ClientId,
        account: AccountId,
        temporary: bool,
    ) -> Result<bool, PersistentDataError> {
        validate_identity(account)?;
        if self.clients.get(&client).is_some_and(|id| *id != account) {
            return Err(PersistentDataError::ClientInUse);
        }
        if self
            .clients
            .iter()
            .any(|(other, id)| *other != client && *id == account)
        {
            return Err(PersistentDataError::AccountInUse);
        }
        if let Some(record) = self.accounts.get(&account) {
            if record.temporary != temporary {
                return Err(PersistentDataError::AccountInUse);
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub fn bind_snapshot(
        &mut self,
        client: ClientId,
        account: AccountId,
        snapshot: Option<&AccountSnapshot>,
    ) -> Result<bool, PersistentDataError> {
        if snapshot.is_some_and(|snapshot| snapshot.account != account) {
            return Err(PersistentDataError::InvalidIdentity);
        }
        if !self.preflight_binding(client, account, false)? {
            let Some(snapshot) = snapshot else {
                return Ok(false);
            };
            self.import(snapshot.clone())?;
        }
        self.bind(client, account)?;
        Ok(true)
    }

    pub fn admit(
        &mut self,
        client: ClientId,
        account: AccountId,
        snapshot: Option<&AccountSnapshot>,
        defaults: Option<&crate::PlayerDataDefaults>,
    ) -> Result<(), PersistentDataError> {
        if self.bind_snapshot(client, account, snapshot)? {
            return Ok(());
        }
        let installed = self.defaults.clone();
        self.initialize(
            account,
            defaults
                .or(installed.as_deref())
                .ok_or(PersistentDataError::MissingDefaults)?,
        )?;
        self.bind(client, account)
    }

    pub fn admit_temporary(
        &mut self,
        client: ClientId,
        account: AccountId,
    ) -> Result<(), PersistentDataError> {
        if !self.preflight_binding(client, account, true)? {
            let definition = self
                .schemas
                .get(PLAYER_SCHEMA)
                .ok_or(PersistentDataError::MissingSchema)?
                .definitions
                .first()
                .ok_or(PersistentDataError::InvalidSchema)?;
            let bytes = vec![0; PLAYER_DATA_BUFFER_BYTES];
            validate_buffer_size(definition, &bytes)?;
            self.accounts.insert(
                account,
                AccountRecord {
                    definition: Arc::new(definition.clone()),
                    bytes,
                    dirty: vec![0; PLAYER_DATA_BUFFER_BYTES.div_ceil(8)],
                    revision: 0,
                    saved_revision: None,
                    temporary: true,
                    skills: crate::SkillRatings::default(),
                },
            );
        }
        self.bind(client, account)
    }

    pub fn bind(
        &mut self,
        client: ClientId,
        account: AccountId,
    ) -> Result<(), PersistentDataError> {
        if !self.accounts.contains_key(&account) {
            return Err(PersistentDataError::UnknownAccount);
        }
        if self
            .clients
            .iter()
            .any(|(other, id)| *other != client && *id == account)
        {
            return Err(PersistentDataError::AccountInUse);
        }
        if self.clients.get(&client).is_some_and(|id| *id != account) {
            return Err(PersistentDataError::ClientInUse);
        }
        self.clients.insert(client, account);
        Ok(())
    }
    pub fn unbind(&mut self, client: ClientId) {
        if let Some(account) = self.clients.remove(&client)
            && self
                .accounts
                .get(&account)
                .is_some_and(|record| record.temporary)
        {
            self.accounts.remove(&account);
        }
    }
    pub fn clear_bindings(&mut self) {
        self.clients.clear();
        self.accounts.retain(|_, record| !record.temporary);
    }
    pub fn account(&self, client: ClientId) -> Option<AccountId> {
        self.clients.get(&client).copied()
    }
    pub fn for_new_match(&self) -> Self {
        let mut store = self.clone();
        store.clear_bindings();
        store
    }

    pub fn read<'a>(
        &'a self,
        client: ClientId,
        keys: &[Key<'_>],
    ) -> Result<Value<'a>, PersistentDataError> {
        let account = self
            .account(client)
            .ok_or(PersistentDataError::UnknownClient)?;
        let record = self
            .accounts
            .get(&account)
            .ok_or(PersistentDataError::UnknownAccount)?;
        Ok(record
            .definition
            .read(&record.bytes, record.definition.lookup(keys)?)?)
    }
    /// Reads one field of an account snapshot, a player's own copy of its data, with
    /// the installed schema. MW2's menus read the local player's stats this way
    /// (getPlayerData in a menu expression), not the server's copy.
    pub fn read_snapshot<'a>(
        &'a self,
        snapshot: &'a AccountSnapshot,
        keys: &[Key<'_>],
    ) -> Result<Value<'a>, PersistentDataError> {
        let set = self
            .schemas
            .get(PLAYER_SCHEMA)
            .ok_or(PersistentDataError::MissingSchema)?;
        let definition = select_definition(set, snapshot.version, snapshot.checksum)?;
        validate_buffer(definition, &snapshot.bytes)?;
        Ok(definition.read(&snapshot.bytes, definition.lookup(keys)?)?)
    }
    pub fn path_type(
        &self,
        client: ClientId,
        keys: &[Key<'_>],
    ) -> Result<structured_data_iw4::DataType, PersistentDataError> {
        let account = self
            .account(client)
            .ok_or(PersistentDataError::UnknownClient)?;
        let record = self
            .accounts
            .get(&account)
            .ok_or(PersistentDataError::UnknownAccount)?;
        Ok(record.definition.path_type(keys)?)
    }

    pub fn field_type(
        &self,
        client: ClientId,
        keys: &[Key<'_>],
    ) -> Result<structured_data_iw4::DataType, PersistentDataError> {
        let account = self
            .account(client)
            .ok_or(PersistentDataError::UnknownClient)?;
        let record = self
            .accounts
            .get(&account)
            .ok_or(PersistentDataError::UnknownAccount)?;
        Ok(record.definition.lookup(keys)?.ty)
    }

    /// Apply a class selection as one transaction, without exposing partial loadouts.
    pub fn write_many(
        &mut self,
        client: ClientId,
        fields: &[(&[Key<'_>], Value<'_>)],
    ) -> Result<bool, PersistentDataError> {
        let account = self
            .account(client)
            .ok_or(PersistentDataError::UnknownClient)?;
        let record = self
            .accounts
            .get(&account)
            .ok_or(PersistentDataError::UnknownAccount)?;
        let mut updated = record.clone();
        let mut changed = false;
        for (keys, value) in fields {
            let lookup = updated.definition.lookup(keys)?;
            changed |=
                updated
                    .definition
                    .write(&mut updated.bytes, &mut updated.dirty, lookup, *value)?;
        }
        if changed {
            updated.revision = updated
                .revision
                .checked_add(1)
                .ok_or(PersistentDataError::RevisionOverflow)?;
            self.accounts.insert(account, updated);
        }
        Ok(changed)
    }
    pub fn write(
        &mut self,
        client: ClientId,
        keys: &[Key<'_>],
        value: Value<'_>,
    ) -> Result<bool, PersistentDataError> {
        let account = self
            .account(client)
            .ok_or(PersistentDataError::UnknownClient)?;
        let record = self
            .accounts
            .get_mut(&account)
            .ok_or(PersistentDataError::UnknownAccount)?;
        let lookup = record.definition.lookup(keys)?;
        let revision = record
            .revision
            .checked_add(1)
            .ok_or(PersistentDataError::RevisionOverflow)?;
        let changed =
            record
                .definition
                .write(&mut record.bytes, &mut record.dirty, lookup, value)?;
        if changed {
            record.revision = revision;
        }
        Ok(changed)
    }
    pub fn enum_index(
        &self,
        client: ClientId,
        keys: &[Key<'_>],
    ) -> Result<Option<(u16, bool)>, PersistentDataError> {
        let account = self
            .account(client)
            .ok_or(PersistentDataError::UnknownClient)?;
        let record = self
            .accounts
            .get(&account)
            .ok_or(PersistentDataError::UnknownAccount)?;
        Ok(record
            .definition
            .enum_index(&record.bytes, record.definition.lookup(keys)?)?)
    }
    pub fn update_skill(
        &mut self,
        first: ClientId,
        second: ClientId,
        mode: &str,
        score: f32,
    ) -> Result<(), PersistentDataError> {
        let first = self
            .account(first)
            .ok_or(PersistentDataError::UnknownClient)?;
        let second = self
            .account(second)
            .ok_or(PersistentDataError::UnknownClient)?;
        if first == second {
            return Err(PersistentDataError::InvalidOpponent);
        }
        let a = self
            .accounts
            .get(&first)
            .ok_or(PersistentDataError::UnknownAccount)?;
        let b = self
            .accounts
            .get(&second)
            .ok_or(PersistentDataError::UnknownAccount)?;
        let (a_skills, b_skills) = a
            .skills
            .updated_pair(b.skills, mode, score)
            .map_err(PersistentDataError::Skill)?;
        let a_revision = a
            .revision
            .checked_add(1)
            .ok_or(PersistentDataError::RevisionOverflow)?;
        let b_revision = b
            .revision
            .checked_add(1)
            .ok_or(PersistentDataError::RevisionOverflow)?;
        for (account, record) in &mut self.accounts {
            if *account == first {
                record.skills = a_skills;
                record.revision = a_revision;
            } else if *account == second {
                record.skills = b_skills;
                record.revision = b_revision;
            }
        }
        Ok(())
    }

    pub fn snapshot(&self, account: AccountId) -> Option<AccountSnapshot> {
        let record = self.accounts.get(&account)?;
        Some(AccountSnapshot {
            account,
            revision: record.revision,
            version: record.definition.version,
            checksum: record.definition.checksum,
            bytes: record.bytes.clone(),
            skills: record.skills,
        })
    }
    pub fn pending(&self) -> Vec<AccountSnapshot> {
        self.accounts
            .iter()
            .filter(|(_, record)| {
                !record.temporary && record.saved_revision != Some(record.revision)
            })
            .filter_map(|(account, _)| self.snapshot(*account))
            .collect()
    }
    pub fn acknowledge_saved(&mut self, account: AccountId, revision: u64) -> bool {
        let Some(record) = self.accounts.get_mut(&account) else {
            return false;
        };
        if record.temporary || record.revision != revision {
            return false;
        }
        record.saved_revision = Some(revision);
        record.dirty.fill(0);
        true
    }
}
fn validate_identity(account: AccountId) -> Result<(), PersistentDataError> {
    if account.0 == [0; 16] {
        Err(PersistentDataError::InvalidIdentity)
    } else {
        Ok(())
    }
}
fn select_definition(
    set: &DefinitionSet,
    version: i32,
    checksum: u32,
) -> Result<&Definition, PersistentDataError> {
    set.definitions
        .iter()
        .find(|definition| definition.version == version && definition.checksum == checksum)
        .ok_or(PersistentDataError::SchemaMismatch)
}
fn validate_buffer_size(definition: &Definition, bytes: &[u8]) -> Result<(), PersistentDataError> {
    if bytes.len() != PLAYER_DATA_BUFFER_BYTES
        || !(8..=PLAYER_DATA_BUFFER_BYTES).contains(&definition.size)
    {
        return Err(PersistentDataError::InvalidBuffer);
    }
    Ok(())
}
fn validate_buffer(definition: &Definition, bytes: &[u8]) -> Result<(), PersistentDataError> {
    validate_buffer_size(definition, bytes)?;
    if bytes[..4] != definition.version.to_le_bytes()
        || bytes[4..8] != definition.checksum.to_le_bytes()
    {
        return Err(PersistentDataError::SchemaMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use structured_data_iw4::{DataType, Enum, EnumArray, EnumEntry, Property, Struct};

    fn challenge_schema() -> Definition {
        Definition {
            version: 7,
            checksum: 0xC0FFEE,
            size: 16,
            root: DataType::Struct(0),
            enums: vec![Enum {
                reserved: 4,
                entries: vec![
                    EnumEntry {
                        name: "ch_expert_m4".into(),
                        index: 1,
                    },
                    EnumEntry {
                        name: "ch_marksman_m4".into(),
                        index: 0,
                    },
                ],
            }],
            structs: vec![Struct {
                properties: vec![Property {
                    name: "challengeState".into(),
                    ty: DataType::EnumArray(0),
                    offset: 8,
                }],
                size: 16,
                bit_offset: 0,
            }],
            indexed_arrays: Vec::new(),
            enum_arrays: vec![EnumArray {
                enumeration: 0,
                element: DataType::Byte,
                stride: 1,
            }],
        }
    }

    fn store_and_snapshot(definition: &Definition) -> (PersistentDataStore, AccountSnapshot) {
        definition.validate().expect("test schema is valid");
        let mut store = PersistentDataStore::default();
        store
            .install_schemas(BTreeMap::from([(
                PLAYER_SCHEMA.to_owned(),
                Arc::new(DefinitionSet {
                    definitions: vec![definition.clone()],
                }),
            )]))
            .expect("schemas install");
        let mut bytes = vec![0; PLAYER_DATA_BUFFER_BYTES];
        bytes[..4].copy_from_slice(&definition.version.to_le_bytes());
        bytes[4..8].copy_from_slice(&definition.checksum.to_le_bytes());
        bytes[8] = 2; // ch_marksman_m4: first tier done
        let snapshot = AccountSnapshot {
            account: AccountId([1; 16]),
            revision: 3,
            version: definition.version,
            checksum: definition.checksum,
            bytes,
            skills: crate::SkillRatings::default(),
        };
        (store, snapshot)
    }

    #[test]
    fn read_snapshot_reads_a_challenge_state() {
        let definition = challenge_schema();
        let (store, snapshot) = store_and_snapshot(&definition);
        let state =
            |name| store.read_snapshot(&snapshot, &[Key::Name("challengeState"), Key::Name(name)]);
        assert_eq!(state("ch_marksman_m4"), Ok(Value::Int(2)));
        assert_eq!(state("ch_expert_m4"), Ok(Value::Int(0)));
        assert_eq!(
            state("ch_unknown"),
            Err(PersistentDataError::Field(
                structured_data_iw4::Error::UnknownName
            ))
        );
    }

    #[test]
    fn read_snapshot_refuses_another_schema() {
        let definition = challenge_schema();
        let (store, mut snapshot) = store_and_snapshot(&definition);
        snapshot.version += 1;
        assert_eq!(
            store.read_snapshot(&snapshot, &[Key::Name("challengeState")]),
            Err(PersistentDataError::SchemaMismatch)
        );
        let empty = PersistentDataStore::default();
        snapshot.version -= 1;
        assert_eq!(
            empty.read_snapshot(&snapshot, &[Key::Name("challengeState")]),
            Err(PersistentDataError::MissingSchema)
        );
    }
}
