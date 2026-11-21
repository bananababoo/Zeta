//! Structural validation of AI DM responses.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 4. Only structure is checked: the
//! schema, that every identifier exists in the Snapshot, and that every coordinate is inside
//! the map. Every message is written for the AI DM to read and correct on retry, so it names
//! the exact field and the allowed values.

use std::fmt;

use thiserror::Error;

use crate::grid::MapBounds;
use crate::resolution::{AiResolution, Branch, LocalId, WorldEffect};
use crate::snapshot::Snapshot;

/// Where in a response an effect sits, written the way the AI DM sees it, such as `fx[2]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectLocation {
    /// The list holding the effect.
    pub branch: Branch,
    /// Position in that list, from 0.
    pub index: usize,
}

impl fmt::Display for EffectLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = match self.branch {
            Branch::Success => "fx",
            Branch::Failure => "fail",
        };
        write!(formatter, "{list}[{}]", self.index)
    }
}

/// 1 structural problem in an AI DM response.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// The text is not JSON matching the AI Resolution schema.
    #[error("the response does not match the schema: {message}")]
    Schema {
        /// The parser's description of the problem.
        message: String,
    },
    /// An effect names an identifier that is not in the Snapshot.
    #[error(
        "{location} ({op}): id {id} is not in the Snapshot; valid ids are 0 to {highest_valid_id}"
    )]
    UnknownId {
        /// The effect.
        location: EffectLocation,
        /// The effect's `op` tag.
        op: &'static str,
        /// The identifier that does not exist.
        id: LocalId,
        /// The highest identifier in the Snapshot.
        highest_valid_id: usize,
    },
    /// An effect places something outside the map.
    #[error(
        "{location} ({op}): square ({x}, {y}) is outside the map; x must be below {width} and y below {height}"
    )]
    OutOfBounds {
        /// The effect.
        location: EffectLocation,
        /// The effect's `op` tag.
        op: &'static str,
        /// Requested column.
        x: u32,
        /// Requested row.
        y: u32,
        /// Map width in squares.
        width: u32,
        /// Map height in squares.
        height: u32,
    },
    /// `fail` holds effects although `chk` is null.
    #[error("fail must be empty when chk is null, because nothing can fail without a check")]
    FailWithoutCheck,
}

/// Every problem found in 1 response, in the order they appear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors(pub Vec<ValidationError>);

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (number, error) in self.0.iter().enumerate() {
            if number > 0 {
                writeln!(formatter)?;
            }
            write!(formatter, "- {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// Parses AI DM output and checks its structure against the request's Snapshot and map.
///
/// # Errors
///
/// Returns every problem found. A schema problem stops parsing, so it is reported alone.
pub fn parse_and_validate(
    text: &str,
    snapshot: &Snapshot,
    bounds: MapBounds,
) -> Result<AiResolution, ValidationErrors> {
    let resolution: AiResolution = serde_json::from_str(text).map_err(|error| {
        ValidationErrors(vec![ValidationError::Schema {
            message: error.to_string(),
        }])
    })?;
    validate(&resolution, snapshot, bounds)?;
    Ok(resolution)
}

/// Checks the structure of an already parsed resolution.
///
/// # Errors
///
/// Returns every problem found, in the order they appear in the response.
pub fn validate(
    resolution: &AiResolution,
    snapshot: &Snapshot,
    bounds: MapBounds,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    if resolution.chk.is_none() && !resolution.fail.is_empty() {
        errors.push(ValidationError::FailWithoutCheck);
    }
    for branch in [Branch::Success, Branch::Failure] {
        for (index, effect) in resolution.effects(branch).iter().enumerate() {
            let location = EffectLocation { branch, index };
            check_effect(effect, location, snapshot, bounds, &mut errors);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errors))
    }
}

fn check_effect(
    effect: &WorldEffect,
    location: EffectLocation,
    snapshot: &Snapshot,
    bounds: MapBounds,
    errors: &mut Vec<ValidationError>,
) {
    for &id in effect.referenced_ids() {
        if !snapshot.contains(id) {
            errors.push(ValidationError::UnknownId {
                location,
                op: effect.op(),
                id,
                highest_valid_id: snapshot.len().saturating_sub(1),
            });
        }
    }
    if let Some((x, y)) = effect.target_square()
        && !bounds.contains(x, y)
    {
        errors.push(ValidationError::OutOfBounds {
            location,
            op: effect.op(),
            x,
            y,
            width: bounds.width,
            height: bounds.height,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::tests::entity;
    use crate::snapshot::{PerceptionSettings, Snapshot};

    const BOUNDS: MapBounds = MapBounds {
        width: 20,
        height: 10,
    };

    /// A Snapshot with local identifiers 0, 1, and 2.
    fn snapshot() -> Snapshot {
        Snapshot::build(
            entity(10, 0, 0),
            [entity(11, 1, 0), entity(12, 2, 0)],
            |_| false,
            PerceptionSettings {
                hearing_range_squares: 5,
                max_count: 10,
            },
        )
    }

    fn errors(text: &str) -> Vec<ValidationError> {
        parse_and_validate(text, &snapshot(), BOUNDS)
            .expect_err("response should be rejected")
            .0
    }

    #[test]
    fn valid_response_is_accepted() {
        let text = r#"{
            "ok": true,
            "chk": {"dc": 12, "reason": "the guard is alert"},
            "fx": [{"op": "move", "id": 0, "x": 19, "y": 9, "style": "sneaks"}],
            "fail": [{"op": "speak", "id": 2, "text": "Halt!", "vol": "shout"}]
        }"#;
        assert!(parse_and_validate(text, &snapshot(), BOUNDS).is_ok());
    }

    #[test]
    fn malformed_json_is_a_schema_error() {
        assert!(matches!(
            errors("{\"ok\": tru").as_slice(),
            [ValidationError::Schema { .. }]
        ));
    }

    #[test]
    fn every_unknown_id_and_bad_square_is_reported_with_its_location() {
        let text = r#"{
            "ok": true,
            "chk": {"dc": 10, "reason": "test"},
            "fx": [
                {"op": "narrate", "text": "fine"},
                {"op": "history", "ids": [1, 7], "summary": "s", "preserved": false}
            ],
            "fail": [{"op": "spawn", "name": "Smoke", "x": 20, "y": 3, "passable": true, "tags": [], "visual": "smoke", "settle": null}]
        }"#;
        let messages: Vec<String> = errors(text).iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "fx[1] (history): id 7 is not in the Snapshot; valid ids are 0 to 2",
                "fail[0] (spawn): square (20, 3) is outside the map; x must be below 20 and y below 10",
            ]
        );
    }

    #[test]
    fn fail_effects_without_a_check_are_rejected() {
        let text =
            r#"{"ok": true, "chk": null, "fx": [], "fail": [{"op": "narrate", "text": "x"}]}"#;
        assert_eq!(errors(text), [ValidationError::FailWithoutCheck]);
    }

    #[test]
    fn errors_join_into_one_retry_message() {
        let joined = ValidationErrors(vec![
            ValidationError::FailWithoutCheck,
            ValidationError::Schema {
                message: "m".into(),
            },
        ])
        .to_string();
        assert_eq!(joined.lines().count(), 2);
        assert!(joined.lines().all(|line| line.starts_with("- ")));
    }
}
