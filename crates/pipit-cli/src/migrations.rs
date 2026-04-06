//! Versioned session schema with forward migration.
//!
//! Each session file carries a `schema_version` field. On load, migrations
//! are applied in order until the current version is reached.
//! Migration cost: O(k) where k = version_current - version_file.

use serde_json::Value;

/// Current schema version. Bump this when session format changes.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// A single migration step transforming schema version `from` to `from + 1`.
struct Migration {
    from: u32,
    transform: fn(Value) -> Result<Value, String>,
}

/// Ordered list of migrations. Each entry advances the schema by one version.
const MIGRATIONS: &[Migration] = &[
    // Future migrations go here, e.g.:
    // Migration { from: 1, transform: v1_to_v2 },
];

/// Migrate a session JSON value from its embedded version to `CURRENT_SCHEMA_VERSION`.
/// If no `schema_version` field is present, assumes version 0 and injects the field.
pub fn migrate_session(mut value: Value) -> Result<Value, String> {
    let version = value
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    if version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "Session schema version {} is newer than supported version {}. \
             Please upgrade pipit.",
            version, CURRENT_SCHEMA_VERSION
        ));
    }

    let mut current = version;
    for migration in MIGRATIONS {
        if migration.from == current {
            value = (migration.transform)(value)?;
            current += 1;
        }
    }

    // Stamp with current version after all migrations applied.
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "schema_version".to_string(),
            Value::Number(CURRENT_SCHEMA_VERSION.into()),
        );
    }

    Ok(value)
}

/// Inject a schema_version field into a freshly-created session value.
pub fn stamp_version(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "schema_version".to_string(),
            Value::Number(CURRENT_SCHEMA_VERSION.into()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migrate_no_version() {
        let input = serde_json::json!({"messages": []});
        let output = migrate_session(input).unwrap();
        assert_eq!(
            output.get("schema_version").unwrap().as_u64().unwrap(),
            CURRENT_SCHEMA_VERSION as u64
        );
    }

    #[test]
    fn test_migrate_current_version() {
        let input = serde_json::json!({"schema_version": CURRENT_SCHEMA_VERSION, "messages": []});
        let output = migrate_session(input).unwrap();
        assert_eq!(
            output.get("schema_version").unwrap().as_u64().unwrap(),
            CURRENT_SCHEMA_VERSION as u64
        );
    }

    #[test]
    fn test_migrate_future_version_error() {
        let input = serde_json::json!({"schema_version": 999, "messages": []});
        assert!(migrate_session(input).is_err());
    }
}
