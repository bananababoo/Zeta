//! Answers for the AI DM's `inspect` tool that do not need the live world.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 2. Native tool calling is not built
//! yet; the server will route `inspect` catalog queries here when it is.

use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::item_rules::entry_for_ai_dm;
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
}
