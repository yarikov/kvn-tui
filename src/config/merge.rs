use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use serde_json::Value;

use super::profile::Config;

pub(crate) fn merge_configs(
    base: &Config,
    current: &Config,
    edited: &Config,
) -> Result<Config, Vec<String>> {
    for (label, config) in [("base", base), ("current", current), ("edited", edited)] {
        if let Err(error) = config.validate() {
            return Err(vec![format!("{label} config is invalid: {error}")]);
        }
    }
    let base = serde_json::to_value(base).expect("Config serialization cannot fail");
    let current = serde_json::to_value(current).expect("Config serialization cannot fail");
    let edited = serde_json::to_value(edited).expect("Config serialization cannot fail");
    let mut conflicts = Vec::new();
    let merged = merge_value(
        Some(&base),
        Some(&current),
        Some(&edited),
        "",
        &mut conflicts,
    );
    if !conflicts.is_empty() {
        return Err(conflicts);
    }
    let config: Config = serde_json::from_value(merged.expect("root config cannot be deleted"))
        .context("merged config is invalid")
        .map_err(|error| vec![error.to_string()])?;
    let mut config = config;
    if config
        .settings
        .last_connected_profile
        .is_some_and(|id| !config.profiles.iter().any(|profile| profile.id == id))
    {
        config.settings.last_connected_profile = None;
    }
    config
        .validate()
        .context("merged config failed validation")
        .map_err(|error| vec![error.to_string()])?;
    Ok(config)
}

fn merge_value(
    base: Option<&Value>,
    current: Option<&Value>,
    edited: Option<&Value>,
    path: &str,
    conflicts: &mut Vec<String>,
) -> Option<Value> {
    if edited == base {
        return current.cloned();
    }
    if current == base {
        return edited.cloned();
    }
    if current == edited {
        return current.cloned();
    }

    match (base, current, edited) {
        (Some(Value::Object(b)), Some(Value::Object(c)), Some(Value::Object(e))) => {
            let keys: BTreeSet<_> = b.keys().chain(c.keys()).chain(e.keys()).collect();
            let mut out = serde_json::Map::new();
            for key in keys {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if let Some(value) =
                    merge_value(b.get(key), c.get(key), e.get(key), &child_path, conflicts)
                {
                    out.insert(key.clone(), value);
                }
            }
            Some(Value::Object(out))
        }
        (Some(Value::Array(b)), Some(Value::Array(c)), Some(Value::Array(e)))
            if path == "profiles" || path == "subscriptions" =>
        {
            merge_uuid_array(b, c, e, path, conflicts)
        }
        _ => {
            conflicts.push(if path.is_empty() {
                "configuration".into()
            } else {
                path.into()
            });
            current.cloned()
        }
    }
}

