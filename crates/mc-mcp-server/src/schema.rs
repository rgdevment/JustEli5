//! The small subset of JSON Schema the tool inputs use.

use serde_json::Value;

/// Checks `value` against `schema`: type, required, properties, additionalProperties,
/// minimum/maximum and minLength/maxLength.
pub fn validate(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    let what = if path.is_empty() { "arguments" } else { path };
    match schema["type"].as_str() {
        Some("object") => {
            let Some(object) = value.as_object() else {
                return Err(format!("{what} must be an object"));
            };
            for key in schema["required"].as_array().into_iter().flatten() {
                let key = key.as_str().unwrap_or_default();
                if !object.contains_key(key) {
                    return Err(format!("missing required argument `{key}`"));
                }
            }
            let properties = &schema["properties"];
            for (key, item) in object {
                match properties.get(key) {
                    Some(sub) => validate(item, sub, key)?,
                    None if schema["additionalProperties"] == Value::Bool(false) => {
                        return Err(format!("unknown argument `{key}`"));
                    }
                    None => {}
                }
            }
            Ok(())
        }
        Some("string") => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("`{what}` must be a string"))?;
            let length = text.chars().count() as u64;
            if schema["minLength"].as_u64().is_some_and(|min| length < min) {
                return Err(format!("`{what}` is too short"));
            }
            if schema["maxLength"].as_u64().is_some_and(|max| length > max) {
                return Err(format!("`{what}` is too long"));
            }
            Ok(())
        }
        Some("integer") => {
            let number = value
                .as_i64()
                .ok_or_else(|| format!("`{what}` must be an integer"))?;
            if schema["minimum"].as_i64().is_some_and(|min| number < min) {
                return Err(format!("`{what}` is below the minimum"));
            }
            if schema["maximum"].as_i64().is_some_and(|max| number > max) {
                return Err(format!("`{what}` is above the maximum"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn checks_types_bounds_and_unknown_keys() {
        let schema = json!({
            "type": "object",
            "properties": {
                "text": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1, "maximum": 5},
            },
            "required": ["text"],
            "additionalProperties": false,
        });
        assert!(validate(&json!({"text": "a", "limit": 3}), &schema, "").is_ok());
        assert!(validate(&json!({}), &schema, "").is_err());
        assert!(validate(&json!({"text": ""}), &schema, "").is_err());
        assert!(validate(&json!({"text": "a", "limit": 9}), &schema, "").is_err());
        assert!(validate(&json!({"text": "a", "limit": "3"}), &schema, "").is_err());
        assert!(validate(&json!({"text": "a", "extra": 1}), &schema, "").is_err());
        assert!(validate(&json!(7), &schema, "").is_err());
    }
}
