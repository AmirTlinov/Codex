use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RolePromptVersion {
    pub(crate) version: u32,
    pub(crate) prompt_profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamProfile {
    pub(crate) profile_id: String,
    pub(crate) role_versions: HashMap<String, u32>,
    pub(crate) updated_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunbookMemoryRecord {
    pub(crate) record_id: String,
    pub(crate) owner: String,
    pub(crate) payload: String,
    pub(crate) expires_at: Instant,
    pub(crate) archived_at: Option<Instant>,
}

#[derive(Debug, Default)]
pub(crate) struct TeamLifecycleStore {
    role_catalog: HashMap<String, BTreeMap<u32, RolePromptVersion>>,
    team_profiles: HashMap<String, TeamProfile>,
    active_runbook_memory: HashMap<String, RunbookMemoryRecord>,
    archived_runbook_memory: HashMap<String, RunbookMemoryRecord>,
}

impl TeamLifecycleStore {
    pub(crate) fn upsert_role_prompt(
        &mut self,
        role: String,
        version: u32,
        prompt_profile: String,
    ) -> Result<(), String> {
        let role_versions = self.role_catalog.entry(role.clone()).or_default();
        if let Some(existing) = role_versions.get(&version) {
            if existing.prompt_profile == prompt_profile {
                return Ok(());
            }
            return Err(format!(
                "role `{role}` version `{version}` is already registered with a different prompt_profile"
            ));
        }

        role_versions.insert(
            version,
            RolePromptVersion {
                version,
                prompt_profile,
            },
        );
        Ok(())
    }

    pub(crate) fn create_or_update_profile(
        &mut self,
        profile_id: String,
        role_versions: HashMap<String, u32>,
        now: Instant,
    ) -> Result<(), String> {
        if profile_id.trim().is_empty() {
            return Err("profile_id cannot be empty".to_string());
        }
        if role_versions.is_empty() {
            return Err("profile must include at least one role version".to_string());
        }

        for (role, version) in &role_versions {
            let Some(versions) = self.role_catalog.get(role) else {
                return Err(format!("unknown role in profile: {role}"));
            };
            if !versions.contains_key(version) {
                return Err(format!(
                    "role `{role}` does not contain version `{version}`"
                ));
            }
        }

        self.team_profiles.insert(
            profile_id.clone(),
            TeamProfile {
                profile_id,
                role_versions,
                updated_at: now,
            },
        );

        Ok(())
    }

    pub(crate) fn profile(&self, profile_id: &str) -> Option<&TeamProfile> {
        self.team_profiles.get(profile_id)
    }

    pub(crate) fn put_runbook_memory(&mut self, record: RunbookMemoryRecord) -> Result<(), String> {
        if let Some(existing) = self.active_runbook_memory.get(&record.record_id) {
            if existing == &record {
                return Ok(());
            }
            return Err(format!(
                "runbook memory record `{}` already exists in active storage",
                record.record_id
            ));
        }
        if self.archived_runbook_memory.contains_key(&record.record_id) {
            return Err(format!(
                "runbook memory record `{}` already exists in archive",
                record.record_id
            ));
        }

        self.active_runbook_memory
            .insert(record.record_id.clone(), record);
        Ok(())
    }

    pub(crate) fn sweep_expired_runbook_memory(&mut self, now: Instant) -> Vec<String> {
        let expired_ids = self
            .active_runbook_memory
            .iter()
            .filter_map(|(record_id, record)| (record.expires_at <= now).then_some(record_id))
            .cloned()
            .collect::<Vec<_>>();

        for record_id in &expired_ids {
            if let Some(mut record) = self.active_runbook_memory.remove(record_id) {
                record.archived_at = Some(now);
                self.archived_runbook_memory
                    .insert(record_id.clone(), record);
            }
        }

        expired_ids
    }

    pub(crate) fn archived_record(&self, record_id: &str) -> Option<&RunbookMemoryRecord> {
        self.archived_runbook_memory.get(record_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_catalog_profile_lifecycle() {
        let mut store = TeamLifecycleStore::default();
        store
            .upsert_role_prompt("scout".to_string(), 1, "scout-v1".to_string())
            .expect("first role prompt should register");
        store
            .upsert_role_prompt("validator".to_string(), 2, "validator-v2".to_string())
            .expect("first role prompt should register");

        store
            .upsert_role_prompt("scout".to_string(), 1, "scout-v1".to_string())
            .expect("same role prompt version should be idempotent");
        let conflicting = store.upsert_role_prompt("scout".to_string(), 1, "scout-v1b".to_string());
        assert!(
            conflicting
                .as_ref()
                .is_err_and(|message| message.contains("different prompt_profile")),
            "unexpected result: {conflicting:?}"
        );

        let now = Instant::now();
        let profile_roles =
            HashMap::from([("scout".to_string(), 1u32), ("validator".to_string(), 2u32)]);

        store
            .create_or_update_profile("team-alpha".to_string(), profile_roles, now)
            .expect("profile should be valid");

        let profile = store.profile("team-alpha").expect("profile should exist");
        assert_eq!(profile.role_versions.get("scout"), Some(&1u32));
        assert_eq!(profile.role_versions.get("validator"), Some(&2u32));
    }

    #[test]
    fn team_profile_config_roundtrip() {
        let mut store = TeamLifecycleStore::default();
        store
            .upsert_role_prompt("scout".to_string(), 1, "scout-v1".to_string())
            .expect("scout role prompt should register");
        store
            .upsert_role_prompt("validator".to_string(), 1, "validator-v1".to_string())
            .expect("validator role prompt should register");
        store
            .upsert_role_prompt("validator".to_string(), 2, "validator-v2".to_string())
            .expect("validator v2 role prompt should register");

        let initial_now = Instant::now();
        let initial_roles =
            HashMap::from([("scout".to_string(), 1u32), ("validator".to_string(), 1u32)]);
        store
            .create_or_update_profile("team-mesh".to_string(), initial_roles, initial_now)
            .expect("initial profile should be valid");

        let initial_profile = store
            .profile("team-mesh")
            .expect("initial profile should exist")
            .clone();
        assert_eq!(initial_profile.profile_id, "team-mesh");
        assert_eq!(initial_profile.role_versions.len(), 2);
        assert_eq!(initial_profile.role_versions.get("scout"), Some(&1u32));
        assert_eq!(initial_profile.role_versions.get("validator"), Some(&1u32));

        let updated_now = initial_now + std::time::Duration::from_secs(5);
        let mut updated_roles = initial_profile.role_versions;
        updated_roles.insert("validator".to_string(), 2u32);
        store
            .create_or_update_profile("team-mesh".to_string(), updated_roles, updated_now)
            .expect("profile update should remain valid");

        let updated_profile = store
            .profile("team-mesh")
            .expect("updated profile should exist");
        assert_eq!(updated_profile.profile_id, "team-mesh");
        assert_eq!(updated_profile.role_versions.get("scout"), Some(&1u32));
        assert_eq!(updated_profile.role_versions.get("validator"), Some(&2u32));
        assert_eq!(updated_profile.updated_at, updated_now);
    }

    #[test]
    fn runbook_memory_lifecycle() {
        let mut store = TeamLifecycleStore::default();
        let now = Instant::now();
        let expired_at = now;

        let record = RunbookMemoryRecord {
            record_id: "mem-1".to_string(),
            owner: "orchestrator".to_string(),
            payload: "runbook state".to_string(),
            expires_at: expired_at,
            archived_at: None,
        };

        store
            .put_runbook_memory(record.clone())
            .expect("new record should be accepted");
        store
            .put_runbook_memory(record)
            .expect("same record should be idempotent");

        let duplicate = store.put_runbook_memory(RunbookMemoryRecord {
            record_id: "mem-1".to_string(),
            owner: "validator".to_string(),
            payload: "different".to_string(),
            expires_at: expired_at,
            archived_at: None,
        });
        assert!(
            duplicate
                .as_ref()
                .is_err_and(|message| message.contains("active storage")),
            "unexpected result: {duplicate:?}"
        );

        let archived = store.sweep_expired_runbook_memory(now);
        assert_eq!(archived, vec!["mem-1".to_string()]);

        let archived_record = store
            .archived_record("mem-1")
            .expect("record must be archived");
        assert_eq!(archived_record.owner, "orchestrator");
        assert_eq!(archived_record.archived_at, Some(now));
    }
}
