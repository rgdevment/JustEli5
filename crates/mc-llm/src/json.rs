use serde_json::{Deserializer, Value};

/// The first JSON object or array in a model answer, skipping prose and code
/// fences around it.
pub fn extract_json(text: &str) -> Option<&str> {
    text.match_indices(['{', '[']).find_map(|(start, _)| {
        let mut values = Deserializer::from_str(&text[start..]).into_iter::<Value>();
        let value = values.next()?.ok()?;
        (value.is_object() || value.is_array()).then(|| &text[start..start + values.byte_offset()])
    })
}
