//! impl subset() for PairPos subtable

use crate::{
    gpos::value_record::compute_effective_format,
    layout::{
        for_each_intersected_coverage_index, intersected_glyphs_and_indices, map_gsub_glyph,
        ClassDefSubsetStruct,
    },
    offset::{SerializeSerialize, SerializeSubset},
    offset_array::SubsetOffsetArray,
    serialize::{SerializeErrorFlags, SerializeResultEmpty, Serializer},
    CollectVariationIndices, Plan, SubsetFlags, SubsetState, SubsetTable,
};

use crate::FastHashMap;
use read_fonts::{
    collections::IntSet,
    tables::{
        gpos::{PairPos, PairPosFormat1, PairPosFormat2, PairSet, PairValueRecord, ValueFormat},
        layout::CoverageTable,
    },
    types::GlyphId,
    types::Offset16,
    FontRef, ReadError, TableProvider,
};

impl<'a> SubsetTable<'a> for PairPos<'_> {
    type ArgsForSubset = (&'a SubsetState, &'a FontRef<'a>, &'a FastHashMap<u16, u16>);
    type Output = ();
    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        args: Self::ArgsForSubset,
    ) -> Result<Self::Output, SerializeErrorFlags> {
        let args = (args.0, args.1);
        match self {
            Self::Format1(item) => item.subset(plan, s, args),
            Self::Format2(item) => item.subset(plan, s, args),
        }
    }
}

fn compute_effective_pair_formats_1(
    pair_pos: &PairPosFormat1,
    glyph_set: &IntSet<GlyphId>,
    strip_hints: bool,
    strip_empty: bool,
    plan: &Plan,
) -> Result<(ValueFormat, ValueFormat), ReadError> {
    let mut new_format1 = ValueFormat::empty();
    let mut new_format2 = ValueFormat::empty();

    let coverage = pair_pos.coverage()?;
    let pair_sets = pair_pos.pair_sets();
    for_each_intersected_coverage_index::<ReadError>(
        &coverage,
        glyph_set,
        pair_pos.pair_set_count(),
        |i| {
            let pair_set = match pair_sets.get(i as usize) {
                Err(ReadError::NullOffset) => return Ok(()),
                other => other?,
            };
            for pair_value_rec in pair_set.pair_value_records().iter() {
                let pair_value_rec = pair_value_rec?;
                let second_glyph = pair_value_rec.second_glyph();
                if !glyph_set.contains(GlyphId::from(second_glyph)) {
                    continue;
                }

                new_format1 |= compute_effective_format(
                    pair_value_rec.value_record1(),
                    strip_hints,
                    strip_empty,
                    Some(plan),
                );
                new_format2 |= compute_effective_format(
                    pair_value_rec.value_record2(),
                    strip_hints,
                    strip_empty,
                    Some(plan),
                );
            }
            Ok(())
        },
    )?;

    Ok((new_format1, new_format2))
}

impl SubsetTable<'_> for PairSet<'_> {
    type ArgsForSubset = (ValueFormat, ValueFormat);
    type Output = ();
    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        args: Self::ArgsForSubset,
    ) -> Result<(), SerializeErrorFlags> {
        // pairvalue count
        let pairvalue_count_pos = s.embed(0_u16)?;
        let mut count = 0_u16;

        let glyph_map = &plan.glyph_map_gsub;
        let (new_format1, new_format2) = args;

        for pairvalue_rec in self.pair_value_records().iter() {
            let pairvalue_rec = pairvalue_rec
                .map_err(|_| s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR))?;
            let Some(gid) = map_gsub_glyph(glyph_map, GlyphId::from(pairvalue_rec.second_glyph()))
            else {
                continue;
            };
            pairvalue_rec.subset(plan, s, (&gid, new_format1, new_format2))?;
            count += 1;
        }

        if count == 0 {
            return Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY);
        }
        s.copy_assign(pairvalue_count_pos, count);
        Ok(())
    }
}

impl<'a> SubsetTable<'a> for PairValueRecord<'_> {
    type ArgsForSubset = (&'a GlyphId, ValueFormat, ValueFormat);
    type Output = ();
    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        args: Self::ArgsForSubset,
    ) -> Result<(), SerializeErrorFlags> {
        let (new_gid, new_format1, new_format2) = args;
        // second glyph
        s.embed(new_gid.to_u32() as u16)?;

        //value records
        self.value_record1().subset(plan, s, new_format1)?;
        self.value_record2().subset(plan, s, new_format2)
    }
}

