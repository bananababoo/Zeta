//! d20 rolls for checks: operating-system randomness, with a fixed source for tests.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 3 (the server rolls the visible d20)
//! and `docs/architecture/code_style.md` Section 11: `getrandom` is a pre-1.0 dependency, so
//! this is the only module that names it, and its error type never leaves this module.

use thiserror::Error;
use zeta_core::resolution::D20;

/// Byte values at or above this are rerolled, so every face keeps the same chance:
/// 240 is the largest multiple of 20 that fits in a byte.
const UNBIASED_BYTE_LIMIT: u8 = 240;

/// Bytes requested from the operating system at once; rerolls draw from the same batch.
const BYTES_PER_REQUEST: usize = 16;

/// The operating system could not provide random bytes.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("the operating system could not provide random bytes: {message}")]
pub struct DiceError {
    /// The operating system's explanation.
    message: String,
}

/// Something that rolls a d20.
pub trait RollD20: Send + Sync {
    /// Rolls 1 d20.
    ///
    /// # Errors
    ///
    /// Returns [`DiceError`] if no randomness is available.
    fn roll(&mut self) -> Result<D20, DiceError>;
}

/// Rolls with the operating system's random number source.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsDice;

impl RollD20 for OsDice {
    fn roll(&mut self) -> Result<D20, DiceError> {
        loop {
            let mut bytes = [0_u8; BYTES_PER_REQUEST];
            getrandom::fill(&mut bytes).map_err(|error| DiceError {
                message: error.to_string(),
            })?;
            if let Some(face) = bytes.into_iter().find_map(face_from_byte) {
                return Ok(face);
            }
        }
    }
}

/// Maps 1 random byte to a face, or `None` when the byte must be rerolled.
fn face_from_byte(byte: u8) -> Option<D20> {
    if byte >= UNBIASED_BYTE_LIMIT {
        return None;
    }
    // `byte % 20 + 1` is between 1 and 20, which `D20::new` always accepts.
    D20::new(byte % 20 + 1).ok()
}

/// Rolls a fixed sequence of faces, starting over at the end. For tests and recorded replays.
#[derive(Clone, Debug)]
pub struct FixedDice {
    faces: Vec<D20>,
    next: usize,
}

impl FixedDice {
    /// Dice that roll `faces` in order, then repeat. An empty list fails every roll.
    pub fn new(faces: Vec<D20>) -> Self {
        Self { faces, next: 0 }
    }
}

impl RollD20 for FixedDice {
    fn roll(&mut self) -> Result<D20, DiceError> {
        let face = self
            .faces
            .get(self.next)
            .copied()
            .ok_or_else(|| DiceError {
                message: "the fixed dice hold no faces".into(),
            })?;
        self.next = (self.next + 1) % self.faces.len();
        Ok(face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d20(face: u8) -> D20 {
        D20::new(face).expect("valid face")
    }

    #[test]
    fn every_kept_byte_maps_to_a_face_and_each_face_equally_often() {
        let mut counts = [0_u32; 20];
        for byte in 0..UNBIASED_BYTE_LIMIT {
            let face = face_from_byte(byte).expect("kept byte");
            counts[usize::from(face.face() - 1)] += 1;
        }
        assert!(counts.iter().all(|&count| count == 12), "{counts:?}");
    }

    #[test]
    fn bytes_above_the_limit_are_rerolled() {
        for byte in UNBIASED_BYTE_LIMIT..=u8::MAX {
            assert_eq!(face_from_byte(byte), None);
        }
    }

    #[test]
    fn operating_system_dice_stay_in_range() {
        let mut dice = OsDice;
        for _ in 0..200 {
            let face = dice.roll().expect("randomness available").face();
            assert!((1..=20).contains(&face));
        }
    }

    #[test]
    fn fixed_dice_repeat_their_faces() {
        let mut dice = FixedDice::new(vec![d20(3), d20(17)]);
        let rolls: Vec<u8> = (0..5).map(|_| dice.roll().expect("fixed").face()).collect();
        assert_eq!(rolls, [3, 17, 3, 17, 3]);
        let mut empty = FixedDice::new(vec![]);
        assert!(empty.roll().is_err());
    }
}
