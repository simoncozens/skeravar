//! Shared horizontal and vertical metrics subsetting.

use crate::{
    serialize::{SerializeErrorFlags, Serializer},
    Plan, SubsetError,
};
use read_fonts::{
    tables::hmtx::GlyphMetricRecords,
    types::{GlyphId, Tag},
};
use skrifa::raw::tables::mvar::tags;
use write_fonts::FontBuilder;

/// Which metrics table is being subset, used to pick the instancing maps,
/// `MVAR` value tags and side-bearing field names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetricsDirection {
    Horizontal,
    Vertical,
}

pub(crate) fn error(s: &mut Serializer, tag: Tag, flag: SerializeErrorFlags) -> SubsetError {
    s.set_err(flag);
    SubsetError::SubsetTableError(tag)
}

/// Byte offsets of the header fields we may rewrite while instancing. The
/// `hhea` and `vhea` layouts agree on all of them.
mod header_offsets {
    pub(crate) const ADVANCE_MAX: usize = 10;
    pub(crate) const MIN_SIDE_BEARING: usize = 12;
    pub(crate) const MIN_OTHER_SIDE_BEARING: usize = 14;
    pub(crate) const MAX_EXTENT: usize = 16;
    pub(crate) const CARET_SLOPE_RISE: usize = 18;
    pub(crate) const CARET_SLOPE_RUN: usize = 20;
    pub(crate) const CARET_OFFSET: usize = 22;
    pub(crate) const METRIC_COUNT: usize = 34;
}

pub(crate) fn subset_metrics(
    records: &GlyphMetricRecords<'_>,
    header: &[u8],
    tags: (Tag, Tag),
    wide: bool,
    plan: &Plan,
    s: &mut Serializer,
    builder: &mut FontBuilder,
    direction: MetricsDirection,
) -> Result<(), SubsetError> {
    let (metric_tag, header_tag) = tags;
    if plan.num_output_glyphs == 0 {
        return Err(error(
            s,
            metric_tag,
            SerializeErrorFlags::SERIALIZE_ERROR_OTHER,
        ));
    }
    // Validate source glyphs, not the compact output IDs. In particular a
    // retained high glyph must have a readable side bearing in the source.
    for (_, old_gid) in &plan.new_to_old_gid_list {
        if records.advance(*old_gid).is_none() || records.side_bearing(*old_gid).is_none() {
            return Err(error(
                s,
                metric_tag,
                SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR,
            ));
        }
    }

    // Instanced metrics are keyed by output glyph id.
    let instanced = !plan.normalized_coords.is_empty();
    let instanced_record = |new_gid: GlyphId| -> Option<(u16, i16)> {
        if !instanced {
            return None;
        }
        match direction {
            MetricsDirection::Horizontal => plan.hmtx_map.borrow().get(&new_gid).copied(),
            MetricsDirection::Vertical => plan.vmtx_map.borrow().get(&new_gid).copied(),
        }
    };

    let advance = |new_gid: usize| -> u16 {
        let new_gid = GlyphId::new(new_gid as u32);
        if let Some((advance, _)) = instanced_record(new_gid) {
            return advance;
        }
        plan.reverse_glyph_map
            .get(&new_gid)
            .and_then(|old_gid| records.advance(*old_gid))
            .unwrap_or(0)
    };
    let mut num_long = plan.num_output_glyphs;
    let last_advance = advance(num_long - 1);
    while num_long > 1 && advance(num_long - 2) == last_advance {
        num_long -= 1;
    }
    let count = if wide {
        u32::try_from(num_long).map(|n| n.to_be_bytes().to_vec())
    } else {
        u16::try_from(num_long).map(|n| n.to_be_bytes().to_vec())
    }
    .map_err(|_| {
        error(
            s,
            metric_tag,
            SerializeErrorFlags::SERIALIZE_ERROR_INT_OVERFLOW,
        )
    })?;
    let mut header = header.to_vec();
    header
        .get_mut(header_offsets::METRIC_COUNT..header_offsets::METRIC_COUNT + count.len())
        .ok_or_else(|| {
            error(
                s,
                header_tag,
                SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR,
            )
        })?
        .copy_from_slice(&count);

    let size = plan
        .num_output_glyphs
        .checked_add(num_long)
        .and_then(|n| n.checked_mul(2))
        .ok_or_else(|| {
            error(
                s,
                metric_tag,
                SerializeErrorFlags::SERIALIZE_ERROR_INT_OVERFLOW,
            )
        })?;
    let start = s
        .allocate_size(size, true)
        .map_err(|_| SubsetError::SubsetTableError(metric_tag))?;
    for (new_gid, old_gid) in &plan.new_to_old_gid_list {
        let new_gid = new_gid.to_u32() as usize;
        if new_gid >= plan.num_output_glyphs {
            return Err(error(
                s,
                metric_tag,
                SerializeErrorFlags::SERIALIZE_ERROR_INT_OVERFLOW,
            ));
        }
        let source_advance = records.advance(*old_gid).unwrap();
        let source_side_bearing = records.side_bearing(*old_gid).unwrap();
        let (advance, side_bearing) = match instanced_record(GlyphId::new(new_gid as u32)) {
            Some((advance, side_bearing)) => (advance, side_bearing),
            None => (source_advance, source_side_bearing),
        };
        if new_gid < num_long {
            let pos = start + new_gid * 4;
            s.copy_assign(pos, advance);
            s.copy_assign(pos + 2, side_bearing);
        } else {
            let pos = start + num_long * 4 + (new_gid - num_long) * 2;
            s.copy_assign(pos, side_bearing);
        }
    }

    if instanced {
        apply_mvar_header_deltas(&mut header, plan, direction);
        recompute_header_bounds(&mut header, plan, direction);
    }

    builder.add_raw(header_tag, header);
    Ok(())
}