impl<'a> SubsetTable<'a> for PairPosFormat1<'_> {
    type ArgsForSubset = (&'a SubsetState, &'a FontRef<'a>);
    type Output = ();
    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        args: Self::ArgsForSubset,
    ) -> Result<(), SerializeErrorFlags> {
        if self.coverage_offset().is_null() {
            return Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY);
        }
        let (subset_state, font) = args;
        let glyph_map = &plan.glyph_map_gsub;
        let glyph_set = &plan.glyphset_gsub;
        // format
        s.embed(self.pos_format())?;

        // coverage offset
        let cov_offset_pos = s.embed(0_u16)?;

        // value_formats
        let (new_format1, new_format2) = if !plan.normalized_coords.is_empty() {
            // Instancing case: always compute effective formats to filter out device flags
            // that won't produce output (variation indices mapped to NO_VARIATION_INDEX)
            compute_effective_pair_formats_1(
                self, glyph_set, false, // strip_hints=false for instancing
                false, // strip_empty=false
                plan,
            )
            .map_err(|_| s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR))?
        } else if plan
            .subset_flags
            .contains(SubsetFlags::SUBSET_FLAGS_NO_HINTING)
        {
            // do not strip hints for VF unless it has no GDEF varstore after subsetting
            let strip_hints = if font.fvar().is_ok() {
                !subset_state.has_gdef_varstore
            } else {
                true
            };

            compute_effective_pair_formats_1(self, glyph_set, strip_hints, true, plan)
                .map_err(|_| s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR))?
        } else {
            (self.value_format1(), self.value_format2())
        };
        s.embed(new_format1)?;
        s.embed(new_format2)?;

        // pairset count
        let pairset_count_pos = s.embed(0_u16)?;
        let mut pairset_count = 0_u16;

        let coverage = self
            .coverage()
            .map_err(|_| s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR))?;
        let pair_sets = self.pair_sets();

        let (glyphs, pairset_idxes) =
            intersected_glyphs_and_indices(&coverage, glyph_set, glyph_map, self.pair_set_count());
        if glyphs.is_empty() {
            return Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY);
        }

        let mut retained_glyphs = Vec::with_capacity(glyphs.len());
        for (i, g) in pairset_idxes.iter().zip(glyphs) {
            if !pair_sets
                .subset_offset(i as usize, s, plan, (new_format1, new_format2))
                .is_empty()?
            {
                pairset_count += 1;
                retained_glyphs.push(g);
            }
        }

        s.copy_assign(pairset_count_pos, pairset_count);
        Offset16::serialize_serialize::<CoverageTable>(s, &retained_glyphs, cov_offset_pos)
    }
}

fn compute_effective_pair_formats_2(
    pair_pos: &PairPosFormat2,
    class1_idxes: &[u16],
    class2_idxes: &[u16],
    strip_hints: bool,
    strip_empty: bool,
    plan: &Plan,
) -> Result<(ValueFormat, ValueFormat), ReadError> {
    let mut new_format1 = ValueFormat::empty();
    let mut new_format2 = ValueFormat::empty();

    let orig_format1 = pair_pos.value_format1();
    let orig_format2 = pair_pos.value_format2();

    let class_records = pair_pos.class_value_records();
    for i in class1_idxes {
        for j in class2_idxes {
            let [rec1, rec2] = class_records.get(*i, *j).ok_or(ReadError::OutOfBounds)?;

            new_format1 |= compute_effective_format(&rec1, strip_hints, strip_empty, Some(plan));
            new_format2 |= compute_effective_format(&rec2, strip_hints, strip_empty, Some(plan));
        }
        if new_format1 == orig_format1 && new_format2 == orig_format2 {
            break;
        }
    }
    Ok((new_format1, new_format2))
}

