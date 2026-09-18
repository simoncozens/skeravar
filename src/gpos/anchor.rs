//! impl subset() for Anchor subtable

use crate::{
    layout::{apply_coordinate_delta, is_no_variation_index},
    offset::SerializeSubset,
    serialize::{SerializeErrorFlags, Serializer},
    CollectVariationIndices, Plan, SubsetFlags, SubsetTable,
};
use skrifa::raw::tables::gpos::DeviceOrVariationIndex;
use write_fonts::{
    read::{
        collections::IntSet,
        tables::gpos::{AnchorFormat1, AnchorFormat2, AnchorFormat3, AnchorTable},
        FontData, MinByteRange,
    },
    types::Offset16,
};

impl<'a> SubsetTable<'a> for AnchorTable<'a> {
    type ArgsForSubset = FontData<'a>;
    type Output = ();
    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        font_data: Self::ArgsForSubset,
    ) -> Result<Self::Output, SerializeErrorFlags> {
        match self {
            Self::Format1(item) => item.subset(plan, s, font_data),
            Self::Format2(item) => item.subset(plan, s, font_data),
            Self::Format3(item) => item.subset(plan, s, font_data),
        }
    }
}

impl<'a> SubsetTable<'a> for AnchorFormat1<'a> {
    type ArgsForSubset = FontData<'a>;
    type Output = ();
    fn subset(
        &self,
        _plan: &Plan,
        s: &mut Serializer,
        _font_data: Self::ArgsForSubset,
    ) -> Result<(), SerializeErrorFlags> {
        s.embed_bytes(self.min_table_bytes()).map(|_| ())
    }
}

impl<'a> SubsetTable<'a> for AnchorFormat2<'a> {
    type ArgsForSubset = FontData<'a>;
    type Output = ();
    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        _font_data: Self::ArgsForSubset,
    ) -> Result<(), SerializeErrorFlags> {
        if plan
            .subset_flags
            .contains(SubsetFlags::SUBSET_FLAGS_NO_HINTING)
        {
            // AnchorFormat 2 just containins extra hinting information
            // if hints are being dropped downgrade to format 1.
            s.embed(1_u16)?;
            s.embed(self.x_coordinate())?;
            s.embed(self.y_coordinate()).map(|_| ())
        } else {
            s.embed_bytes(self.min_table_bytes()).map(|_| ())
        }
    }
}

impl<'a> SubsetTable<'a> for AnchorFormat3<'a> {
    type ArgsForSubset = FontData<'a>;
    type Output = ();
    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        _font_data: Self::ArgsForSubset,
    ) -> Result<(), SerializeErrorFlags> {
        let format_pos = s.embed(self.anchor_format())?;
        let x_device = self
            .x_device()
            .transpose()
            .map_err(|_| SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR)?;
        let x_device = x_device.as_ref();
        let y_device = self
            .y_device()
            .transpose()
            .map_err(|_| SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR)?;
        let y_device = y_device.as_ref();
        let x_var_index = x_device.and_then(|device| match device {
            DeviceOrVariationIndex::VariationIndex(varidx) => Some(varidx),
            _ => None,
        });
        let y_var_index = y_device.and_then(|device| match device {
            DeviceOrVariationIndex::VariationIndex(varidx) => Some(varidx),
            _ => None,
        });

        // Apply deltas to coordinates when instancing
        let x_coord = apply_coordinate_delta(self.x_coordinate(), x_var_index, plan);
        let y_coord = apply_coordinate_delta(self.y_coordinate(), y_var_index, plan);

        s.embed(x_coord)?;
        s.embed(y_coord)?;

        // Check if devices should be kept during instancing based on variation index mapping
        // println!("Looking at an anchor");
        // println!("X device is some? {}", x_device.is_some());
        // println!(
        //     "X device isn't a variation index? {}",
        //     !matches!(x_device, Some(DeviceOrVariationIndex::VariationIndex(_)))
        // );
        // println!(
        //     "X device var index: {:?}",
        //     x_var_index.map(|x| (x.delta_set_outer_index(), x.delta_set_inner_index()))
        // );
        // println!("Y device is some? {}", y_device.is_some());
        // println!(
        //     "Y device isn't a variation index? {}",
        //     !matches!(y_device, Some(DeviceOrVariationIndex::VariationIndex(_)))
        // );
        // println!(
        //     "Y device var index: {:?}",
        //     y_var_index.map(|x| (x.delta_set_outer_index(), x.delta_set_inner_index()))
        // );
        let no_downgrade = (
            // x is some and not a variation index, or is a variation index which maps to NO_VARIATION_INDEX
            (x_device.as_ref().is_some() &&!matches!(x_device, Some(DeviceOrVariationIndex::VariationIndex(_)))
            || !is_no_variation_index(x_var_index, plan))
        ) || // similarly for y
            (y_device.as_ref().is_some() &&!matches!(y_device, Some(DeviceOrVariationIndex::VariationIndex(_)))
            || !is_no_variation_index(y_var_index, plan));
        if !no_downgrade {
            // Set to format 1 and we're done
            s.copy_assign(format_pos, 1_u16);
            return Ok(());
        }

        let x_device_offset_pos = s.embed(0_u16)?;
        if let Some(x_device) = self
            .x_device()
            .transpose()
            .map_err(|_| SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR)?
        {
            match Offset16::serialize_subset(
                &x_device,
                s,
                plan,
                &plan.layout_varidx_delta_map.borrow(),
                x_device_offset_pos,
            ) {
                Ok(()) => (),
                Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY) => (),
                Err(e) => {
                    return Err(e);
                }
            }
        }

        let y_device_offset_pos = s.embed(0_u16)?;
        if let Some(y_device) = self
            .y_device()
            .transpose()
            .map_err(|_| SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR)?
        {
            match Offset16::serialize_subset(
                &y_device,
                s,
                plan,
                &plan.layout_varidx_delta_map.borrow(),
                y_device_offset_pos,
            ) {
                Ok(()) => (),
                Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY) => (),
                Err(e) => {
                    return Err(e);
                }
            }
        }

        Ok(())
    }
}

impl CollectVariationIndices for AnchorTable<'_> {
    fn collect_variation_indices(&self, plan: &Plan, varidx_set: &mut IntSet<u32>) {
        if let Self::Format3(item) = self {
            item.collect_variation_indices(plan, varidx_set)
        }
    }
}

impl CollectVariationIndices for AnchorFormat3<'_> {
    fn collect_variation_indices(&self, plan: &Plan, varidx_set: &mut IntSet<u32>) {
        if let Some(Ok(x_device)) = self.x_device() {
            x_device.collect_variation_indices(plan, varidx_set);
        }
        if let Some(Ok(y_device)) = self.y_device() {
            y_device.collect_variation_indices(plan, varidx_set);
        }
    }
}