/// Apply the `MVAR` deltas to the caret fields of the metrics header.
fn apply_mvar_header_deltas(header: &mut [u8], plan: &Plan, direction: MetricsDirection) {
    let (rise, run, offset) = match direction {
        MetricsDirection::Horizontal => (tags::HCRS, tags::HCRN, tags::HCOF),
        MetricsDirection::Vertical => (tags::VCRS, tags::VCRN, tags::VCOF),
    };
    for (field_offset, tag) in [
        (header_offsets::CARET_SLOPE_RISE, rise),
        (header_offsets::CARET_SLOPE_RUN, run),
        (header_offsets::CARET_OFFSET, offset),
    ] {
        let Some(delta) = plan.mvar_entries.get(&tag) else {
            continue;
        };
        let Some(bytes) = header.get_mut(field_offset..field_offset + 2) else {
            return;
        };
        let value = i16::from_be_bytes([bytes[0], bytes[1]]).wrapping_add(*delta as i16);
        bytes.copy_from_slice(&value.to_be_bytes());
    }
}

/// Recompute the header's advance/bearing summary fields from the instanced
/// metrics, matching what Harfbuzz does when instancing.
fn recompute_header_bounds(header: &mut [u8], plan: &Plan, direction: MetricsDirection) {
    let metrics = match direction {
        MetricsDirection::Horizontal => &plan.hmtx_map,
        MetricsDirection::Vertical => &plan.vmtx_map,
    };
    let bounds = match direction {
        MetricsDirection::Horizontal => &plan.bounds_width_vec,
        MetricsDirection::Vertical => &plan.bounds_height_vec,
    };
    let metrics = metrics.borrow();
    let bounds = bounds.borrow();

    let mut has_bounds = false;
    let mut min_side_bearing: i16 = 0x7FFF;
    let mut min_other_side_bearing: i16 = 0x7FFF;
    let mut max_extent: i16 = -0x7FFF;
    let mut max_advance: u16 = 0;
    for (gid, &(advance, side_bearing)) in metrics.iter() {
        max_advance = max_advance.max(advance);
        let Some(&bound) = bounds.get(gid) else {
            continue;
        };
        has_bounds = true;
        let other_side_bearing = (advance as i16) - side_bearing - (bound as i16);
        let extent = side_bearing + (bound as i16);
        min_side_bearing = min_side_bearing.min(side_bearing);
        min_other_side_bearing = min_other_side_bearing.min(other_side_bearing);
        max_extent = max_extent.max(extent);
    }

    let mut write = |offset: usize, bytes: [u8; 2]| {
        if let Some(slot) = header.get_mut(offset..offset + 2) {
            slot.copy_from_slice(&bytes);
        }
    };
    write(header_offsets::ADVANCE_MAX, max_advance.to_be_bytes());
    if has_bounds {
        write(
            header_offsets::MIN_SIDE_BEARING,
            min_side_bearing.to_be_bytes(),
        );
        write(
            header_offsets::MIN_OTHER_SIDE_BEARING,
            min_other_side_bearing.to_be_bytes(),
        );
        write(header_offsets::MAX_EXTENT, max_extent.to_be_bytes());
    }
}

macro_rules! subset_metrics_table {
    ($table:ident, $header:ident, $records:ident, $read_header:ident, $wide:expr, $direction:expr) => {
        impl crate::Subset for $table<'_> {
            fn subset(
                &self,
                plan: &crate::Plan,
                font: &read_fonts::FontRef,
                s: &mut crate::serialize::Serializer,
                builder: &mut write_fonts::FontBuilder,
            ) -> Result<(), crate::SubsetError> {
                let records = font.$records().map_err(|_| {
                    crate::metrics::error(
                        s,
                        $table::TAG,
                        crate::serialize::SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR,
                    )
                })?;
                let header = font.$read_header().map_err(|_| {
                    crate::metrics::error(
                        s,
                        $header::TAG,
                        crate::serialize::SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR,
                    )
                })?;
                crate::metrics::subset_metrics(
                    &records,
                    header.offset_data().as_bytes(),
                    ($table::TAG, $header::TAG),
                    $wide,
                    plan,
                    s,
                    builder,
                    $direction,
                )
            }
        }
    };
}
pub(crate) use subset_metrics_table;

#[cfg(test)]
mod tests;
