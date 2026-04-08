//! Sled-backed Team Store implementing TeamPort.
//!
//! Teams persist across daemon restarts via sled.
//! Capability mask intersection is enforced in PolicyKernel.

use async_trait::async_trait;
use pipit_core::integration_ports::*;

/// Sled-backed team store.
pub struct SledTeamStore {
    db: sled::Db,
}

impl SledTeamStore {
    pub fn new(db_path: &std::path::Path) -> Result<Self, String> {
        let db = sled::open(db_path.join("teams"))
            .map_err(|e| format!("Failed to open team store: {}", e))?;
        Ok(Self { db })
    }

    fn team_key(id: &str) -> Vec<u8> {
        format!("team:{}", id).into_bytes()
    }

    fn user_key(user_id: &str) -> Vec<u8> {
        format!("user_teams:{}", user_id).into_bytes()
    }
}

#[async_trait]
impl TeamPort for SledTeamStore {
    async fn create_team(&self, name: &str, creator: &str) -> Result<Team, TeamError> {
        let id = format!("team-{}", uuid::Uuid::new_v4().to_string()[..8].to_string());

        if self.db.contains_key(Self::team_key(&id)).unwrap_or(false) {
            return Err(TeamError::AlreadyExists(name.to_string()));
        }

        let team = Team {
            id: id.clone(),
            name: name.to_string(),
            members: vec![TeamMember {
                user_id: creator.to_string(),
                role: TeamRole::Admin,
                joined_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            }],
            settings: TeamSettings::default(),
        };

        let json = serde_json::to_vec(&team).map_err(|e| TeamError::Storage(e.to_string()))?;
        self.db
            .insert(Self::team_key(&id), json)
            .map_err(|e| TeamError::Storage(e.to_string()))?;

        // Add to user's team list
        let mut user_teams = self.user_team_ids(creator);
        user_teams.push(id.clone());
        let user_json =
            serde_json::to_vec(&user_teams).map_err(|e| TeamError::Storage(e.to_string()))?;
        self.db
            .insert(Self::user_key(creator), user_json)
            .map_err(|e| TeamError::Storage(e.to_string()))?;

        self.db
            .flush()
            .map_err(|e| TeamError::Storage(e.to_string()))?;
        Ok(team)
    }

    async fn delete_team(&self, team_id: &str) -> Result<(), TeamError> {
        self.db
            .remove(Self::team_key(team_id))
            .map_err(|e| TeamError::Storage(e.to_string()))?;
        self.db
            .flush()
            .map_err(|e| TeamError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn add_member(
        &self,
        team_id: &str,
        user_id: &str,
        role: TeamRole,
    ) -> Result<(), TeamError> {
        let mut team = self.get_team(team_id).await?;
        if team.members.iter().any(|m| m.user_id == user_id) {
            return Err(TeamError::AlreadyExists(format!(
                "User {} already in team",
                user_id
            )));
        }
        team.members.push(TeamMember {
            user_id: user_id.to_string(),
            role,
            joined_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
        let json = serde_json::to_vec(&team).map_err(|e| TeamError::Storage(e.to_string()))?;
        self.db
            .insert(Self::team_key(team_id), json)
            .map_err(|e| TeamError::Storage(e.to_string()))?;
        self.db
            .flush()
            .map_err(|e| TeamError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn remove_member(&self, team_id: &str, user_id: &str) -> Result<(), TeamError> {
        let mut team = self.get_team(team_id).await?;
        team.members.retain(|m| m.user_id != user_id);
        let json = serde_json::to_vec(&team).map_err(|e| TeamError::Storage(e.to_string()))?;
        self.db
            .insert(Self::team_key(team_id), json)
            .map_err(|e| TeamError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_team(&self, team_id: &str) -> Result<Team, TeamError> {
        let data = self
            .db
            .get(Self::team_key(team_id))
            .map_err(|e| TeamError::Storage(e.to_string()))?
            .ok_or_else(|| TeamError::NotFound(team_id.to_string()))?;
        serde_json::from_slice(&data).map_err(|e| TeamError::Storage(e.to_string()))
    }

    async fn list_teams(&self, user_id: &str) -> Result<Vec<Team>, TeamError> {
        let ids = self.user_team_ids(user_id);
        let mut teams = Vec::new();
        for id in ids {
            if let Ok(team) = self.get_team(&id).await {
                teams.push(team);
            }
        }
        Ok(teams)
    }

    async fn update_settings(
        &self,
        team_id: &str,
        settings: TeamSettings,
    ) -> Result<(), TeamError> {
        let mut team = self.get_team(team_id).await?;
        team.settings = settings;
        let json = serde_json::to_vec(&team).map_err(|e| TeamError::Storage(e.to_string()))?;
        self.db
            .insert(Self::team_key(team_id), json)
            .map_err(|e| TeamError::Storage(e.to_string()))?;
        self.db
            .flush()
            .map_err(|e| TeamError::Storage(e.to_string()))?;
        Ok(())
    }
}

impl SledTeamStore {
    fn user_team_ids(&self, user_id: &str) -> Vec<String> {
        self.db
            .get(Self::user_key(user_id))
            .ok()
            .flatten()
            .and_then(|data| serde_json::from_slice::<Vec<String>>(&data).ok())
            .unwrap_or_default()
    }
}
