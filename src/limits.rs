//! Layout ops budget helpers.
//!
//! These mirror `read_fonts::limits::max_ops` and
//! `read_fonts::limits::MAX_OPS_EXCEEDED`, which were added to fontations after
//! the release we depend on.
//!
//! HarfBuzz bounds how much of a layout table's offset graph a walk may read,
//! relative to the size of the whole table, and rejects a table that exceeds
//! it. See <https://github.com/harfbuzz/harfbuzz/blob/main/src/hb-sanitize.hh>.

use read_fonts::ReadError;

/// Matches HarfBuzz's `HB_SANITIZE_MAX_OPS_FACTOR`.
const MAX_OPS_FACTOR: usize = 64;

/// Matches HarfBuzz's `HB_SANITIZE_MAX_OPS_MIN`.
const MAX_OPS_MIN: usize = 16384;

/// Matches HarfBuzz's `HB_SANITIZE_MAX_OPS_MAX`.
const MAX_OPS_MAX: usize = 0x3FFFFFFF;

/// The number of bytes one walk of a table's offset graph may read, for a
/// table of `table_len` bytes.
///
/// HarfBuzz sizes this from the whole table it sanitizes, such as GSUB or
/// GPOS, so `table_len` should be the length of that table rather than of one
/// of its subtables.
pub(crate) fn max_ops(table_len: usize) -> usize {
    table_len
        .saturating_mul(MAX_OPS_FACTOR)
        .clamp(MAX_OPS_MIN, MAX_OPS_MAX)
}

/// The error of a walk of a layout table's offset graph that would read more
/// than [`max_ops`] allows.
pub(crate) const MAX_OPS_EXCEEDED: ReadError =
    ReadError::MalformedData("layout tables share too much data");
