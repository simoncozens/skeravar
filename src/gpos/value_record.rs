//! impl subset() for ValueRecord

use crate::{
    offset::SerializeSubset,
    serialize::{SerializeErrorFlags, SerializeResultEmpty, Serializer},
    CollectVariationIndices, Plan, SubsetTable,
};
use read_fonts::{
    collections::IntSet,
    tables::gpos::{ValueFormat, ValueRecord},
    types::Offset16,
};
use skrifa::raw::{
    tables::{gpos::DeviceOrVariationIndex, variations::NO_VARIATION_INDEX},
    ReadError,
};

/// The on-disk length of a value record, counting every bit set in the format
/// (including reserved ones), matching how Harfbuzz sizes the record array.
pub(crate) fn compute_record_len(value_format: ValueFormat) -> usize {
    (value_format.bits() as u8).count_ones() as usize * 2
}

pub(crate) fn compute_effective_format(
    value_record: &ValueRecord<'_>,
    strip_hints: bool,
    strip_empty: bool,
    plan: Option<&Plan>,
) -> ValueFormat {
    let mut effective = value_record.format();

    if strip_hints {
        effective -= ValueFormat::ANY_DEVICE_OR_VARIDX;
    }

    let devices = [
        (
            ValueFormat::X_PLACEMENT_DEVICE,
            value_record.x_placement_device(),
        ),
        (
            ValueFormat::Y_PLACEMENT_DEVICE,
            value_record.y_placement_device(),
        ),
        (
            ValueFormat::X_ADVANCE_DEVICE,
            value_record.x_advance_device(),
        ),
        (
            ValueFormat::Y_ADVANCE_DEVICE,
            value_record.y_advance_device(),
        ),
    ];

    if !strip_hints {
        if let Some(plan) = plan {
            // Instancing: drop device fields whose variation index is not
            // retained, or that have no offset at all.
            let varidx_map = plan.layout_varidx_delta_map.borrow();
            for (flag, device) in devices {
                if !effective.contains(flag) {
                    continue;
                }
                let device = match device {
                    Some(Ok(device)) => device,
                    // Null or unreadable offsets produce no output.
                    _ => {
                        effective -= flag;
                        continue;
                    }
                };
                // Plain device tables are kepty as they may have non-zero deltas.
                let DeviceOrVariationIndex::VariationIndex(varidx) = device else {
                    continue;
                };
                let ix = varidx.delta_set_inner_index() as u32
                    | ((varidx.delta_set_outer_index() as u32) << 16);
                match varidx_map.get(&ix) {
                    Some((first, _)) if *first != NO_VARIATION_INDEX => {}
                    _ => effective -= flag,
                }
            }
        } else if strip_empty {
            // Drop device fields with a null offset.
            for (flag, device) in devices {
                if device.is_none() {
                    effective -= flag;
                }
            }
        }
    }

    if strip_empty {
        if value_record.x_placement().unwrap_or(0) == 0 {
            effective -= ValueFormat::X_PLACEMENT;
        }
        if value_record.y_placement().unwrap_or(0) == 0 {
            effective -= ValueFormat::Y_PLACEMENT;
        }
        if value_record.x_advance().unwrap_or(0) == 0 {
            effective -= ValueFormat::X_ADVANCE;
        }
        if value_record.y_advance().unwrap_or(0) == 0 {
            effective -= ValueFormat::Y_ADVANCE;
        }
    }

    effective
}

/// Apply delta to a base value if applicable during instancing.
/// For now, we don't apply deltas at the base value level as the device/varidx handling
/// is done through the Device subset logic. This is a placeholder for future enhancements.
fn apply_value_delta(value_record: &ValueRecord<'_>, which_one: ValueFormat, plan: &Plan) -> i16 {
    let base = match which_one {
        ValueFormat::X_PLACEMENT => value_record.x_placement().unwrap_or_default(),
        ValueFormat::Y_PLACEMENT => value_record.y_placement().unwrap_or_default(),
        ValueFormat::X_ADVANCE => value_record.x_advance().unwrap_or_default(),
        ValueFormat::Y_ADVANCE => value_record.y_advance().unwrap_or_default(),
        _ => 0, // For device/varidx fields, the deltas are handled in the device subset logic}
    };
    let device_offset = match which_one {
        ValueFormat::X_PLACEMENT => value_record.x_placement_device().transpose().ok().flatten(),
        ValueFormat::Y_PLACEMENT => value_record.y_placement_device().transpose().ok().flatten(),
        ValueFormat::X_ADVANCE => value_record.x_advance_device().transpose().ok().flatten(),
        ValueFormat::Y_ADVANCE => value_record.y_advance_device().transpose().ok().flatten(),
        _ => None,
    };
    if let Some(DeviceOrVariationIndex::VariationIndex(varidx)) = device_offset {
        // Encode the two-level variation index as a single u32:
        // combine outer and inner indices as (outer << 16) | inner
        let combined_idx = ((varidx.delta_set_outer_index() as u32) << 16)
            | (varidx.delta_set_inner_index() as u32);
        if let Some((_idx, delta)) = plan.layout_varidx_delta_map.borrow().get(&combined_idx) {
            return base.saturating_add(*delta as i16);
        }
    }
    base
}