fn merge_uuid_array(
    base: &[Value],
    current: &[Value],
    edited: &[Value],
    path: &str,
    conflicts: &mut Vec<String>,
) -> Option<Value> {
    fn index(values: &[Value]) -> HashMap<String, &Value> {
        values
            .iter()
            .filter_map(|value| {
                value
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_owned(), value))
            })
            .collect()
    }
    let b = index(base);
    let c = index(current);
    let e = index(edited);
    let id_order = |values: &[Value]| {
        values
            .iter()
            .filter_map(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let base_order = id_order(base);
    let current_order = id_order(current);
    let edited_order = id_order(edited);
    let current_reordered = common_order_changed(&base_order, &current_order);
    let edited_reordered = common_order_changed(&base_order, &edited_order);
    if current_reordered && edited_reordered && current_order != edited_order {
        conflicts.push(format!("{path} order"));
    }
    let mut ids = Vec::new();
    let first = if edited_reordered { edited } else { current };
    let second = if edited_reordered { current } else { edited };
    for value in first.iter().chain(second) {
        if let Some(id) = value.get("id").and_then(Value::as_str)
            && !ids.iter().any(|known| known == id)
        {
            ids.push(id.to_owned());
        }
    }
    let mut out = Vec::new();
    for id in ids {
        if let Some(value) = merge_value(
            b.get(&id).copied(),
            c.get(&id).copied(),
            e.get(&id).copied(),
            &format!("{path}[{id}]"),
            conflicts,
        ) {
            out.push(value);
        }
    }
    Some(Value::Array(out))
}

fn common_order_changed(base: &[String], side: &[String]) -> bool {
    let common: Vec<_> = base.iter().filter(|id| side.contains(id)).collect();
    let side_common: Vec<_> = side.iter().filter(|id| base.contains(id)).collect();
    common != side_common
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::Profile;

    #[test]
    fn merges_disjoint_profile_and_setting_changes() {
        let mut base = Config::default();
        base.profiles.push(Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        ));
        let mut current = base.clone();
        current.settings.auto_connect = true;
        let mut edited = base.clone();
        edited.profiles[0].name = "Edited".into();
        let merged = merge_configs(&base, &current, &edited).unwrap();
        assert!(merged.settings.auto_connect);
        assert_eq!(merged.profiles[0].name, "Edited");
    }

    #[test]
    fn reports_same_field_conflict() {
        let base = Config::default();
        let mut current = base.clone();
        current.settings.theme = "nord".into();
        let mut edited = base.clone();
        edited.settings.theme = "catppuccin".into();
        assert_eq!(
            merge_configs(&base, &current, &edited).unwrap_err(),
            vec!["settings.theme"]
        );
    }

    #[test]
    fn merges_independent_additions_and_deletions_by_uuid() {
        let mut base = Config::default();
        let removed = Profile::new_vless(
            "Removed".into(),
            "1.1.1.1".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        );
        base.profiles.push(removed);
        let mut current = base.clone();
        current.profiles.push(Profile::new_vless(
            "Daemon".into(),
            "2.2.2.2".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        ));
        let mut edited = base.clone();
        edited.profiles.clear();
        edited.profiles.push(Profile::new_vless(
            "Editor".into(),
            "3.3.3.3".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        ));

        let merged = merge_configs(&base, &current, &edited).unwrap();
        let names: Vec<_> = merged
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect();
        assert_eq!(names, vec!["Daemon", "Editor"]);
    }

    #[test]
    fn modification_conflicts_with_concurrent_deletion() {
        let mut base = Config::default();
        base.profiles.push(Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        ));
        let mut current = base.clone();
        current.profiles.clear();
        let mut edited = base.clone();
        edited.profiles[0].name = "Edited".into();

        let conflicts = merge_configs(&base, &current, &edited).unwrap_err();
        assert!(conflicts[0].starts_with("profiles["));
    }

    #[test]
    fn rejects_duplicate_ids_before_indexing() {
        let mut base = Config::default();
        let profile = Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        );
        base.profiles = vec![profile.clone(), profile];

        let errors = merge_configs(&base, &Config::default(), &Config::default()).unwrap_err();
        assert!(errors[0].contains("base config is invalid"));
        assert!(errors[0].contains("duplicate id"));
    }

    #[test]
    fn preserves_editor_reorder_during_concurrent_field_change() {
        let mut base = Config::default();
        let a = Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        );
        let b = Profile::new_vless(
            "B".into(),
            "2.2.2.2".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        );
        base.profiles = vec![a, b];
        let mut current = base.clone();
        current.settings.auto_connect = true;
        let mut edited = base.clone();
        edited.profiles.reverse();
        let merged = merge_configs(&base, &current, &edited).unwrap();
        assert_eq!(merged.profiles[0].name, "B");
        assert!(merged.settings.auto_connect);
    }

    #[test]
    fn conflicting_reorders_are_reported() {
        let mut base = Config::default();
        for name in ["A", "B", "C"] {
            base.profiles.push(Profile::new_vless(
                name.into(),
                "1.1.1.1".into(),
                443,
                uuid::Uuid::new_v4().to_string(),
            ));
        }
        let mut current = base.clone();
        current.profiles.swap(0, 1);
        let mut edited = base.clone();
        edited.profiles.swap(1, 2);
        assert!(
            merge_configs(&base, &current, &edited)
                .unwrap_err()
                .contains(&"profiles order".to_string())
        );
    }

    #[test]
    fn clears_dangling_last_connected_profile_after_merge() {
        let mut base = Config::default();
        let profile = Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            uuid::Uuid::new_v4().to_string(),
        );
        base.settings.last_connected_profile = Some(profile.id);
        base.profiles.push(profile);
        let mut edited = base.clone();
        edited.profiles.clear();
        let merged = merge_configs(&base, &base, &edited).unwrap();
        assert_eq!(merged.settings.last_connected_profile, None);
    }
}
