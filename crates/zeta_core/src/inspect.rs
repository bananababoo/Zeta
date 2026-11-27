//! Answers for the AI DM's `inspect` tool that do not need the live world.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 2 and
//! `docs/architecture/entity_properties.md` Section 2. Native tool calling routes catalog
//! and ruleset schema lookups here so the AI DM gets complete, consistent SRD 5.2.1 meaning.

use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::item_rules::entry_for_ai_dm;
use crate::ruleset::{Ruleset, RulesetDomain};
use crate::settings::CATALOG_QUERY_MAX_RESULTS;

/// Looks up item catalog entries by name or identifier through [`Catalog::search`]. Returns
/// `{"query", "total", "items": [entries]}` with entries from [`entry_for_ai_dm`], best
/// matches first, at most [`CATALOG_QUERY_MAX_RESULTS`] entries.
pub fn catalog_query(catalog: &Catalog, query: &str) -> Value {
    let matches = catalog.search(query);
    let items: Vec<Value> = matches
        .iter()
        .take(CATALOG_QUERY_MAX_RESULTS)
        .map(|item| entry_for_ai_dm(item))
        .collect();
    json!({
        "query": query,
        "total": matches.len(),
        "items": items,
    })
}

/// Looks up SRD 5.2.1 ruleset entries or Zeta ECS property component schemas by optional
/// `domain` (`"class"`, `"species"`, `"condition"`, `"component"`, etc.) and `query`.
///
/// Returns `{"domain", "query", "total", "entries": [{domain, id, name, summary, rules, data?}]}`
/// capped at [`CATALOG_QUERY_MAX_RESULTS`] results, or an error object listing valid domains
/// if `domain` is unrecognized.
pub fn ruleset_query(ruleset: &Ruleset, domain: Option<&str>, query: &str) -> Value {
    let parsed_domain = match domain {
        Some(name) => {
            if let Some(dom) = RulesetDomain::from_name(name) {
                Some(dom)
            } else {
                let valid: Vec<&str> = RulesetDomain::ALL.iter().map(|d| d.as_str()).collect();
                return json!({
                    "domain": name,
                    "query": query,
                    "error": format!("unknown ruleset domain \"{name}\""),
                    "valid_domains": valid,
                });
            }
        }
        None => None,
    };

    let matches = ruleset.search(parsed_domain, query);
    let entries: Vec<Value> = matches
        .iter()
        .take(CATALOG_QUERY_MAX_RESULTS)
        .map(|(dom, entry)| {
            let mut obj = json!({
                "domain": dom.as_str(),
                "id": entry.id,
                "name": entry.name,
                "summary": entry.summary,
                "rules": entry.rules,
            });
            if let Some(data) = &entry.data {
                obj["data"] = data.clone();
            }
            obj
        })
        .collect();

    json!({
        "domain": parsed_domain.map(RulesetDomain::as_str),
        "query": query,
        "total": matches.len(),
        "entries": entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_names_come_first_with_full_entries() {
        let answer = catalog_query(Catalog::srd(), "rope");
        assert_eq!(answer["items"][0]["id"], "rope");
        assert_eq!(answer["items"][0]["category"], "adventuring_gear");
        assert_eq!(answer["query"], "rope");
        let tools = catalog_query(Catalog::srd(), "painter's supplies");
        assert_eq!(tools["items"][0]["id"], "painters_supplies");
        assert!(tools["items"][0].get("crafts").is_none());
    }

    #[test]
    fn results_are_capped_but_the_total_is_not() {
        let answer = catalog_query(Catalog::srd(), "s");
        let total = answer["total"].as_u64().expect("number");
        let returned = answer["items"].as_array().expect("array").len();
        assert!(total > CATALOG_QUERY_MAX_RESULTS as u64, "{total}");
        assert_eq!(returned, CATALOG_QUERY_MAX_RESULTS);
    }

    #[test]
    fn unknown_and_empty_queries_return_no_items() {
        for query in ["", "lightsaber"] {
            let answer = catalog_query(Catalog::srd(), query);
            assert_eq!(answer["total"], 0, "{query}");
        }
    }

    #[test]
    fn ruleset_query_returns_full_class_condition_and_component_definitions() {
        let fighter = ruleset_query(Ruleset::srd(), Some("class"), "fighter");
        assert_eq!(fighter["entries"][0]["id"], "fighter");
        assert_eq!(fighter["entries"][0]["data"]["hit_die"], 10);

        let exhaustion = ruleset_query(Ruleset::srd(), Some("condition"), "exhaustion");
        assert_eq!(exhaustion["entries"][0]["id"], "exhaustion");
        assert!(
            exhaustion["entries"][0]["rules"]
                .as_str()
                .expect("rules")
                .contains("2 * level")
        );

        let health_schema = ruleset_query(Ruleset::srd(), Some("component"), "health");
        assert_eq!(health_schema["entries"][0]["id"], "health");
        assert!(health_schema["entries"][0]["data"]["fields"].is_array());

        let bad_domain = ruleset_query(Ruleset::srd(), Some("homebrew"), "foo");
        assert!(bad_domain["error"].is_string());
        assert!(bad_domain["valid_domains"].is_array());
    }
}