impl<'a> SubsetTable<'a> for PairPosFormat2<'_> {
    type ArgsForSubset = (&'a SubsetState, &'a FontRef<'a>);
    type Output = ();
    fn subset(
        &self,
        plan: &Plan,
        s: &mut Serializer,
        args: Self::ArgsForSubset,
    ) -> Result<(), SerializeErrorFlags> {
        if self.coverage_offset().is_null()
            || self.class_def1_offset().is_null()
            || self.class_def2_offset().is_null()
        {
            return Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY);
        }
        let Ok(coverage) = self.coverage() else {
            return Err(s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR));
        };

        let glyphs: Vec<GlyphId> = coverage
            .intersect_set(&plan.glyphset_gsub)
            .iter()
            .filter_map(|g| map_gsub_glyph(&plan.glyph_map_gsub, g))
            .collect();
        if glyphs.is_empty() {
            return Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY);
        }

        // format
        s.embed(self.pos_format())?;

        // coverage offset
        let cov_offset_pos = s.embed(0_u16)?;

        // value format
        let value_format1_pos = s.embed(0_u16)?;
        let value_format2_pos = s.embed(0_u16)?;

        // classdef1 offset
        let classdef1_offset_pos = s.embed(0_u16)?;
        let class_def1 = self
            .class_def1()
            .map_err(|_| s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR))?;
        let class1_map = match Offset16::serialize_subset(
            &class_def1,
            s,
            plan,
            &ClassDefSubsetStruct {
                remap_class: true,
                keep_empty_table: true,
                use_class_zero: true,
                glyph_filter: Some(&coverage),
            },
            classdef1_offset_pos,
        ) {
            Ok(Some(out)) => out,
            _ => FastHashMap::default(),
        };

        if class1_map.is_empty() {
            return Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY);
        }
        let class1_count = class1_map.len() as u16;

        // classdef2 offset
        let classdef2_offset_pos = s.embed(0_u16)?;
        let class_def2 = self
            .class_def2()
            .map_err(|_| s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR))?;

        let class2_map = match Offset16::serialize_subset(
            &class_def2,
            s,
            plan,
            &ClassDefSubsetStruct {
                remap_class: true,
                keep_empty_table: true,
                use_class_zero: false,
                glyph_filter: None,
            },
            classdef2_offset_pos,
        ) {
            Ok(Some(out)) => out,
            _ => FastHashMap::default(),
        };

        // If only Class2 0 left, no need to keep anything.
        if class2_map.len() <= 1 {
            return Err(SerializeErrorFlags::SERIALIZE_ERROR_EMPTY);
        }
        let class2_count = class2_map.len() as u16;

        // class1_count
        s.embed(class1_count)?;
        // class2_count
        s.embed(class2_count)?;

        // value formats
        let (subset_state, font) = args;
        let class1_idxes: Vec<u16> = (0..self.class1_count())
            .filter(|i| class1_map.contains_key(i))
            .collect();
        let class2_idxes: Vec<u16> = (0..self.class2_count())
            .filter(|i| class2_map.contains_key(i))
            .collect();

        let (new_format1, new_format2) = if !plan.normalized_coords.is_empty() {
            // Instancing case: always compute effective formats to filter out device flags
            // that won't produce output (variation indices mapped to NO_VARIATION_INDEX)
            compute_effective_pair_formats_2(
                self,
                &class1_idxes,
                &class2_idxes,
                false, // strip_hints=false for instancing
                false, // strip_empty=false
                plan,
            )
            .map_err(|_| s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR))?
        } else if plan
            .subset_flags
            .contains(SubsetFlags::SUBSET_FLAGS_NO_HINTING)
        {
            // do not strip hints for VF unless it has no GDEF varstore after subsetting
            let strip_hints = if font.fvar().is_ok() {
                !subset_state.has_gdef_varstore
            } else {
                true
            };

            compute_effective_pair_formats_2(
                self,
                &class1_idxes,
                &class2_idxes,
                strip_hints,
                true,
                plan,
            )
            .map_err(|_| s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR))?
        } else {
            (self.value_format1(), self.value_format2())
        };

        s.copy_assign(value_format1_pos, new_format1);
        s.copy_assign(value_format2_pos, new_format2);

        // serialize value records
        let class_records = self.class_value_records();
        for i in class1_idxes {
            for j in &class2_idxes {
                let Some([rec1, rec2]) = class_records.get(i, *j) else {
                    return Err(s.set_err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR));
                };

                rec1.subset(plan, s, new_format1)?;
                rec2.subset(plan, s, new_format2)?;
            }
        }

        // this can be moved, put it at last so we have the same binary data with Harfbuzz subsetter
        Offset16::serialize_serialize::<CoverageTable>(s, &glyphs, cov_offset_pos)
    }
}

