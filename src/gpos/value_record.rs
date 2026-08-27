//! impl subset() for ValueRecord

use crate::{
    offset::SerializeSubset,
    serialize::{SerializeErrorFlags, Serializer},
    CollectVariationIndices, Plan, SubsetTable,
};
use skrifa::raw::{
    tables::{gpos::DeviceOrVariationIndex, variations::NO_VARIATION_INDEX},
    ReadError,
};
use write_fonts::{
    read::{
        collections::IntSet,
        tables::gpos::{ValueFormat, ValueRecord},
    },
    types::Offset16,
};

pub(crate) fn compute_effective_format(
    value_record: &ValueRecord<'_>,
    strip_hints: bool,
    strip_empty: bool,
    plan: Option<&Plan>,
) -> ValueFormat {
    let mut value_format = ValueFormat::empty();

    if let Some(x_placement) = value_record.x_placement() {
        if !strip_empty || x_placement != 0 {
            value_format |= ValueFormat::X_PLACEMENT;
        }
    }

    if let Some(y_placement) = value_record.y_placement() {
        if !strip_empty || y_placement != 0 {
            value_format |= ValueFormat::Y_PLACEMENT;
        }
    }

    if let Some(x_advance) = value_record.x_advance() {
        if !strip_empty || x_advance != 0 {
            value_format |= ValueFormat::X_ADVANCE;
        }
    }

    if let Some(y_advance) = value_record.y_advance() {
        if !strip_empty || y_advance != 0 {
            value_format |= ValueFormat::Y_ADVANCE;
        }
    }

    if !strip_hints {
        if let Some(device) = value_record.x_placement_device() {
            update_var_flag(
                Some(device),
                ValueFormat::X_PLACEMENT_DEVICE,
                &mut value_format,
                plan,
            );
        }
        if let Some(device) = value_record.y_placement_device() {
            update_var_flag(
                Some(device),
                ValueFormat::Y_PLACEMENT_DEVICE,
                &mut value_format,
                plan,
            );
        }
        if let Some(device) = value_record.x_advance_device() {
            update_var_flag(
                Some(device),
                ValueFormat::X_ADVANCE_DEVICE,
                &mut value_format,
                plan,
            );
        }
        if let Some(device) = value_record.y_advance_device() {
            update_var_flag(
                Some(device),
                ValueFormat::Y_ADVANCE_DEVICE,
                &mut value_format,
                plan,
            );
        }
    }
    value_format
}

fn update_var_flag(
    value: Option<Result<DeviceOrVariationIndex<'_>, ReadError>>,
    flag: ValueFormat,
    format: &mut ValueFormat,
    plan: Option<&Plan>,
) {
    if let Some(plan_ref) = plan {
        let varidx_map = plan_ref.layout_varidx_delta_map.borrow();

        if let Some(varidx) = value.transpose().ok().flatten() {
            match varidx {
                DeviceOrVariationIndex::Device(_device) => {
                    // For device tables, we conservatively assume they may have non-zero deltas and keep the flag
                    *format |= flag;
                }
                DeviceOrVariationIndex::VariationIndex(varidx) => {
                    let ix = varidx.delta_set_inner_index() as u32
                        | ((varidx.delta_set_outer_index() as u32) << 16);
                    if let Some((first, _)) = varidx_map.get(&ix) {
                        if *first != NO_VARIATION_INDEX {
                            *format |= flag;
                            return;
                        }
                    }
                }
            }
            *format &= !flag;
        }
    } else {
        *format |= flag;
    }
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

    // A null offset (or a broken device table) leaves the slot as a null offset.
    let Some(Ok(device)) = device else {
        return Ok(());
    };

    // Mirrors HarfBuzz's ValueFormat::copy_device: if the device table cannot be
    // serialized it is simply dropped, leaving a null offset behind.
    let varidx_map = plan.layout_varidx_delta_map.borrow();
    match Offset16::serialize_subset(&device, s, plan, &varidx_map, offset_pos) {
        Ok(()) => Ok(()),
        Err(_) => Ok(()),
    }
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
