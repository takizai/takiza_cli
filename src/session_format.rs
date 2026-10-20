//! Shared session wire format; older CLI histories used externally tagged enums.
use serde::{
    de::{DeserializeOwned, Error},
    Deserialize, Deserializer,
};
use serde_json::{json, Value};

pub fn deserialize_history<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| {
            let canonical = match value.as_object() {
                Some(object) if !object.contains_key("type") && object.len() == 1 => {
                    let (kind, payload) = object.iter().next().unwrap();
                    json!({"type":kind,"payload":payload})
                }
                _ => value,
            };
            serde_json::from_value(canonical).map_err(D::Error::custom)
        })
        .collect()
}