impl CollectVariationIndices for PairSet<'_> {
    fn collect_variation_indices(&self, plan: &Plan, varidx_set: &mut IntSet<u32>) {
        let glyph_set = &plan.glyphset_gsub;
        for pairvalue_record in self.pair_value_records().iter() {
            let Ok(pairvalue_record) = pairvalue_record else {
                return;
            };
            if !glyph_set.contains(GlyphId::from(pairvalue_record.second_glyph())) {
                continue;
            }

            pairvalue_record
                .value_record1()
                .collect_variation_indices(plan, varidx_set);
            pairvalue_record
                .value_record2()
                .collect_variation_indices(plan, varidx_set);
        }
    }
}

impl CollectVariationIndices for PairPos<'_> {
    fn collect_variation_indices(&self, plan: &Plan, varidx_set: &mut IntSet<u32>) {
        match self {
            Self::Format1(item) => item.collect_variation_indices(plan, varidx_set),
            Self::Format2(item) => item.collect_variation_indices(plan, varidx_set),
        }
    }
}

impl CollectVariationIndices for PairPosFormat1<'_> {
    fn collect_variation_indices(&self, plan: &Plan, varidx_set: &mut IntSet<u32>) {
        let value_format1 = self.value_format1();
        let value_format2 = self.value_format2();

        if !value_format1.intersects(ValueFormat::ANY_DEVICE_OR_VARIDX)
            && !value_format2.intersects(ValueFormat::ANY_DEVICE_OR_VARIDX)
        {
            return;
        }

        let Ok(coverage) = self.coverage() else {
            return;
        };

        let glyph_set = &plan.glyphset_gsub;
        let pair_sets = self.pair_sets();
        let _ = for_each_intersected_coverage_index::<()>(
            &coverage,
            glyph_set,
            self.pair_set_count(),
            |i| {
                if let Ok(pair_set) = pair_sets.get(i as usize) {
                    pair_set.collect_variation_indices(plan, varidx_set);
                }
                Ok(())
            },
        );
    }
}

