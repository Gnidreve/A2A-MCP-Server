//! Load/save small JSON state files, mirroring the original `persistence_utils.py`.

use std::path::Path;

use serde::{Serialize, de::DeserializeOwned};

/// Used starting Phase 2, once tools mutate the registry/task mapping at runtime.
#[allow(dead_code)]
pub fn save_to_json<T: Serialize>(data: &T, path: impl AsRef<Path>) -> anyhow::Result<()> {
    let path = path.as_ref();
    let json = serde_json::to_string_pretty(data)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Loads `path` as JSON, deserializing to `T`. Returns `T::default()` if the
/// file does not exist yet (first run), matching the Python original's behavior.
pub fn load_from_json<T: DeserializeOwned + Default>(path: impl AsRef<Path>) -> T {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "failed to parse JSON, starting fresh");
            T::default()
        }),
        Err(_) => T::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn round_trips_a_map() {
        let dir = tempdir();
        let path = dir.join("data.json");

        let mut data = HashMap::new();
        data.insert("key".to_string(), "value".to_string());
        save_to_json(&data, &path).unwrap();

        let loaded: HashMap<String, String> = load_from_json(&path);
        assert_eq!(loaded, data);
    }

    #[test]
    fn missing_file_yields_default() {
        let dir = tempdir();
        let path = dir.join("does-not-exist.json");

        let loaded: HashMap<String, String> = load_from_json(&path);
        assert!(loaded.is_empty());
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("a2a-mcp-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uuid_like() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