impl<'a> SubsetTable<'a> for ValueRecord<'_> {
    type ArgsForSubset = ValueFormat;
    type Output = ();

    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        new_format: Self::ArgsForSubset,
    ) -> Result<(), SerializeErrorFlags> {
        if new_format.is_empty() {
            return Ok(());
        }

        if new_format.contains(ValueFormat::X_PLACEMENT) {
            let value = apply_value_delta(self, ValueFormat::X_PLACEMENT, plan);
            s.embed(value)?;
        }

        if new_format.contains(ValueFormat::Y_PLACEMENT) {
            let value = apply_value_delta(self, ValueFormat::Y_PLACEMENT, plan);
            s.embed(value)?;
        }

        if new_format.contains(ValueFormat::X_ADVANCE) {
            let value = apply_value_delta(self, ValueFormat::X_ADVANCE, plan);
            s.embed(value)?;
        }

        if new_format.contains(ValueFormat::Y_ADVANCE) {
            let value = apply_value_delta(self, ValueFormat::Y_ADVANCE, plan);
            s.embed(value)?;
        }

        if !new_format.intersects(ValueFormat::ANY_DEVICE_OR_VARIDX) {
            return Ok(());
        }

        copy_device(
            s,
            plan,
            new_format,
            ValueFormat::X_PLACEMENT_DEVICE,
            self.x_placement_device(),
        )?;

        copy_device(
            s,
            plan,
            new_format,
            ValueFormat::Y_PLACEMENT_DEVICE,
            self.y_placement_device(),
        )?;

        copy_device(
            s,
            plan,
            new_format,
            ValueFormat::X_ADVANCE_DEVICE,
            self.x_advance_device(),
        )?;

        copy_device(
            s,
            plan,
            new_format,
            ValueFormat::Y_ADVANCE_DEVICE,
            self.y_advance_device(),
        )?;

        Ok(())
    }
}

/// Serialize one Device/VariationIndex offset field of a [`ValueRecord`].
///
/// The field's presence is determined solely by `new_format`: whenever the flag
/// is set we must emit exactly two bytes, even if the source record had a null
/// (or unreadable) offset. Writing nothing in that case would shorten the record
/// and desynchronize every following record in the parent table.
fn copy_device(
    s: &mut Serializer,
    plan: &Plan,
    new_format: ValueFormat,
    flag: ValueFormat,
    device: Option<Result<DeviceOrVariationIndex<'_>, ReadError>>,
) -> Result<(), SerializeErrorFlags> {
    if !new_format.contains(flag) {
        return Ok(());
    }

    // Reserve the offset slot up front so the record always has the size implied
    // by `new_format`, regardless of whether the source has a real device table.
    let offset_pos = s.embed(0_u16)?;

    let device = match device {
        // A null offset leaves the slot as a null offset.
        None => return Ok(()),
        Some(Ok(device)) => device,
        // An offset that leads nowhere is like something HarfBuzz's sanitizer
        // rejects, so fail the table rather than emitting a null offset.
        Some(Err(_)) => return Err(s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR)),
    };

    // Mirrors HarfBuzz's ValueFormat::copy_device: a device that subsets to
    // nothing (an unretained variation index, say) leaves a null offset behind,
    // but any other failure fails the table.
    let varidx_map = plan.layout_varidx_delta_map.borrow();
    Offset16::serialize_subset(&device, s, plan, &varidx_map, offset_pos).is_empty()?;
    Ok(())
}