impl CollectVariationIndices for PairPosFormat2<'_> {
    fn collect_variation_indices(&self, plan: &Plan, varidx_set: &mut IntSet<u32>) {
        let value_format1 = self.value_format1();
        let value_format2 = self.value_format2();

        if !value_format1.intersects(ValueFormat::ANY_DEVICE_OR_VARIDX)
            && !value_format2.intersects(ValueFormat::ANY_DEVICE_OR_VARIDX)
        {
            return;
        }
        let Ok(coverage) = self.coverage() else {
            return;
        };

        let glyph_set = &plan.glyphset_gsub;
        let cov_glyphs = coverage.intersect_set(glyph_set);
        if cov_glyphs.is_empty() {
            return;
        };

        let Ok(classdef1) = self.class_def1() else {
            return;
        };

        let Ok(classdef2) = self.class_def2() else {
            return;
        };

        let class1_set = classdef1.intersect_classes(&cov_glyphs);
        if class1_set.is_empty() {
            return;
        }
        let mut class2_set = classdef2.intersect_classes(glyph_set);
        if class2_set.is_empty() {
            return;
        }
        class2_set.insert(0);

        let class1_count = self.class1_count();
        let class2_count = self.class2_count();
        let class_records = self.class_value_records();
        for class1 in class1_set.iter() {
            // intersect_classes reports the class values a ClassDef actually
            // assigns, which is not tied to class1Count/class2Count and so can
            // run past the end of the record matrix in a malformed font. The
            // sets are iterated in ascending order, so we can stop at the
            // first out-of-range value, matching HarfBuzz.
            if class1 >= u32::from(class1_count) {
                break;
            }
            for class2 in class2_set.iter() {
                if class2 >= u32::from(class2_count) {
                    break;
                }
                let Some([rec1, rec2]) = class_records.get(class1 as u16, class2 as u16) else {
                    continue;
                };
                rec1.collect_variation_indices(plan, varidx_set);
                rec2.collect_variation_indices(plan, varidx_set);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use read_fonts::{FontRef, TableProvider};

    #[test]
    fn test_subset_pairpos_format1() {
        use read_fonts::tables::gpos::PositionSubtables;

        let font = FontRef::new(include_bytes!("../../test-data/fonts/Amiri-Regular.ttf")).unwrap();
        let gpos_lookups = font.gpos().unwrap().lookup_list().unwrap();
        let lookup = gpos_lookups.lookups().get(56).unwrap();

        let PositionSubtables::Pair(sub_tables) = lookup.subtables().unwrap() else {
            panic!("Wrong type of lookup table!");
        };
        let pairpos_table = sub_tables.get(0).unwrap();

        let subset_state = SubsetState::default();
        let mut plan = Plan {
            glyph_map_gsub: vec![crate::INVALID_GID; 6299],
            ..Default::default()
        };

        plan.glyph_map_gsub[6292] = GlyphId::from(3_u32);
        plan.glyph_map_gsub[6298] = GlyphId::from(4_u32);

        plan.glyphset_gsub.insert(GlyphId::from(6292_u32));
        plan.glyphset_gsub.insert(GlyphId::from(6298_u32));

        let mut s = Serializer::new(1024);
        assert_eq!(s.start_serialize(), Ok(()));

        pairpos_table
            .subset(&plan, &mut s, (&subset_state, &font, &plan.gpos_lookups))
            .unwrap();
        assert!(!s.in_error());
        s.end_serialize();

        let subsetted_data = s.copy_bytes();
        let expected_data: [u8; 34] = [
            0x00, 0x01, 0x00, 0x0e, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02, 0x00, 0x1c, 0x00, 0x16,
            0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0x00, 0x01, 0x00, 0x03, 0xff, 0xcf,
            0x00, 0x01, 0x00, 0x04, 0xff, 0xe8,
        ];

        assert_eq!(subsetted_data, expected_data);
    }

    #[test]
    fn test_subset_pairpos_format2() {
        use read_fonts::tables::gpos::PositionSubtables;

        let font = FontRef::new(include_bytes!("../../test-data/fonts/Amiri-Regular.ttf")).unwrap();
        let gpos_lookups = font.gpos().unwrap().lookup_list().unwrap();
        let lookup = gpos_lookups.lookups().get(82).unwrap();

        let PositionSubtables::Pair(sub_tables) = lookup.subtables().unwrap() else {
            panic!("Wrong type of lookup table!");
        };
        let pairpos_table = sub_tables.get(0).unwrap();

        let subset_state = SubsetState::default();
        let mut plan = Plan {
            glyph_map_gsub: vec![crate::INVALID_GID; 6737],
            ..Default::default()
        };

        //test case 1: ValueFormat remains the same
        plan.glyph_map_gsub[40] = GlyphId::from(1_u32);
        plan.glyph_map_gsub[72] = GlyphId::from(2_u32);
        plan.glyph_map_gsub[168] = GlyphId::from(3_u32);
        plan.glyph_map_gsub[6736] = GlyphId::from(4_u32);

        plan.glyphset_gsub.insert(GlyphId::from(40_u32));
        plan.glyphset_gsub.insert(GlyphId::from(72_u32));
        plan.glyphset_gsub.insert(GlyphId::from(168_u32));
        plan.glyphset_gsub.insert(GlyphId::from(6736_u32));

        let mut s = Serializer::new(1024);
        assert_eq!(s.start_serialize(), Ok(()));

        pairpos_table
            .subset(&plan, &mut s, (&subset_state, &font, &plan.gpos_lookups))
            .unwrap();
        assert!(!s.in_error());
        s.end_serialize();

        let subsetted_data = s.copy_bytes();
        let expected_data: [u8; 82] = [
            0x00, 0x02, 0x00, 0x2e, 0x00, 0x04, 0x00, 0x00, 0x00, 0x46, 0x00, 0x38, 0x00, 0x03,
            0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0xff, 0xf2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x01, 0x00, 0x04, 0x00, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x04, 0x00, 0x03,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x03, 0x00, 0x02, 0x00, 0x01, 0x00, 0x01,
        ];

        assert_eq!(subsetted_data, expected_data);

        // test case 2: strip hints is enabled
        plan.subset_flags = SubsetFlags::SUBSET_FLAGS_NO_HINTING;
        plan.glyph_map_gsub.resize(6737, crate::INVALID_GID);
        plan.glyph_map_gsub[72] = GlyphId::from(1_u32);
        plan.glyph_map_gsub[168] = GlyphId::from(2_u32);
        plan.glyph_map_gsub[6736] = GlyphId::from(3_u32);

        plan.glyphset_gsub.clear();
        plan.glyphset_gsub.insert(GlyphId::from(72_u32));
        plan.glyphset_gsub.insert(GlyphId::from(168_u32));
        plan.glyphset_gsub.insert(GlyphId::from(6736_u32));

        let mut s = Serializer::new(1024);
        assert_eq!(s.start_serialize(), Ok(()));

        pairpos_table
            .subset(&plan, &mut s, (&subset_state, &font, &plan.gpos_lookups))
            .unwrap();
        assert!(!s.in_error());
        s.end_serialize();

        let subsetted_data = s.copy_bytes();
        let expected_data: [u8; 48] = [
            0x00, 0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x26, 0x00, 0x1a, 0x00, 0x02,
            0x00, 0x04, 0x00, 0x01, 0x00, 0x03, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x01,
            0x00, 0x01, 0x00, 0x03, 0x00, 0x01, 0x00, 0x03, 0x00, 0x02, 0x00, 0x01, 0x00, 0x01,
            0x00, 0x02, 0x00, 0x01, 0x00, 0x01,
        ];

        assert_eq!(subsetted_data, expected_data);
    }

    #[test]
    fn test_collect_variation_indices_pairpos_format1() {
        use read_fonts::tables::gpos::PositionSubtables;

        let font = FontRef::new(include_bytes!(
            "../../test-data/fonts/RobotoFlex-Variable.ttf"
        ))
        .unwrap();
        let gpos_lookups = font.gpos().unwrap().lookup_list().unwrap();
        let lookup = gpos_lookups.lookups().get(0).unwrap();

        let PositionSubtables::Pair(sub_tables) = lookup.subtables().unwrap() else {
            panic!("Wrong type of lookup table!");
        };
        let pairpos_table = sub_tables.get(0).unwrap();
        let mut plan = Plan::default();

        plan.glyphset_gsub.insert(GlyphId::from(3_u32));
        plan.glyphset_gsub.insert(GlyphId::from(11_u32));
        plan.glyphset_gsub.insert(GlyphId::from(55_u32));
        plan.glyphset_gsub.insert(GlyphId::from(57_u32));

        let mut varidx_set = IntSet::empty();
        pairpos_table.collect_variation_indices(&plan, &mut varidx_set);
        assert_eq!(varidx_set.len(), 5);
        assert!(varidx_set.contains(0x6f0013_u32));
        assert!(varidx_set.contains(0x3e0004_u32));
        assert!(varidx_set.contains(0x540010_u32));
        assert!(varidx_set.contains(0x1c0024_u32));
        assert!(varidx_set.contains(0x1c003c_u32));
    }

    #[test]
    fn test_collect_variation_indices_pairpos_format2() {
        use read_fonts::tables::gpos::PositionSubtables;

        let font = FontRef::new(include_bytes!(
            "../../test-data/fonts/RobotoFlex-Variable.ttf"
        ))
        .unwrap();
        let gpos_lookups = font.gpos().unwrap().lookup_list().unwrap();
        let lookup = gpos_lookups.lookups().get(0).unwrap();

        let PositionSubtables::Pair(sub_tables) = lookup.subtables().unwrap() else {
            panic!("Wrong type of lookup table!");
        };
        let pairpos_table = sub_tables.get(1).unwrap();
        let mut plan = Plan::default();

        plan.glyphset_gsub.insert(GlyphId::from(38_u32));
        plan.glyphset_gsub.insert(GlyphId::from(39_u32));
        plan.glyphset_gsub.insert(GlyphId::from(68_u32));
        plan.glyphset_gsub.insert(GlyphId::from(127_u32));

        let mut varidx_set = IntSet::empty();
        pairpos_table.collect_variation_indices(&plan, &mut varidx_set);
        assert_eq!(varidx_set.len(), 9);
        assert!(varidx_set.contains(0x12000f_u32));
        assert!(varidx_set.contains(0x3c0000_u32));
        assert!(varidx_set.contains(0x54001e_u32));
        assert!(varidx_set.contains(0x1c0031_u32));
        assert!(varidx_set.contains(0xb000b_u32));
        assert!(varidx_set.contains(0x1c0035_u32));
        assert!(varidx_set.contains(0x1c0022_u32));
        assert!(varidx_set.contains(0x100005_u32));
        assert!(varidx_set.contains(0x1c0036_u32));
    }

    /// A PairPosFormat2 whose ClassDefs assign classes outside the declared
    /// class1Count x class2Count matrix.
    ///
    /// intersect_classes reports the classes a ClassDef actually assigns, and
    /// nothing ties those to the counts in the PairPos header, so a malformed
    /// font can produce a record index past the end of the matrix. Harfbuzz
    /// reads the records out of an array sized to the counts, and its sub_array
    /// clamps, so those contribute nothing.
    #[test]
    fn test_collect_variation_indices_pairpos_format2_class_past_matrix() {
        use read_fonts::{FontData, FontRead};

        // class1Count is 1 and class2Count is 2, so only records [0][0] and
        // [0][1] exist. Everything after them is filler pointing at a
        // VariationIndex that the matrix itself never references.
        #[rustfmt::skip]
        let raw_table: [u8; 92] = [
            // posFormat=2, coverageOffset=58
            0x00, 0x02, 0x00, 0x3a,
            // valueFormat1=X_ADVANCE_DEVICE, valueFormat2=0
            0x00, 0x40, 0x00, 0x00,
            // classDef1Offset=66, classDef2Offset=76
            0x00, 0x42, 0x00, 0x4c,
            // class1Count=1, class2Count=2
            0x00, 0x01, 0x00, 0x02,
            // @16 the 1x2 record matrix: device offsets 40 and 46
            0x00, 0x28, 0x00, 0x2e,
            // @20 past the matrix: ten slots, every one pointing at 52
            0x00, 0x34, 0x00, 0x34, 0x00, 0x34, 0x00, 0x34, 0x00, 0x34,
            0x00, 0x34, 0x00, 0x34, 0x00, 0x34, 0x00, 0x34, 0x00, 0x34,
            // @40 VariationIndex (1, 2)
            0x00, 0x01, 0x00, 0x02, 0x80, 0x00,
            // @46 VariationIndex (3, 4)
            0x00, 0x03, 0x00, 0x04, 0x80, 0x00,
            // @52 VariationIndex (11, 12), only reachable from past the matrix
            0x00, 0x0b, 0x00, 0x0c, 0x80, 0x00,
            // @58 coverage: format 1, glyphs 10 and 20
            0x00, 0x01, 0x00, 0x02, 0x00, 0x0a, 0x00, 0x14,
            // @66 classDef1: format 2, glyph 20 -> class 3, past class1Count
            0x00, 0x02, 0x00, 0x01, 0x00, 0x14, 0x00, 0x14, 0x00, 0x03,
            // @76 classDef2: format 2, glyph 30 -> class 1, glyph 40 -> class 5
            0x00, 0x02, 0x00, 0x02,
            0x00, 0x1e, 0x00, 0x1e, 0x00, 0x01,
            0x00, 0x28, 0x00, 0x28, 0x00, 0x05,
        ];

        let pairpos = PairPosFormat2::read(FontData::new(&raw_table)).unwrap();

        let mut plan = Plan::default();
        for g in [10_u32, 20, 30, 40] {
            plan.glyphset_gsub.insert(GlyphId::from(g));
        }

        let mut varidx_set = IntSet::empty();
        pairpos.collect_variation_indices(&plan, &mut varidx_set);

        // class1 3 and class2 5 both land outside the matrix, so only the two
        // real records contribute. 0x000b000c would mean a read past the end.
        assert_eq!(
            varidx_set.iter().collect::<Vec<_>>(),
            vec![0x00010002, 0x00030004]
        );
    }
}
