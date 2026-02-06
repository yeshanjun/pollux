use serde::{Deserialize, Serialize};

/// Newtype wrapper around `u64`.
/// Runtime representation is identical to `u64`, but the type encodes intent:
/// a bitset of model capabilities (each bit corresponds to a model index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[repr(transparent)] // ABI-compatible with u64 for FFI or direct passthrough.
pub struct ModelCapabilities(u64);

impl ModelCapabilities {
    /// Creates an empty set (no capabilities enabled).
    #[inline(always)]
    pub fn none() -> Self {
        Self(0)
    }

    /// Creates a full set (all bits enabled), often used as a default.
    #[inline(always)]
    pub fn all() -> Self {
        Self(u64::MAX) // Use ((1 << n) - 1) to bound by the actual model count.
    }

    /// Builds from raw bits (e.g., loaded from storage).
    #[inline(always)]
    pub fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the raw bitset (e.g., for persistence or interop).
    #[inline(always)]
    pub fn bits(&self) -> u64 {
        self.0
    }

    // ================= Bitset helpers =================

    /// Checks whether a specific model index is enabled.
    /// Semantics: `caps.supports(index)`.
    #[inline(always)]
    pub fn supports(&self, index: usize) -> bool {
        (self.0 & (1u64 << index)) != 0
    }

    /// Enables the bit for a given model index.
    #[inline(always)]
    pub fn enable(&mut self, index: usize) {
        self.0 |= 1u64 << index;
    }

    /// Clears the bit for a given model index.
    #[inline(always)]
    pub fn disable(&mut self, index: usize) {
        self.0 &= !(1u64 << index);
    }

    /// Clears bits for all models included in the given bitmask.
    /// This is useful when the caller naturally has a model mask instead of an index.
    #[inline(always)]
    pub fn disable_mask(&mut self, mask: u64) {
        self.0 &= !mask;
    }

    /// Returns true if `self` is a superset of `required`.
    /// Example: a request needs [GPT4 + Stream], so the provider must contain both.
    #[inline(always)]
    pub fn contains_all(&self, required: ModelCapabilities) -> bool {
        (self.0 & required.0) == required.0
    }

    /// Returns true if there is any overlap between the two sets.
    /// Example: supporting any one of several fallback models is sufficient.
    #[inline(always)]
    pub fn intersects(&self, other: ModelCapabilities) -> bool {
        (self.0 & other.0) != 0
    }

    /// Returns the union of two capability sets.
    #[inline(always)]
    pub fn merge(&self, other: ModelCapabilities) -> Self {
        Self(self.0 | other.0)
    }
}

// Enable direct use of bitwise operators (e.g., caps_a | caps_b).
use std::ops::{BitAnd, BitOr};

impl BitOr for ModelCapabilities {
    type Output = Self;
    #[inline(always)]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitAnd for ModelCapabilities {
    type Output = Self;
    #[inline(always)]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}