impl CollectVariationIndices for ValueRecord<'_> {
    fn collect_variation_indices(&self, plan: &Plan, varidx_set: &mut IntSet<u32>) {
        if !self.format().intersects(ValueFormat::ANY_DEVICE_OR_VARIDX) {
            return;
        }

        for device in [
            self.x_placement_device(),
            self.y_placement_device(),
            self.x_advance_device(),
            self.y_advance_device(),
        ]
        .into_iter()
        .flatten()
        .flatten()
        {
            device.collect_variation_indices(plan, varidx_set);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use read_fonts::FontData;

    const ALL: ValueFormat = ValueFormat::from_bits_truncate(0x00FF);

    /// A record in `ALL` format: four values then four device offsets.
    fn record(bytes: &[u8]) -> ValueRecord<'_> {
        ValueRecord::new(FontData::new(bytes), 0, ALL)
    }

    /// Bytes for a record whose every field is zero.
    const EMPTY: [u8; 16] = [0; 16];

    /// x_placement = 1, x_placement_device -> 12; everything else zero.
    const PARTLY_SET: [u8; 16] = [0, 1, 0, 0, 0, 0, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0];

    #[test]
    fn keeps_everything_when_stripping_nothing() {
        // with neither flag set the format is reported as-is, even for fields
        // that are zero: the caller has not asked for anything to be dropped
        assert_eq!(
            compute_effective_format(&record(&EMPTY), false, false, None),
            ALL
        );
        assert_eq!(
            compute_effective_format(&record(&PARTLY_SET), false, false, None),
            ALL
        );
    }

    #[test]
    fn strip_empty_drops_zero_values_and_null_offsets() {
        // an all-zero record keeps nothing
        assert_eq!(
            compute_effective_format(&record(&EMPTY), false, true, None),
            ValueFormat::empty()
        );
        // and only the two fields that are actually set survive
        assert_eq!(
            compute_effective_format(&record(&PARTLY_SET), false, true, None),
            ValueFormat::X_PLACEMENT | ValueFormat::X_PLACEMENT_DEVICE
        );
    }

    #[test]
    fn strip_hints_drops_devices_however_they_are_set() {
        // device fields go whether or not their offsets are null, and whether
        // or not empty fields are being stripped
        assert_eq!(
            compute_effective_format(&record(&PARTLY_SET), true, false, None),
            ALL - ValueFormat::ANY_DEVICE_OR_VARIDX
        );
        assert_eq!(
            compute_effective_format(&record(&PARTLY_SET), true, true, None),
            ValueFormat::X_PLACEMENT
        );
    }

    /// Fields absent from the format are never reported present, whatever the
    /// bytes underneath happen to say.
    #[test]
    fn absent_fields_stay_absent() {
        let format = ValueFormat::Y_ADVANCE | ValueFormat::Y_ADVANCE_DEVICE;
        let bytes = [0u8, 7, 0, 12];
        let rec = ValueRecord::new(FontData::new(&bytes), 0, format);
        assert_eq!(compute_effective_format(&rec, false, true, None), format);
        assert_eq!(
            compute_effective_format(&rec, true, true, None),
            ValueFormat::Y_ADVANCE
        );
    }

    #[test]
    fn positioning_survives_unmappable_variation_devices() {
        use read_fonts::tables::variations::NO_VARIATION_INDEX;
        use write_fonts::tables::{gpos::ValueRecord as OwnedValue, layout::VariationIndex};

        // An index mapped to NO_VARIATION_INDEX is still written out as
        // (0xffff, 0xffff), like Harfbuzz does; only unmapped indices are
        // dropped.
        for (index, mapped, expected) in [
            ((1, 2), None, None),
            ((0xffff, 0xffff), None, None),
            ((1, 2), Some(NO_VARIATION_INDEX), Some((0xffff, 0xffff))),
            ((1, 2), Some(0x30004), Some((3, 4))),
        ] {
            let source = OwnedValue::new()
                .with_x_advance(40)
                .with_x_advance_device(VariationIndex::new(index.0, index.1));
            let format = source.format();
            let source = write_fonts::dump_table(&source).unwrap();
            let value = ValueRecord::new(FontData::new(&source), 0, format);
            let mut plan = Plan::default();
            if let Some(mapped) = mapped {
                // All mapped cases use source (1, 2): (outer << 16) | inner.
                plan.layout_varidx_delta_map
                    .borrow_mut()
                    .insert(0x10002, (mapped, 0));
            }
            let mut s = Serializer::new(64).unwrap();
            s.start_serialize().unwrap();
            value.subset(&plan, &mut s, format).unwrap();
            s.end_serialize();

            let bytes = s.copy_bytes();
            let value = ValueRecord::new(FontData::new(&bytes), 0, format);
            assert_eq!(value.x_advance(), Some(40));
            let actual = value.x_advance_device().map(|device| {
                let DeviceOrVariationIndex::VariationIndex(index) = device.unwrap() else {
                    panic!("expected variation index")
                };
                (index.delta_set_outer_index(), index.delta_set_inner_index())
            });
            assert_eq!(actual, expected);
        }
    }
}
