//! Tag rules: `--tag 'Tag1,Tag2=query; Tag3=query'`, a YAML file (`--tags-config`) and
//! `[[tags.rules]]` in the TOML configuration.

use std::path::Path;

use serde::Deserialize;

use crate::settings::TagRule;

fn tags_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn rule(query: &str, tags: Vec<String>) -> Result<TagRule, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("a tag rule needs a search query".into());
    }
    if tags.is_empty() {
        return Err(format!("the rule for {query:?} has no tags"));
    }
    Ok(TagRule {
        query: query.to_string(),
        tags,
    })
}

/// `Tag1,Tag2=query; Tag3=query`.
pub(crate) fn parse_spec(spec: &str) -> Result<Vec<TagRule>, String> {
    spec.split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|entry| {
            let (tags, query) = entry
                .split_once('=')
                .ok_or_else(|| format!("{entry:?}: expected Tag1,Tag2=search query"))?;
            rule(query, tags_list(tags))
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Tags {
    One(String),
    Many(Vec<String>),
}

impl Tags {
    fn into_list(self) -> Vec<String> {
        match self {
            Tags::One(s) => tags_list(&s),
            Tags::Many(v) => v.iter().flat_map(|t| tags_list(t)).collect(),
        }
    }
}

#[derive(Deserialize)]
struct Filter {
    #[serde(rename = "match")]
    query: String,
    tags: Tags,
}

#[derive(Deserialize)]
struct YamlFile {
    #[serde(default)]
    filters: Vec<Filter>,
}

/// `filters: [{match: query, tags: "a, b"}]` (tags may also be a list).
pub(crate) fn parse_yaml(text: &str) -> Result<Vec<TagRule>, String> {
    let file: YamlFile = serde_saphyr::from_str(text).map_err(|e| e.to_string())?;
    file.filters
        .into_iter()
        .map(|f| rule(&f.query, f.tags.into_list()))
        .collect()
}

pub(crate) fn read_yaml(path: &Path) -> Result<Vec<TagRule>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_yaml(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_spec() {
        let rules =
            parse_spec("Invoice, Finance = subject:invoice ;Big=larger:1m;").expect("parse");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].tags, ["Invoice", "Finance"]);
        assert_eq!(rules[0].query, "subject:invoice");
        assert!(parse_spec("no-equals").is_err());
        assert!(parse_spec("=subject:x").is_err());
        assert!(parse_spec("A=  ").is_err());
    }

    #[test]
    fn yaml_file() {
        let yaml = "filters:\n  - match: subject:invoice\n    tags: Invoice, Finance\n  - match: from:shop@example.ir\n    tags: [فروشگاه, Orders]\n";
        let rules = parse_yaml(yaml).expect("yaml");
        assert_eq!(rules[0].tags, ["Invoice", "Finance"]);
        assert_eq!(rules[1].tags, ["فروشگاه", "Orders"]);
        assert!(
            parse_yaml("filters:\n  - match: x\n").is_err(),
            "tags are required"
        );
        assert!(parse_yaml("filters: [").is_err());
    }
}
